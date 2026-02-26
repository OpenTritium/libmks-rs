use crate::{
    dbus::{console::ConsoleController, listener::Event as QemuEvent, mouse::MouseController},
    display::{
        capture_state::{Capture, CaptureState},
        coordinate::Coordinate,
        input_handler::InputHandler,
        screen::{DirtyFlags, Screen},
        wayland_confine::WaylandConfine,
    },
};
use gdk4_wayland::{
    WaylandDisplay, WaylandSurface,
    gdk::{Key, ModifierType, Rectangle, Texture},
    glib::{ControlFlow, IOCondition, Propagation, SourceId, translate::IntoGlib, unix_fd_add_local},
    prelude::*,
};
use kanal::AsyncReceiver;
use log::{debug, error, info, warn};
use relm4::{
    Component, ComponentParts, ComponentSender,
    gtk::{
        Align, ContentFit, CssProvider, DrawingArea, EventController, EventControllerKey, EventControllerMotion,
        EventControllerScroll, EventControllerScrollFlags, Fixed, GestureClick, GraphicsOffload,
        GraphicsOffloadEnabled, Label, Overlay, Picture, STYLE_PROVIDER_PRIORITY_APPLICATION, accelerator_get_label,
        gdk::Display, graphene::Point, gsk::Transform, prelude::*, style_context_add_provider_for_display,
    },
};
use std::{cell::RefCell, fmt, num::NonZeroU32, rc::Rc, sync::Once, time::Duration};
use tokio::{task::AbortHandle, time::sleep};

const INCH_TO_MM: f32 = 25.4;
const DEFAULT_DPI: f32 = 96.;
const DEFAULT_PIXEL_PITCH_MM: f32 = INCH_TO_MM / DEFAULT_DPI;

/// 确保 css provider 只被初始化一次，因为它不是 sync 的所以我们没有使用 LazyCell
#[inline]
fn ensure_css_loaded() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let provider = CssProvider::new();
        provider.load_from_string(include_str!("capture-hint.css"));
        if let Some(display) = Display::default() {
            style_context_add_provider_for_display(&display, &provider, STYLE_PROVIDER_PRIORITY_APPLICATION);
        }
    });
}

/// 捕获输入的快捷键配置
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
    pub hint_timer: Option<AbortHandle>,
}

impl ConfineState {
    pub fn connect_to_wayland(mouse_ctrl: MouseController) -> Option<Self> {
        let display = Display::default()?;
        let wl_display = display.downcast::<WaylandDisplay>().ok()?;
        info!("Wayland environment detected. Initializing pointer confine.");
        let confine = WaylandConfine::from_gdk(&wl_display, mouse_ctrl);
        let confine = Rc::new(RefCell::new(confine));
        let fd = confine.borrow().get_conn_raw_fd();
        let confine_clone = confine.clone();
        let poll_source = unix_fd_add_local(fd, IOCondition::IN, move |_fd, _condition| {
            confine_clone.borrow().dispatch_pending();
            ControlFlow::Continue
        });
        debug!("Wayland fd monitor attached to GLib main context");
        Some(Self { wayland_confine: confine, poll_source: Some(poll_source), is_captured: false, hint_timer: None })
    }

    #[inline]
    fn try_cancel_hint_timer(&mut self) -> bool {
        if let Some(handle) = self.hint_timer.take() {
            handle.abort();
            return true;
        }
        false
    }

    #[inline]
    fn reset_hint_timer(&mut self, sender: ComponentSender<VmDisplayModel>) {
        self.try_cancel_hint_timer();
        self.hint_timer = Some(
            relm4::spawn(async move {
                sleep(Duration::from_secs(3)).await;
                sender.input(Message::HideCaptureHint);
            })
            .abort_handle(),
        );
    }
}

impl Drop for ConfineState {
    fn drop(&mut self) {
        if let Some(source) = self.poll_source.take() {
            source.remove();
        }
        self.wayland_confine.borrow_mut().unconfine();
        self.try_cancel_hint_timer();
    }
}

