use super::{
    capture_state::{Capture, CaptureState},
    coordinate::Coordinate,
    input_handler::{self, InputHandler},
    monitor_metrics,
    screen::{DirtyFlags, Screen},
    wayland_confine::ConfineState,
};
use crate::{
    dbus::{console::ConsoleController, keyboard::PressAction, listener::Event as QemuEvent},
    mks_debug, mks_error, mks_info, mks_trace, mks_warn,
};
use gdk4_wayland::{
    WaylandSurface,
    gdk::{Key, ModifierType, Texture},
    prelude::*,
    wayland_client::protocol::wl_surface::WlSurface,
};
use kanal::AsyncReceiver;
use relm4::{
    Component, ComponentParts, ComponentSender,
    adw::{self, Toast, ToastOverlay},
    gtk::{
        Align, ContentFit, CssProvider, DrawingArea, Fixed, Overlay, Picture, STYLE_PROVIDER_PRIORITY_APPLICATION,
        accelerator_get_label, gdk::Display, graphene::Point, gsk::Transform, prelude::*,
        style_context_add_provider_for_display,
    },
};
use std::{
    borrow::Cow,
    fmt, mem,
    num::{NonZeroU16, NonZeroU32},
    sync::Once,
    time::Duration,
};
use tokio::{task::AbortHandle, time::sleep};

const LOG_TARGET: &str = "mks.display.vm";
const INCH_TO_MM: f32 = 25.4;
const DEFAULT_DPI: f32 = 96.;
const DEFAULT_PIXEL_PITCH_MM: f32 = INCH_TO_MM / DEFAULT_DPI;
const TOAST_DURATION_SECS: u32 = 3;
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
    Key { keycode: u32, transition: PressAction },
    UpdateMonitorInfo { pixel_pitch_mm: f32 },
    SetScalingMode(ScalingMode),
    SetConfined(CaptureEvent),
    ShowToast(Cow<'static, str>),
    UpdateCaptureView, // View-only refresh for capture visuals (for example host cursor visibility).
    MouseLeave,
    SetInputCaptureMode(InputMode),
    MouseModeChanged { is_absolute: bool },
}

pub struct VmDisplayModel {
    pub screen: Screen,
    pub dirty_flags: DirtyFlags,
    pub confine_state: Option<ConfineState>,
    pub scaling_mode: ScalingMode,
    console_ctrl: ConsoleController,
    pixel_pitch_mm: f32,
    resize_timer: Option<AbortHandle>,
    input_overlay: DrawingArea,
    grab_shortcut: GrabShortcut,
    coord_system: Coordinate,
    input: InputHandler,
    capture_state: CaptureState,
    requested_input_mode: InputMode,
}

pub struct VmDisplayWidgets {
    pub toast_overlay: ToastOverlay,
    pub view_stack: Overlay,
    pub vm_fixed: Fixed,
    pub vm_picture: Picture,
    pub input_overlay: DrawingArea,
    pub cursor_fixed: Fixed,
    pub cursor_picture: Picture,
}

pub struct VmDisplayInit {
    pub rx: AsyncReceiver<QemuEvent>,
    pub console_ctrl: ConsoleController,
    pub input_handler: InputHandler,
    pub grab_shortcut: GrabShortcut,
}

impl VmDisplayModel {
    #[inline]
    const fn current_input_mode(&self) -> InputMode {
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
        let ppm = self.pixel_pitch_mm;
        let console = self.console_ctrl.clone();
        self.resize_timer = relm4::spawn(async move {
            sleep(Duration::from_millis(100)).await;
            let width_px = w.get();
            let height_px = h.get();
            let w_mm = NonZeroU16::new((width_px as f32 * ppm).round().clamp(1., u16::MAX as f32) as u16).unwrap();
            let h_mm = NonZeroU16::new((height_px as f32 * ppm).round().clamp(1., u16::MAX as f32) as u16).unwrap();
            mks_info!("Sending debounced guest resize: {width_px}x{height_px} ({w_mm}mm x {h_mm}mm)");
            if let Err(e) = console.set_ui_info(w_mm, h_mm, 0, 0, w, h) {
                mks_error!(error:? = e; "Failed to send debounced guest resize update");
            }
        })
        .abort_handle()
        .into();
    }

