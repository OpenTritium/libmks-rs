use super::{
    capture_state::{Capture, CaptureState},
    coordinate::Coordinate,
    input_handler::InputHandler,
    screen::{DirtyFlags, Screen},
    wayland_confine::WaylandConfine,
};
use crate::{
    dbus::{console::ConsoleController, keyboard::PressAction, listener::Event as QemuEvent},
    display::input_daemon::InputCommand,
    mks_debug, mks_error, mks_info, mks_trace, mks_warn,
};
use gdk4_wayland::{
    WaylandDisplay, WaylandSurface,
    gdk::{Key, ModifierType, Rectangle, Texture},
    glib::{ControlFlow, IOCondition, Propagation, SourceId, translate::IntoGlib, unix_fd_add_local},
    prelude::*,
    wayland_client::protocol::wl_surface::WlSurface,
};
use kanal::AsyncReceiver;
use relm4::{
    Component, ComponentParts, ComponentSender,
    gtk::{
        Align, ContentFit, CssProvider, DrawingArea, EventController, EventControllerKey, EventControllerMotion,
        EventControllerScroll, EventControllerScrollFlags, Fixed, GestureClick, Label, Overlay, Picture,
        STYLE_PROVIDER_PRIORITY_APPLICATION, accelerator_get_label, gdk::Display, graphene::Point, gsk::Transform,
        prelude::*, style_context_add_provider_for_display,
    },
};
use std::{borrow::Cow, cell::RefCell, fmt, mem, num::NonZeroU32, rc::Rc, sync::Once, time::Duration};
use tokio::{task::AbortHandle, time::sleep};

const LOG_TARGET: &str = "mks.display.vm";
const INCH_TO_MM: f32 = 25.4;
const DEFAULT_DPI: f32 = 96.;
const DEFAULT_PIXEL_PITCH_MM: f32 = INCH_TO_MM / DEFAULT_DPI;
const TOAST_DURATION_SECS: u64 = 3;
const RELATIVE_SEAMLESS_UNSUPPORTED_TOAST: &str =
    "Relative mouse mode does not support seamless capture. Switch to confined mode manually.";
const CONFINED_CAPTURE_UNAVAILABLE_TOAST: &str =
    "Confined capture is unavailable in the current environment. Falling back to seamless mode.";
const RELATIVE_CONFINED_UNSUPPORTED_TOAST: &str =
    "Relative capture requires Wayland relative-pointer protocol, which is unavailable in this environment.";

/// Loads the component CSS once per process.
///
/// `CssProvider` is not `Sync`, so we guard one-time initialization with `Once`.
#[inline]
fn ensure_css_loaded() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let provider = CssProvider::new();
        provider.load_from_string(include_str!("vm-display.css"));
        if let Some(display) = Display::default() {
            style_context_add_provider_for_display(&display, &provider, STYLE_PROVIDER_PRIORITY_APPLICATION);
        }
    });
}

/// Keyboard shortcut used to release pointer capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrabShortcut {
    pub mask: ModifierType,
    pub key: Key,
}

impl Default for GrabShortcut {
    fn default() -> Self { Self { mask: ModifierType::CONTROL_MASK | ModifierType::ALT_MASK, key: Key::g } }
}

impl fmt::Display for GrabShortcut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = accelerator_get_label(self.key, self.mask);
        write!(f, "{label}")
    }
}

pub struct ConfineState {
    pub wayland_confine: Rc<RefCell<WaylandConfine>>,
    pub poll_source: Option<SourceId>,
    pub is_captured: bool,
}

impl ConfineState {
    pub fn connect_to_wayland(input_tx: kanal::Sender<InputCommand>) -> Option<Self> {
        let display = Display::default()?;
        let wl_display = display.downcast::<WaylandDisplay>().ok()?;
        mks_info!("Wayland session detected; enabling pointer-confinement support");
        let confine = WaylandConfine::from_gdk(&wl_display, input_tx);
        let confine = Rc::new(RefCell::new(confine));
        let fd = confine.borrow().get_conn_raw_fd();
        let confine_clone = confine.clone();
        let poll_source = unix_fd_add_local(fd, IOCondition::IN, move |_fd, _condition| {
            confine_clone.borrow().dispatch_pending();
            ControlFlow::Continue
        });
        mks_debug!("Attached Wayland FD monitor to GLib main context");
        Some(Self { wayland_confine: confine, poll_source: Some(poll_source), is_captured: false })
    }
}