/// 输入捕获模式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    /// 强捕获模式，该模式下鼠标无法逃逸虚拟机画面
    Confined,
    /// 弱捕获模式，光标会自动切换到虚拟机光标，离开虚拟机画面自动切换回宿主光标
    Seamless,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalingMode {
    /// 根据宿主窗口大小自动缩放虚拟机分辨率
    ResizeGuest,
    /// 固定虚拟机分辨率，将虚拟机画面按照宿主窗口大小进行缩放
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
    MouseButton { button: u32, pressed: bool },
    Scroll { dy: f64 },
    Key { keyval: u32, keycode: u32, pressed: bool },
    UpdateMonitorInfo { pixel_pitch_mm: f32 },
    SetScalingMode(ScalingMode),
    ToggleCapture(CaptureEvent),
    HideCaptureHint,
    UpdateCaptureView, // 对于视图来说，它只知道收到这个事件以后隐藏宿主光标就行了
    CheckCaptureState,
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
    hint_visible: bool,
    coord_system: Coordinate,
    input: InputHandler,
    capture_state: CaptureState,
}

pub struct VmDisplayWidgets {
    pub view_stack: Overlay,
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
                info!("Resize debounced: {w}x{h} ({w_mm}mm x {h_mm}mm)");
                if let Err(e) = console.set_ui_info(w_mm, h_mm, 0, 0, w, h) {
                    error!(error:? = e; "Failed to send resize command");
                }
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
            hint_visible: false,
            input_overlay: input_plane.clone(),
            confine_state: None,
            coord_system: Coordinate::new(0, 0, 0., 0., 1.),
            input: init.input_handler,
            capture_state: CaptureState::new(),
        };
        let view_stack = Overlay::builder().hexpand(true).vexpand(true).build();
        let vm_picture = Picture::builder()
            .can_shrink(true)
            .content_fit(ContentFit::Contain)
            .halign(Align::Center)
            .valign(Align::Center)
            .can_target(false)
            .build();
        let offload = GraphicsOffload::builder()
            .enabled(GraphicsOffloadEnabled::Enabled)
            .child(&vm_picture)
            .hexpand(true)
            .vexpand(true)
            .build();
        let sender_clone = sender.clone();
        let update_monitor_info = move |widget: &DrawingArea| {
            let display = widget.display();
            let Some(native) = widget.native() else {
                error!("Failed to get native widget");
                return;
            };
            let Some(surface) = native.surface() else {
                error!("Failed to get surface from native");
                return;
            };
            let Some(monitor) = display.monitor_at_surface(&surface) else {
                error!("Failed to get monitor from surface");
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
                debug!(
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
            sender_clone.input(Message::ToggleCapture(CaptureEvent::Capture { click_pos: Some((x as f32, y as f32)) }));
            sender_clone.input(MouseButton { button: gesture.current_button(), pressed: true });
        });
        let sender_clone = sender.clone();
        click.connect_released(move |gesture, _, _, _| {
            sender_clone.input(MouseButton { button: gesture.current_button(), pressed: false });
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
        let grab_shortcut = init.grab_shortcut;
        let sender_for_release = sender.clone();
        let sender_for_key = sender.clone();
        key.connect_key_pressed(move |_, keyval, keycode, modifiers| {
            if modifiers.contains(grab_shortcut.mask) && keyval == grab_shortcut.key {
                sender_for_release.input(Message::ToggleCapture(CaptureEvent::Release));
                return Propagation::Stop;
            }
            let keyval_raw: u32 = keyval.into_glib();
            sender_for_key.input(Key { keyval: keyval_raw, keycode, pressed: true });
            Propagation::Stop
        });
        let sender_clone = sender.clone();
        key.connect_key_released(move |_, keyval, keycode, _| {
            let keyval_raw: u32 = keyval.into_glib();
            sender_clone.input(Key { keyval: keyval_raw, keycode, pressed: false });
        });
        root.add_controller(key.clone());
        controllers.push(key.upcast());

        let capture_hint = Label::builder()
            .label(format!("Press {grab_shortcut} to release mouse"))
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
                    warn!("Can not resize canvas: ({w}, {h})");
                }
            }
        };
        let handler_clone = resize_handler.clone();
        input_plane.connect_resize(move |widget, _, _| handler_clone(widget));
        let handler_clone = resize_handler.clone();
        input_plane.connect_scale_factor_notify(move |widget| handler_clone(widget));

        view_stack.set_child(Some(&offload));
        view_stack.add_overlay(&input_plane);
        view_stack.add_overlay(&cursor_fixed);
        view_stack.add_overlay(&capture_hint);
        root.set_child(Some(&view_stack));

        relm4::spawn(async move {
            while let Ok(event) = init.rx.recv().await {
                sender.input(Qemu(event));
            }
            warn!("VM display channel closed");
            sender.input(Qemu(QemuEvent::Disable));
        });
        let controllers = controllers.into_boxed_slice();
        let widgets = VmDisplayWidgets {
            view_stack,
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
                if self.input_mode() == mode {
                    warn!("Input mode already set to {mode:?}");
                    return;
                }
                self.capture_state.reset();
                if self.confine_state.take().is_some() {
                    self.hint_visible = false;
                } else {
                    let Some(ctrl) = self.input.mouse_ctrl() else {
                        warn!("No mouse controller available, fallback to seamless");
                        return;
                    };
                    let Some(confine) = ConfineState::connect_to_wayland(ctrl.clone()) else {
                        warn!("Failed connect to wayland session, fallback to seamless");
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
                let current_capture = self.capture_state.current();
                match (mode, current_capture) {
                    // 鼠标进入了有效画面区域
                    (InputMode::Seamless, Capture::Idle) => {
                        self.capture_state.on_mouse_enter(mode);
                        sender.input(UpdateCaptureView);
                    }
                    // 鼠标从有效画面区域离开
                    (InputMode::Seamless, Capture::Seamless) => {
                        self.capture_state.on_mouse_leave();
                        sender.input(UpdateCaptureView);
                    }
                    // 其他情况（如 Confined 模式，或状态未改变）无需干预
                    _ => {}
                }
                if self.capture_state.should_forward() {
                    self.input.move_mouse_to(x, y, &self.coord_system);
                }
            }

            ToggleCapture(event) => {
                let mode = self.input_mode();
                let should_capture = event.should_capture();

                let native = self.input_overlay.native();
                let widget_rect = if let Some(native) = &native
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
                };
                let click_pos = event.click_pos();
                let vm_coords = click_pos.and_then(|(x, y)| self.coord_system.widget_to_guest(x, y));
                let Some(proxy) = native
                    .and_then(|n| n.surface())
                    .and_then(|s| s.downcast::<WaylandSurface>().ok())
                    .and_then(|ws| ws.wl_surface())
                else {
                    warn!("Failed to get wl_surface proxy");
                    return;
                };
                let Some(confine) = &mut self.confine_state else {
                    sender.input(UpdateCaptureView);
                    return;
                };
                self.hint_visible = should_capture;
                if should_capture {
                    self.capture_state.on_click(mode);
                    confine.reset_hint_timer(sender.clone());
                    confine.wayland_confine.borrow_mut().confine_pointer(&proxy, &widget_rect);
                    if let Some((x, y)) = vm_coords {
                        let Some(ctrl) = self.input.mouse_ctrl() else {
                            return;
                        };
                        debug!("Mouse was confined, and latest position: {x},{y}");
                        if let Err(e) = ctrl.try_set_abs_position(x, y) {
                            error!(error:? = e; "Failed to set absolute position");
                        }
                    }
                    info!("Pointer confined to region: {widget_rect:?}");
                } else {
                    self.capture_state.on_release();
                    confine.wayland_confine.borrow_mut().unconfine();
                    confine.try_cancel_hint_timer();
                    info!("Pointer unconfined");
                }
                confine.is_captured = should_capture;
                sender.input(UpdateCaptureView);
            }

            Qemu(event) => {
                // 注意：不要同步 QEMU 的 MouseSet 位置到 InputHandler！
                // 相对鼠标模式必须只根据宿主机输入计算位移，否则会形成输入反馈循环导致疯狂跳跃
                if let Ok(flags) = self.screen.handle_event(event) {
                    let (w, h) = self.screen.resolution();
                    self.coord_system.set_vm_resolution(w, h);
                    self.dirty_flags.cursor |= flags.cursor;
                    self.dirty_flags.frame |= flags.frame;
                }
            }

            SetScalingMode(mode) => {
                self.scaling_mode = mode;
                if mode == ScalingMode::ResizeGuest
                    && let Some((w_nz, h_nz)) = self.coord_system.target_guest_resolution()
                {
                    self.reset_resize_timer(w_nz, h_nz);
                }
            }

            CanvasResize { logical_width, logical_height } => {
                self.coord_system.set_widget_size(logical_width, logical_height);
                if let Some(_native) = self.input_overlay.native() {
                    self.coord_system.monitor_scale = self.input_overlay.scale_factor() as f32;
                }
                self.dirty_flags.cursor = true;
                if self.scaling_mode == ScalingMode::ResizeGuest
                    && let Some((w_nz, h_nz)) = self.coord_system.target_guest_resolution()
                {
                    self.reset_resize_timer(w_nz, h_nz);
                }
            }

            CheckCaptureState => {
                if self.capture_state.should_forward() && self.input_mode() == InputMode::Confined {
                    sender.input(UpdateCaptureView);
                }
            }

            HideCaptureHint => {
                self.hint_visible = false;
                if let Some(confine) = &mut self.confine_state {
                    confine.try_cancel_hint_timer();
                }
                self.dirty_flags.cursor = true;
            }

            UpdateCaptureView => {
                self.dirty_flags.cursor = true;
            }

            MouseButton { button, pressed } => {
                // Sync call - try_send is non-blocking
                self.input.press_mouse_button(button, pressed);
            }

            Scroll { dy } => {
                if !self.capture_state.should_forward() {
                    return;
                }
                let steps = self.input.cache_mouse_scroll(dy);
                if steps != 0 {
                    // Sync call - try_send is non-blocking
                    self.input.scroll_mouse(steps);
                }
            }

            Key { keyval: _, keycode, pressed } => {
                if !self.capture_state.should_forward() {
                    return;
                }
                // Sync call - try_send is non-blocking
                self.input.press_keyboard(keycode, pressed);
            }

            UpdateMonitorInfo { pixel_pitch_mm } => {
                self.pixel_pitch_mm = pixel_pitch_mm;
            }

            MouseModeChanged { is_absolute } => {
                let mode_str = if is_absolute { "absolute" } else { "relative" };
                info!("Guest mouse mode changed: {}", mode_str);
                self.input.is_absolute = is_absolute;
            }
        }
    }

    fn update_view(&self, widgets: &mut Self::Widgets, _sender: ComponentSender<Self>) {
        let (class_add, class_remove) = if self.hint_visible {
            ("toast-visible", "toast-hidden")
        } else {
            ("toast-hidden", "toast-visible")
        };
        widgets.capture_hint.add_css_class(class_add);
        widgets.capture_hint.remove_css_class(class_remove);
        let is_interactive = self.capture_state.should_forward();
        widgets.input_overlay.set_cursor_from_name(is_interactive.then_some("none"));
        if !self.dirty_flags.any() {
            return;
        }
        if self.dirty_flags.frame {
            widgets.vm_picture.set_paintable(self.screen.get_background_texture());
        }
        if self.dirty_flags.cursor || self.dirty_flags.frame {
            let cursor = &self.screen.cursor;
            let visiable = cursor.visible && is_interactive;
            widgets.cursor_picture.set_visible(visiable);
            if visiable {
                if let Some(texture) = &cursor.texture {
                    widgets.cursor_picture.set_paintable(Some(texture));
                    let tex_w = texture.width();
                    let tex_h = texture.height();
                    widgets.cursor_picture.set_size_request(tex_w, tex_h);
                    if let Some(transform) = self.coord_system.get_cached_viewport() {
                        let logical_scale = transform.scale;
                        let (logical_offset_x, logical_offset_y) = (transform.offset_x, transform.offset_y);
                        let anchor_x = logical_offset_x + cursor.x as f32 * logical_scale;
                        let anchor_y = logical_offset_y + cursor.y as f32 * logical_scale;
                        let offset_x = cursor.hot_x as f32 * logical_scale;
                        let offset_y = cursor.hot_y as f32 * logical_scale;
                        let draw_x = (anchor_x - offset_x).round();
                        let draw_y = (anchor_y - offset_y).round();
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

    fn shutdown(&mut self, _widgets: &mut Self::Widgets, _output: relm4::Sender<Self::Output>) {
        self.confine_state = None;
        if let Some(handle) = self.resize_timer.take() {
            handle.abort();
        }
    }
}