    #[inline]
    fn release_hint_text(shortcut: GrabShortcut) -> String { format!("Press {shortcut} to release mouse") }

    /// Returns the input overlay bounds for pointer confinement.
    ///
    /// - `x`: left edge in native-surface coordinates.
    /// - `y`: top edge in native-surface coordinates.
    /// - `width`: confined width in pixels.
    /// - `height`: confined height in pixels.
    #[inline]
    fn confined_widget_rect(&self) -> (u32, u32, u32, u32) {
        let native = self.input_overlay.native();
        if let Some(native) = &native
            && let Some(bounds) = self.input_overlay.compute_bounds(native)
        {
            let x = bounds.x().floor() as u32;
            let y = bounds.y().floor() as u32;
            let w = bounds.width().ceil() as u32;
            let h = bounds.height().ceil() as u32;
            (x, y, w, h)
        } else {
            (0, 0, 0, 0)
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
        let cursor = &self.screen.cursor;
        let cursor_visible = cursor.visible && is_interactive;
        widgets.cursor_picture.set_visible(cursor_visible);
        if !cursor_visible {
            return;
        }
        let Some(texture) = cursor.texture.as_ref() else {
            widgets.cursor_picture.set_paintable(None::<&Texture>);
            return;
        };
        widgets.cursor_picture.set_paintable(Some(texture));
        let tex_w = texture.width();
        let tex_h = texture.height();
        // Only update size request when dimensions actually change to avoid GTK layout thrashing
        if widgets.cursor_picture.width_request() != tex_w || widgets.cursor_picture.height_request() != tex_h {
            widgets.cursor_picture.set_size_request(tex_w, tex_h);
        }
        let Some(transform) = self.coord_system.get_cached_viewport() else {
            return;
        };
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

    fn show_toast(&self, text: impl Into<Cow<'static, str>>, sender: ComponentSender<Self>) {
        sender.input(Message::ShowToast(text.into()));
    }
}

impl Component for VmDisplayModel {
    type CommandOutput = ();
    type Init = VmDisplayInit;
    type Input = Message;
    type Output = ();
    type Root = adw::ToastOverlay;
    type Widgets = VmDisplayWidgets;

    #[inline]
    fn init_root() -> Self::Root { adw::ToastOverlay::new() }

    fn init(init: Self::Init, root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        use Message::*;
        ensure_css_loaded();
        let grab_shortcut = init.grab_shortcut;

        // Build widgets manually (view! macro has issues with nested overlays)
        let view_stack = Overlay::builder().hexpand(true).vexpand(true).css_classes(["vm-display-bg"]).build();

        let input_overlay =
            DrawingArea::builder().focusable(true).focus_on_click(true).hexpand(true).vexpand(true).build();
        input_overlay.set_content_width(0);
        input_overlay.set_content_height(0);

        let vm_picture = Picture::builder()
            .can_shrink(true)
            .content_fit(ContentFit::Contain)
            .halign(Align::Center)
            .valign(Align::Center)
            .can_target(false)
            .build();

        let vm_fixed = Fixed::builder().hexpand(true).vexpand(true).can_target(false).build();
        vm_fixed.put(&vm_picture, 0., 0.);

        let cursor_picture = Picture::builder()
            .can_target(false)
            .halign(Align::Start)
            .valign(Align::Start)
            .css_classes(["cursor-layer"])
            .build();

        let cursor_fixed = Fixed::builder().can_target(false).hexpand(true).vexpand(true).build();
        cursor_fixed.put(&cursor_picture, 0., 0.);

        // Build model
        let model = VmDisplayModel {
            screen: Screen::new(),
            dirty_flags: DirtyFlags::default(),
            console_ctrl: init.console_ctrl,
            scaling_mode: ScalingMode::ResizeGuest,
            pixel_pitch_mm: DEFAULT_PIXEL_PITCH_MM,
            resize_timer: None,
            grab_shortcut,
            input_overlay: input_overlay.clone(),
            confine_state: None,
            coord_system: Coordinate::new(0, 0, 0., 0., 1.),
            input: init.input_handler,
            capture_state: CaptureState::new(),
            requested_input_mode: InputMode::Seamless,
        };

        monitor_metrics::attach_resize_handlers(&input_overlay, &sender);

        input_handler::attach_gtk_controllers(&input_overlay, &root, &sender, grab_shortcut);

        // Set up overlays
        view_stack.set_child(Some(&input_overlay));
        view_stack.add_overlay(&vm_fixed);
        view_stack.set_measure_overlay(&vm_fixed, false);
        view_stack.add_overlay(&cursor_fixed);
        view_stack.set_measure_overlay(&cursor_fixed, false);
        root.set_child(Some(&view_stack));

        relm4::spawn(async move {
            while let Ok(event) = init.rx.recv().await {
                sender.input(Qemu(event));
            }
            mks_error!("VM display event channel closed; forcing display disable state");
            sender.input(Qemu(QemuEvent::Disable));
        });

        let widgets = VmDisplayWidgets {
            toast_overlay: root,
            view_stack,
            vm_fixed,
            vm_picture,
            input_overlay,
            cursor_fixed,
            cursor_picture,
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
                    self.capture_state.release();
                    self.confine_state = None;
                    self.show_toast(RELATIVE_SEAMLESS_UNSUPPORTED_TOAST, sender.clone());
                    sender.input(UpdateCaptureView);
                    return;
                }
                if self.current_input_mode() == mode {
                    mks_debug!("Input capture mode already set to {mode:?}; ignoring duplicate request");
                    return;
                }
                self.capture_state.release();
                if self.confine_state.take().is_some() {
                    sender.input(UpdateCaptureView);
                    return;
                }
                if !self.input.capability.mouse {
                    mks_error!("Mouse capability unavailable; cannot enter confined mode (keeping seamless mode)");
                    return;
                }
                let Some(tx) = self.input.input_cmd_tx() else {
                    mks_error!("Input command channel unavailable; cannot enter confined mode (keeping seamless mode)");
                    return;
                };
                let Some(confine) = ConfineState::connect_to_wayland(tx.clone()) else {
                    mks_error!("Failed to connect to Wayland session; keeping seamless input mode");
                    self.show_toast(CONFINED_CAPTURE_UNAVAILABLE_TOAST, sender.clone());
                    return;
                };
                self.confine_state = Some(confine);
                sender.input(UpdateCaptureView);
            }

            MouseLeave => {
                self.capture_state.leave();
                sender.input(UpdateCaptureView);
            }

            MouseMove { x, y } => {
                let mode = self.current_input_mode();
                if self.requested_input_mode == InputMode::Seamless
                    && mode == InputMode::Seamless
                    && !self.input.is_absolute
                {
                    let had_capture = self.capture_state.should_forward();
                    self.capture_state.release();
                    if had_capture {
                        sender.input(UpdateCaptureView);
                    }
                    return;
                }
                let current_capture = self.capture_state.current();
                let is_in_viewport = self.coord_system.is_in_viewport(x, y);
                match (mode, current_capture, is_in_viewport) {
                    // Pointer just entered VM viewport.
                    (InputMode::Seamless, Capture::Idle, true) => {
                        self.capture_state.enter(mode);
                        sender.input(UpdateCaptureView);
                    }
                    // Pointer just left VM viewport.
                    (InputMode::Seamless, Capture::Seamless, false) => {
                        self.capture_state.leave();
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
                let mode = self.current_input_mode();
                if mode != InputMode::Confined {
                    // Capture requests are meaningful only in confined mode.
                    return;
                }
                let should_capture = event.should_capture();
                let was_captured = self.capture_state.should_forward();
                if !should_capture {
                    self.capture_state.release();
                    if let Some(confine) = &mut self.confine_state {
                        confine.wayland_confine.borrow_mut().unconfine();
                        confine.is_captured = false;
                    } else {
                        mks_error!("Confined state unavailable while stopping pointer capture");
                    }
                    mks_info!("Pointer confinement released");
                    sender.input(UpdateCaptureView);
                }
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
                    confine.wayland_confine.borrow_mut().confine_pointer(&proxy, widget_rect, prefer_relative)
                });
                if !confine_ok {
                    mks_error!("Failed to establish Wayland pointer confinement");
                    self.show_confined_capture_unavailable_toast(prefer_relative, &sender);
                    if let Some(confine) = &mut self.confine_state {
                        confine.is_captured = false;
                    }
                    sender.input(UpdateCaptureView);
                    return;
                }
                self.capture_state.capture(mode);
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
                sender.input(UpdateCaptureView);
            }

            Qemu(event) => match self.screen.handle_event(event) {
                Ok(flags) => {
                    let (w, h) = self.screen.resolution();
                    self.coord_system.set_vm_resolution(w, h);
                    self.dirty_flags.merge(flags);
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
                self.dirty_flags.set_frame_and_cursor_dirty();
                if self.scaling_mode == ScalingMode::ResizeGuest
                    && let Some((w_nz, h_nz)) = self.coord_system.physical_canvas_size()
                {
                    self.reset_resize_timer(w_nz, h_nz);
                }
            }

            // HideCaptureHint is now handled by adw::Toast auto-dismiss
            ShowToast(_text) => {
                // This is handled in update_with_view via widgets.toast_overlay
                // We store it temporarily in dirty_flags for the view update
                // self.dirty_flags.set_frame_and_cursor_dirty();
            }

            UpdateCaptureView => {
                self.dirty_flags.set_cursor_dirty();
            }

            MouseButton { button, transition } => {
                if self.requested_input_mode == InputMode::Seamless
                    && self.current_input_mode() == InputMode::Seamless
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
                if self.current_input_mode() == InputMode::Confined
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

            Key { keycode, transition } => {
                if !self.capture_state.should_forward() {
                    return;
                }
                self.input.press_keyboard(keycode, transition);
            }

            UpdateMonitorInfo { pixel_pitch_mm } => {
                self.pixel_pitch_mm = pixel_pitch_mm;
            }

            MouseModeChanged { is_absolute } => {
                mks_info!("Guest mouse mode switched to {}", if is_absolute { "absolute" } else { "relative" });
                self.input.set_mouse_mode(is_absolute);
                if self.current_input_mode() == InputMode::Confined && self.capture_state.should_forward() {
                    let widget_rect = self.confined_widget_rect();
                    let Some(proxy) = self.current_wayland_surface() else {
                        mks_error!("Failed to resolve wl_surface proxy; cannot reconfigure confined capture");
                        return;
                    };
                    let prefer_relative = !is_absolute;
                    let recapture_ok = self.confine_state.as_mut().is_some_and(|confine| {
                        confine.wayland_confine.borrow_mut().unconfine();
                        let ok =
                            confine.wayland_confine.borrow_mut().confine_pointer(&proxy, widget_rect, prefer_relative);
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
                        self.capture_state.release();
                        self.show_confined_capture_unavailable_toast(prefer_relative, &sender);
                        sender.input(UpdateCaptureView);
                    }
                }
                if !is_absolute
                    && self.requested_input_mode == InputMode::Seamless
                    && self.current_input_mode() == InputMode::Seamless
                {
                    let had_capture = self.capture_state.should_forward();
                    self.capture_state.release();
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
        // Handle ShowToast message here to display toast via adw::ToastOverlay
        if let Message::ShowToast(text) = &message {
            let toast = Toast::new(text);
            toast.set_timeout(TOAST_DURATION_SECS);
            widgets.toast_overlay.add_toast(toast);
        }
        let was_forwarding_input = self.capture_state.should_forward();
        self.update(message, sender, root);
        if was_forwarding_input && !self.capture_state.should_forward() {
            self.input.release_all_keys();
        }
        let dirty_flags = mem::take(&mut self.dirty_flags);
        self.render_view(widgets, dirty_flags);
    }

    fn shutdown(&mut self, _widgets: &mut Self::Widgets, _output: relm4::Sender<Self::Output>) {
        self.confine_state = None;
        if let Some(handle) = self.resize_timer.take() {
            handle.abort();
        }
    }
}