impl Drop for ConfineState {
    fn drop(&mut self) {
        if let Some(source) = self.poll_source.take() {
            source.remove();
        }
        self.wayland_confine.borrow_mut().unconfine();
    }
}

/// Pointer capture policy for VM input forwarding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    /// Strict capture: pointer cannot leave the VM view while captured.
    Confined,
    /// Seamless capture: follow viewport enter/leave state automatically.
    Seamless,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalingMode {
    /// Resize guest resolution to follow host window size.
    ResizeGuest,
    /// Keep guest resolution fixed; scale presentation only.
    FixedGuest,
}

#[derive(Debug)]
pub enum CaptureEvent {
    Capture { click_pos: Option<(f32, f32)> },
    Release,
}

impl CaptureEvent {
    #[inline]
    const fn should_capture(&self) -> bool {
        match self {
            CaptureEvent::Capture { .. } => true,
            CaptureEvent::Release => false,
        }
    }

    #[inline]
    /// Returns click position as `(x, y)` in widget logical coordinates.
    ///
    /// - `x`: click X coordinate in the widget.
    /// - `y`: click Y coordinate in the widget.
    const fn click_pos(&self) -> Option<(f32, f32)> {
        match self {
            CaptureEvent::Capture { click_pos } => *click_pos,
            CaptureEvent::Release => None,
        }
    }
}

#[derive(Debug)]
pub enum Message {
    Qemu(QemuEvent),
    CanvasResize { logical_width: f32, logical_height: f32 },
    MouseMove { x: f32, y: f32 },
    MouseButton { button: u32, transition: PressAction },
    Scroll { dy: f64 },
    Key { keyval: u32, keycode: u32, transition: PressAction },
    UpdateMonitorInfo { pixel_pitch_mm: f32 },
    SetScalingMode(ScalingMode),
    SetConfined(CaptureEvent),
    HideCaptureHint,
    UpdateCaptureView, // View-only refresh for capture visuals (for example host cursor visibility).
    MouseLeave,
    SetInputCaptureMode(InputMode),
    MouseModeChanged { is_absolute: bool },
}

pub struct VmDisplayModel {
    pub screen: Screen,
    pub dirty_flags: DirtyFlags,
    console_ctrl: ConsoleController,
    pixel_pitch_mm: f32,
    resize_timer: Option<AbortHandle>,
    input_overlay: DrawingArea,
    pub confine_state: Option<ConfineState>,
    pub scaling_mode: ScalingMode,
    grab_shortcut: GrabShortcut,
    hint_visible: bool,
    hint_text: Cow<'static, str>,
    hint_timer: Option<AbortHandle>,
    coord_system: Coordinate,
    input: InputHandler,
    capture_state: CaptureState,
    requested_input_mode: InputMode,
    last_logged_presentation_y_flip: Option<bool>,
}

pub struct VmDisplayWidgets {
    pub view_stack: Overlay,
    pub vm_fixed: Fixed,
    pub vm_picture: Picture,
    pub input_overlay: DrawingArea,
    pub cursor_fixed: Fixed,
    pub cursor_picture: Picture,
    pub controllers: Box<[EventController]>,
    pub capture_hint: Label,
}

pub struct VmDisplayInit {
    pub rx: AsyncReceiver<QemuEvent>,
    pub console_ctrl: ConsoleController,
    pub input_handler: InputHandler,
    pub grab_shortcut: GrabShortcut,
}

impl VmDisplayModel {
    #[inline]
    const fn input_mode(&self) -> InputMode {
        if self.confine_state.is_some() {
            InputMode::Confined
        } else {
            InputMode::Seamless
        }
    }

    #[inline]
    fn reset_resize_timer(&mut self, w: NonZeroU32, h: NonZeroU32) {
        if let Some(handle) = self.resize_timer.take() {
            handle.abort();
        }
        let pixel_pitch_mm = self.pixel_pitch_mm;
        let console = self.console_ctrl.clone();
        let w = w.get();
        let h = h.get();
        self.resize_timer = Some(
            relm4::spawn(async move {
                sleep(Duration::from_millis(200)).await;
                let w_mm = (w as f32 * pixel_pitch_mm) as u16;
                let h_mm = (h as f32 * pixel_pitch_mm) as u16;
                mks_info!("Sending debounced guest resize: {w}x{h} ({w_mm}mm x {h_mm}mm)");
                if let Err(e) = console.set_ui_info(w_mm, h_mm, 0, 0, w, h) {
                    mks_error!(error:? = e; "Failed to send debounced guest resize update");
                }
            })
            .abort_handle(),
        );
    }

    #[inline]
    fn cancel_hint_timer(&mut self) {
        if let Some(handle) = self.hint_timer.take() {
            handle.abort();
        }
    }

    #[inline]
    fn release_hint_text(shortcut: GrabShortcut) -> String { format!("Press {shortcut} to release mouse") }

    #[inline]
    fn mark_cursor_dirty(&mut self) { self.dirty_flags.cursor = true; }

    #[inline]
    fn mark_frame_and_cursor_dirty(&mut self) {
        self.dirty_flags.frame = true;
        self.dirty_flags.cursor = true;
    }

    #[inline]
    fn merge_dirty_flags(&mut self, flags: DirtyFlags) {
        self.dirty_flags.frame |= flags.frame;
        self.dirty_flags.cursor |= flags.cursor;
    }

    #[inline]
    fn confined_widget_rect(&self) -> Rectangle {
        let native = self.input_overlay.native();
        if let Some(native) = &native
            && let Some(bounds) = self.input_overlay.compute_bounds(native)
        {
            Rectangle::new(
                bounds.x().floor() as i32,
                bounds.y().floor() as i32,
                bounds.width().ceil() as i32,
                bounds.height().ceil() as i32,
            )
        } else {
            Rectangle::new(0, 0, 0, 0)
        }
    }

    #[inline]
    fn current_wayland_surface(&self) -> Option<WlSurface> {
        self.input_overlay
            .native()
            .and_then(|n| n.surface())
            .and_then(|s| s.downcast::<WaylandSurface>().ok())
            .and_then(|ws| ws.wl_surface())
    }

    #[inline]
    fn show_confined_capture_unavailable_toast(&mut self, prefer_relative: bool, sender: &ComponentSender<Self>) {
        if prefer_relative {
            self.show_toast(RELATIVE_CONFINED_UNSUPPORTED_TOAST, sender.clone());
        } else {
            self.show_toast(CONFINED_CAPTURE_UNAVAILABLE_TOAST, sender.clone());
        }
    }

    fn render_view(&self, widgets: &mut VmDisplayWidgets, dirty_flags: DirtyFlags) {
        let (class_add, class_remove) = if self.hint_visible {
            ("toast-visible", "toast-hidden")
        } else {
            ("toast-hidden", "toast-visible")
        };
        widgets.capture_hint.set_label(self.hint_text.as_ref());
        widgets.capture_hint.add_css_class(class_add);
        widgets.capture_hint.remove_css_class(class_remove);
        let is_interactive = self.capture_state.should_forward();
        widgets.input_overlay.set_cursor_from_name(is_interactive.then_some("none"));
        if !dirty_flags.any() {
            return;
        }
        if dirty_flags.frame {
            if let Some((offset_x, offset_y, viewport_w, viewport_h)) = self.coord_system.vm_display_bounds() {
                let req_w = viewport_w.ceil().max(1.) as i32;
                let req_h = viewport_h.ceil().max(1.) as i32;
                if widgets.vm_picture.width_request() != req_w || widgets.vm_picture.height_request() != req_h {
                    widgets.vm_picture.set_size_request(req_w, req_h);
                }
                let matrix = if self.screen.y0_top {
                    Transform::new().translate(&Point::new(offset_x, offset_y + viewport_h)).scale(1., -1.)
                } else {
                    Transform::new().translate(&Point::new(offset_x, offset_y))
                };
                widgets.vm_fixed.set_child_transform(&widgets.vm_picture, Some(&matrix));
            } else {
                widgets.vm_fixed.set_child_transform(&widgets.vm_picture, None);
            }
            let texture = self.screen.get_background_texture();
            if let Some(texture) = texture {
                let width = texture.width();
                let height = texture.height();
                mks_trace!("Frame texture presented: {width}x{height}, y0_top={}", self.screen.y0_top);
                widgets.vm_picture.set_paintable(Some(texture));
            } else {
                mks_trace!("Frame texture cleared");
                widgets.vm_picture.set_paintable(None::<&Texture>);
            }
        }
        if dirty_flags.cursor || dirty_flags.frame {
            let cursor = &self.screen.cursor;
            let visible = cursor.visible && is_interactive;
            widgets.cursor_picture.set_visible(visible);
            if visible {
                if let Some(texture) = &cursor.texture {
                    widgets.cursor_picture.set_paintable(Some(texture));
                    let tex_w = texture.width();
                    let tex_h = texture.height();
                    // Only update size request when dimensions actually change to avoid GTK layout thrashing
                    if widgets.cursor_picture.width_request() != tex_w
                        || widgets.cursor_picture.height_request() != tex_h
                    {
                        widgets.cursor_picture.set_size_request(tex_w, tex_h);
                    }
                    if let Some(transform) = self.coord_system.get_cached_viewport() {
                        let logical_scale = transform.scale;
                        let (logical_offset_x, logical_offset_y) = (transform.offset_x, transform.offset_y);
                        // Intentionally align by cursor image top-left, not hotspot.
                        let top_left_guest_x = cursor.x;
                        let top_left_guest_y = cursor.y;
                        let anchor_x = logical_offset_x + top_left_guest_x as f32 * logical_scale;
                        let anchor_y = logical_offset_y + top_left_guest_y as f32 * logical_scale;
                        let draw_x = anchor_x.round();
                        let draw_y = anchor_y.round();
                        let transform_matrix =
                            Transform::new().translate(&Point::new(draw_x, draw_y)).scale(logical_scale, logical_scale);
                        widgets.cursor_fixed.set_child_transform(&widgets.cursor_picture, Some(&transform_matrix));
                    }
                } else {
                    widgets.cursor_picture.set_paintable(None::<&Texture>);
                }
            }
        }
    }

    fn show_toast(&mut self, text: impl Into<Cow<'static, str>>, sender: ComponentSender<Self>) {
        self.hint_text = text.into();
        self.hint_visible = true;
        self.mark_cursor_dirty();
        self.cancel_hint_timer();
        self.hint_timer = Some(
            relm4::spawn(async move {
                sleep(Duration::from_secs(TOAST_DURATION_SECS)).await;
                sender.input(Message::HideCaptureHint);
            })
            .abort_handle(),
        );
    }
}

impl Component for VmDisplayModel {
    type CommandOutput = ();
    type Init = VmDisplayInit;
    type Input = Message;
    type Output = ();
    type Root = Overlay;
    type Widgets = VmDisplayWidgets;

    #[inline]
    fn init_root() -> Self::Root {
        Overlay::builder().hexpand(true).vexpand(true).focusable(true).can_focus(true).build()
    }

    fn init(init: Self::Init, root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        use Message::*;
        ensure_css_loaded();
        let grab_shortcut = init.grab_shortcut;
        let default_hint_text = Self::release_hint_text(grab_shortcut);
        let input_plane =
            DrawingArea::builder().focusable(true).focus_on_click(true).hexpand(true).vexpand(true).build();
        input_plane.set_content_width(0);
        input_plane.set_content_height(0);
        let model = VmDisplayModel {
            screen: Screen::new(),
            dirty_flags: DirtyFlags::default(),
            console_ctrl: init.console_ctrl,
            scaling_mode: ScalingMode::ResizeGuest,
            pixel_pitch_mm: DEFAULT_PIXEL_PITCH_MM,
            resize_timer: None,
            grab_shortcut,
            hint_visible: false,
            hint_text: Cow::Owned(default_hint_text.clone()),
            hint_timer: None,
            input_overlay: input_plane.clone(),
            confine_state: None,
            coord_system: Coordinate::new(0, 0, 0., 0., 1.),
            input: init.input_handler,
            capture_state: CaptureState::new(),
            requested_input_mode: InputMode::Seamless,
            last_logged_presentation_y_flip: None,
        };
        let view_stack = Overlay::builder().hexpand(true).vexpand(true).css_classes(["vm-display-bg"]).build();
        let vm_picture = Picture::builder()
            .can_shrink(true)
            .content_fit(ContentFit::Contain)
            .halign(Align::Center)
            .valign(Align::Center)
            .can_target(false)
            .build();
        let vm_fixed = Fixed::builder().hexpand(true).vexpand(true).can_target(false).build();
        vm_fixed.put(&vm_picture, 0., 0.);
        let sender_clone = sender.clone();
        let update_monitor_info = move |widget: &DrawingArea| {
            let display = widget.display();
            let Some(native) = widget.native() else {
                mks_error!("Input overlay has no native widget; skipping monitor metrics update");
                return;
            };
            let Some(surface) = native.surface() else {
                mks_error!("Native widget has no surface; skipping monitor metrics update");
                return;
            };
            let Some(monitor) = display.monitor_at_surface(&surface) else {
                mks_error!("No monitor found for surface; skipping monitor metrics update");
                return;
            };
            let geometry = monitor.geometry();
            let width_mm = monitor.width_mm() as f32;
            let height_mm = monitor.height_mm() as f32;
            let scale_factor = (widget.scale_factor() as f32).max(0.5).clamp(0.5, 8.);
            let geometry_width_physical = geometry.width() as f32 * scale_factor;
            let geometry_height_physical = geometry.height() as f32 * scale_factor;

            if width_mm > 0. && height_mm > 0. && geometry_width_physical > 0. && geometry_height_physical > 0. {
                let pixel_pitch_mm = width_mm / geometry_width_physical;
                mks_debug!(
                    "Monitor {}: {}×{}mm, {}×{} logical px (scale={:.2}) → {}×{} physical px (pitch={:.4}mm/px)",
                    monitor.model().as_deref().unwrap_or("unknown"),
                    width_mm,
                    height_mm,
                    geometry.width(),
                    geometry.height(),
                    scale_factor,
                    geometry_width_physical,
                    geometry_height_physical,
                    pixel_pitch_mm
                );
                sender_clone.input(UpdateMonitorInfo { pixel_pitch_mm });
            }
        };
        let updater = update_monitor_info.clone();
        input_plane.connect_realize(move |widget| updater(widget));

        let mut controllers = Vec::new();
        let motion_ctrl = EventControllerMotion::new();
        let sender_clone = sender.clone();
        motion_ctrl.connect_motion(move |_, x, y| {
            sender_clone.input(MouseMove { x: x as f32, y: y as f32 });
        });
        let sender_c = sender.clone();
        motion_ctrl.connect_leave(move |_| sender_c.input(MouseLeave));
        input_plane.add_controller(motion_ctrl.clone());
        controllers.push(motion_ctrl.upcast());

        let click = GestureClick::new();
        click.set_button(0);
        let sender_clone = sender.clone();
        let input_plane_click = input_plane.clone();
        click.connect_pressed(move |gesture, _, x, y| {
            input_plane_click.grab_focus();
            sender_clone.input(Message::SetConfined(CaptureEvent::Capture { click_pos: Some((x as f32, y as f32)) }));
            sender_clone.input(MouseButton { button: gesture.current_button(), transition: PressAction::Press });
        });
        let sender_clone = sender.clone();
        click.connect_released(move |gesture, _, _, _| {
            sender_clone.input(MouseButton { button: gesture.current_button(), transition: PressAction::Release });
        });
        input_plane.add_controller(click.clone());
        controllers.push(click.upcast());

        let scroll = EventControllerScroll::new(EventControllerScrollFlags::VERTICAL);
        let sender_clone = sender.clone();
        scroll.connect_scroll(move |_, _dx, dy| {
            sender_clone.input(Scroll { dy });
            Propagation::Proceed
        });
        input_plane.add_controller(scroll.clone());
        controllers.push(scroll.upcast());

        let key = EventControllerKey::new();
        let sender_for_release = sender.clone();
        let sender_for_key = sender.clone();
        key.connect_key_pressed(move |_, keyval, keycode, modifiers| {
            if modifiers.contains(grab_shortcut.mask) && keyval == grab_shortcut.key {
                sender_for_release.input(Message::SetConfined(CaptureEvent::Release));
                return Propagation::Stop;
            }
            let keyval_raw: u32 = keyval.into_glib();
            sender_for_key.input(Key { keyval: keyval_raw, keycode, transition: PressAction::Press });
            Propagation::Stop
        });
        let sender_clone = sender.clone();
        key.connect_key_released(move |_, keyval, keycode, _| {
            let keyval_raw: u32 = keyval.into_glib();
            sender_clone.input(Key { keyval: keyval_raw, keycode, transition: PressAction::Release });
        });
        root.add_controller(key.clone());
        controllers.push(key.upcast());

        let capture_hint = Label::builder()
            .label(&default_hint_text)
            .halign(Align::Center)
            .valign(Align::Start)
            .margin_top(20)
            .css_classes(["toast-label", "toast-hidden"])
            .selectable(false)
            .can_target(false)
            .build();

        let cursor_picture = Picture::builder()
            .can_target(false)
            .halign(Align::Start)
            .valign(Align::Start)
            .css_classes(["cursor-layer"])
            .build();

        let cursor_fixed = Fixed::builder().can_target(false).hexpand(true).vexpand(true).build();
        cursor_fixed.put(&cursor_picture, 0., 0.);

        let resize_handler = {
            let updater = update_monitor_info.clone();
            let sender = sender.clone();
            move |widget: &DrawingArea| {
                updater(widget);
                let w = widget.width() as f32;
                let h = widget.height() as f32;
                if w > 0. && h > 0. {
                    sender.input(Message::CanvasResize { logical_width: w, logical_height: h });
                } else {
                    mks_error!("Ignoring canvas resize with invalid dimensions: ({w}, {h})");
                }
            }
        };
        let handler_clone = resize_handler.clone();
        input_plane.connect_resize(move |widget, _, _| handler_clone(widget));
        let handler_clone = resize_handler.clone();
        input_plane.connect_scale_factor_notify(move |widget| handler_clone(widget));

        // Keep a 0x0 minimum from the input plane while rendering layers float above it.
        view_stack.set_child(Some(&input_plane));
        view_stack.add_overlay(&vm_fixed);
        view_stack.set_measure_overlay(&vm_fixed, false);
        view_stack.add_overlay(&cursor_fixed);
        view_stack.set_measure_overlay(&cursor_fixed, false);
        view_stack.add_overlay(&capture_hint);
        view_stack.set_measure_overlay(&capture_hint, false);
        root.set_child(Some(&view_stack));

        relm4::spawn(async move {
            while let Ok(event) = init.rx.recv().await {
                sender.input(Qemu(event));
            }
            mks_error!("VM display event channel closed; forcing display disable state");
            sender.input(Qemu(QemuEvent::Disable));
        });
        let controllers = controllers.into_boxed_slice();
        let widgets = VmDisplayWidgets {
            view_stack,
            vm_fixed,
            vm_picture,
            input_overlay: input_plane,
            cursor_fixed,
            cursor_picture,
            controllers,
            capture_hint,
        };
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        use Message::*;
        match msg {
            SetInputCaptureMode(mode) => {
                self.requested_input_mode = mode;
                if mode == InputMode::Seamless && !self.input.is_absolute {
                    mks_warn!("Seamless capture requires absolute guest mouse mode; ignoring request");
                    self.capture_state.reset();
                    let _ = self.confine_state.take();
                    self.cancel_hint_timer();
                    self.hint_visible = false;
                    self.show_toast(RELATIVE_SEAMLESS_UNSUPPORTED_TOAST, sender.clone());
                    sender.input(UpdateCaptureView);
                    return;
                }
                if self.input_mode() == mode {
                    mks_debug!("Input capture mode already set to {mode:?}; ignoring duplicate request");
                    return;
                }
                self.capture_state.reset();
                if self.confine_state.take().is_some() {
                    self.cancel_hint_timer();
                    self.hint_visible = false;
                } else {
                    if !self.input.capability.mouse {
                        mks_error!("Mouse capability unavailable; cannot enter confined mode (keeping seamless mode)");
                        return;
                    }
                    let Some(tx) = self.input.input_cmd_tx() else {
                        mks_error!(
                            "Input command channel unavailable; cannot enter confined mode (keeping seamless mode)"
                        );
                        return;
                    };
                    let Some(confine) = ConfineState::connect_to_wayland(tx.clone()) else {
                        mks_error!("Failed to connect to Wayland session; keeping seamless input mode");
                        self.show_toast(CONFINED_CAPTURE_UNAVAILABLE_TOAST, sender.clone());
                        return;
                    };
                    self.confine_state = Some(confine);
                }
                sender.input(UpdateCaptureView);
            }

            MouseLeave => {
                self.capture_state.on_mouse_leave();
                sender.input(UpdateCaptureView);
            }

            MouseMove { x, y } => {
                let mode = self.input_mode();
                if self.requested_input_mode == InputMode::Seamless
                    && mode == InputMode::Seamless
                    && !self.input.is_absolute
                {
                    let had_capture = self.capture_state.should_forward();
                    self.capture_state.reset();
                    if had_capture {
                        sender.input(UpdateCaptureView);
                    }
                    return;
                }
                let current_capture = self.capture_state.current();
                let point = Point::new(x, y);
                let is_in_viewport = self.coord_system.is_in_viewport(&point);
                match (mode, current_capture, is_in_viewport) {
                    // Pointer just entered VM viewport.
                    (InputMode::Seamless, Capture::Idle, true) => {
                        self.capture_state.on_mouse_enter(mode);
                        sender.input(UpdateCaptureView);
                    }
                    // Pointer just left VM viewport.
                    (InputMode::Seamless, Capture::Seamless, false) => {
                        self.capture_state.on_mouse_leave();
                        sender.input(UpdateCaptureView);
                    }
                    // Continuous in/out movement keeps current state unchanged.
                    _ => {}
                }
                if self.capture_state.should_forward() {
                    // Relative motion in confined mode is delivered only by native Wayland relative-pointer events.
                    if mode == InputMode::Confined && !self.input.is_absolute {
                        return;
                    }
                    self.input.move_mouse_to(x, y, &self.coord_system);
                }
            }

            SetConfined(event) => {
                let mode = self.input_mode();
                if mode != InputMode::Confined {
                    // Capture requests are meaningful only in confined mode.
                    return;
                }
                let should_capture = event.should_capture();
                let was_captured = self.capture_state.should_forward();

                if should_capture {
                    if was_captured {
                        return;
                    }
                    let widget_rect = self.confined_widget_rect();
                    let click_pos = event.click_pos();
                    let vm_coords = click_pos.and_then(|(x, y)| self.coord_system.widget_to_guest(x, y));
                    let Some(proxy) = self.current_wayland_surface() else {
                        mks_error!("Failed to resolve wl_surface proxy; cannot start confined capture");
                        sender.input(UpdateCaptureView);
                        return;
                    };
                    if self.confine_state.is_none() {
                        mks_error!("Confined state unavailable; cannot start pointer capture");
                        sender.input(UpdateCaptureView);
                        return;
                    }
                    let prefer_relative = !self.input.is_absolute;
                    let confine_ok = self.confine_state.as_ref().is_some_and(|confine| {
                        confine.wayland_confine.borrow_mut().confine_pointer(&proxy, &widget_rect, prefer_relative)
                    });
                    if !confine_ok {
                        mks_error!("Failed to establish Wayland pointer confinement");
                        self.cancel_hint_timer();
                        self.hint_visible = false;
                        self.show_confined_capture_unavailable_toast(prefer_relative, &sender);
                        if let Some(confine) = &mut self.confine_state {
                            confine.is_captured = false;
                        }
                        sender.input(UpdateCaptureView);
                        return;
                    }

                    self.hint_visible = true;
                    self.capture_state.on_click(mode);
                    self.show_toast(Self::release_hint_text(self.grab_shortcut), sender.clone());
                    if let Some(confine) = &mut self.confine_state {
                        confine.is_captured = true;
                    }
                    if self.input.is_absolute
                        && let Some((x, y)) = vm_coords
                    {
                        mks_debug!("Pointer confined; restoring latest absolute position: {x}, {y}");
                        self.input.set_abs_position(x, y);
                    }
                    mks_info!("Pointer confined to widget region: {widget_rect:?}");
                } else {
                    self.capture_state.on_release();
                    if let Some(confine) = &mut self.confine_state {
                        confine.wayland_confine.borrow_mut().unconfine();
                        confine.is_captured = false;
                    } else {
                        mks_error!("Confined state unavailable while stopping pointer capture");
                    }
                    self.cancel_hint_timer();
                    self.hint_visible = false;
                    mks_info!("Pointer confinement released");
                }
                sender.input(UpdateCaptureView);
            }

            Qemu(event) => match self.screen.handle_event(event) {
                Ok(flags) => {
                    let (w, h) = self.screen.resolution();
                    self.coord_system.set_vm_resolution(w, h);
                    self.merge_dirty_flags(flags);
                    let y_flip = self.screen.y0_top;
                    if self.last_logged_presentation_y_flip != Some(y_flip) {
                        let state = if y_flip { "enabled" } else { "disabled" };
                        mks_debug!("Presentation Y-flip is now {state}");
                        self.last_logged_presentation_y_flip = Some(y_flip);
                    }
                }
                Err(e) => {
                    mks_error!("Failed to process QEMU display event: {e}");
                }
            },

            SetScalingMode(mode) => {
                self.scaling_mode = mode;
                if mode == ScalingMode::ResizeGuest
                    && let Some((w_nz, h_nz)) = self.coord_system.physical_canvas_size()
                {
                    self.reset_resize_timer(w_nz, h_nz);
                }
            }

            CanvasResize { logical_width, logical_height } => {
                self.coord_system.set_widget_size(logical_width, logical_height);
                if let Some(_native) = self.input_overlay.native() {
                    self.coord_system.ui_scale = self.input_overlay.scale_factor() as f32;
                }
                self.mark_frame_and_cursor_dirty();
                if self.scaling_mode == ScalingMode::ResizeGuest
                    && let Some((w_nz, h_nz)) = self.coord_system.physical_canvas_size()
                {
                    self.reset_resize_timer(w_nz, h_nz);
                }
            }

            HideCaptureHint => {
                self.cancel_hint_timer();
                self.hint_visible = false;
                self.mark_frame_and_cursor_dirty();
            }

            UpdateCaptureView => {
                self.mark_cursor_dirty();
            }

            MouseButton { button, transition } => {
                if self.requested_input_mode == InputMode::Seamless
                    && self.input_mode() == InputMode::Seamless
                    && !self.input.is_absolute
                {
                    if transition == PressAction::Press {
                        mks_error!(
                            "Ignoring mouse click: seamless mode does not support relative guest mouse \
                             (button={button}, transition={transition})"
                        );
                        self.show_toast(RELATIVE_SEAMLESS_UNSUPPORTED_TOAST, sender.clone());
                    }
                    return;
                }
                if self.input_mode() == InputMode::Confined
                    && !self.input.is_absolute
                    && !self.capture_state.should_forward()
                {
                    if transition == PressAction::Press {
                        mks_error!("Ignoring mouse click: relative confined capture is not active");
                        self.show_toast(RELATIVE_CONFINED_UNSUPPORTED_TOAST, sender.clone());
                    }
                    return;
                }
                self.input.press_mouse_button(button, transition);
            }

            Scroll { dy } => {
                if !self.capture_state.should_forward() {
                    return;
                }
                let steps = self.input.cache_mouse_scroll(dy);
                if steps != 0 {
                    self.input.scroll_mouse(steps);
                }
            }

            Key { keyval: _, keycode, transition } => {
                if !self.capture_state.should_forward() {
                    return;
                }
                self.input.press_keyboard(keycode, transition);
            }

            UpdateMonitorInfo { pixel_pitch_mm } => {
                self.pixel_pitch_mm = pixel_pitch_mm;
            }

            MouseModeChanged { is_absolute } => {
                let mode_str = if is_absolute { "absolute" } else { "relative" };
                mks_info!("Guest mouse mode switched to {}", mode_str);
                self.input.set_mouse_mode(is_absolute);
                if self.input_mode() == InputMode::Confined && self.capture_state.should_forward() {
                    let widget_rect = self.confined_widget_rect();
                    let Some(proxy) = self.current_wayland_surface() else {
                        mks_error!("Failed to resolve wl_surface proxy; cannot reconfigure confined capture");
                        return;
                    };
                    let prefer_relative = !is_absolute;
                    let recapture_ok = self.confine_state.as_mut().is_some_and(|confine| {
                        confine.wayland_confine.borrow_mut().unconfine();
                        let ok =
                            confine.wayland_confine.borrow_mut().confine_pointer(&proxy, &widget_rect, prefer_relative);
                        confine.is_captured = ok;
                        ok
                    });
                    if recapture_ok {
                        mks_info!(
                            "Reconfigured confined pointer capture for {} guest mouse mode",
                            if prefer_relative { "relative" } else { "absolute" }
                        );
                    } else {
                        mks_error!(
                            "Failed to reconfigure confined pointer capture after mouse mode switch; releasing capture"
                        );
                        self.capture_state.on_release();
                        self.cancel_hint_timer();
                        self.hint_visible = false;
                        self.show_confined_capture_unavailable_toast(prefer_relative, &sender);
                        sender.input(UpdateCaptureView);
                    }
                }
                if !is_absolute
                    && self.requested_input_mode == InputMode::Seamless
                    && self.input_mode() == InputMode::Seamless
                {
                    let had_capture = self.capture_state.should_forward();
                    self.capture_state.reset();
                    mks_error!(
                        "Relative guest mouse mode is incompatible with seamless capture; capture released until mode \
                         changes"
                    );
                    self.show_toast(RELATIVE_SEAMLESS_UNSUPPORTED_TOAST, sender.clone());
                    if had_capture {
                        sender.input(UpdateCaptureView);
                    }
                }
            }
        }
    }

    fn update_with_view(
        &mut self, widgets: &mut Self::Widgets, message: Self::Input, sender: ComponentSender<Self>, root: &Self::Root,
    ) {
        let was_forwarding_input = self.capture_state.should_forward();
        self.update(message, sender, root);
        if was_forwarding_input && !self.capture_state.should_forward() {
            self.input.release_all_keys();
        }
        let dirty_flags = mem::take(&mut self.dirty_flags);
        self.render_view(widgets, dirty_flags);
    }

    fn update_view(&self, widgets: &mut Self::Widgets, _sender: ComponentSender<Self>) {
        self.render_view(widgets, self.dirty_flags);
    }

    fn shutdown(&mut self, _widgets: &mut Self::Widgets, _output: relm4::Sender<Self::Output>) {
        self.cancel_hint_timer();
        self.confine_state = None;
        if let Some(handle) = self.resize_timer.take() {
            handle.abort();
        }
    }
}
