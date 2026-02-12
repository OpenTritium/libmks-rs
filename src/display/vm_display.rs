use crate::{
    dbus::{
        console::ConsoleController,
        keyboard::KeyboardController,
        listener::Event as QemuEvent,
        mouse::{Button as QemuButton, MouseController},
    },
    display::{
        screen::{Screen, UpdateFlags},
        wayland_lock::WaylandLock,
    },
    keymaps::Qnum,
};
use gdk4_wayland::{WaylandDisplay, WaylandSurface, gdk::Texture, prelude::*};
use kanal::AsyncReceiver;
use log::{error, info, warn};
use relm4::{
    gtk::{
        Align, AspectFrame, ContentFit, CssProvider, DrawingArea, EventController, EventControllerKey,
        EventControllerMotion, EventControllerScroll, EventControllerScrollFlags, Fixed, GestureClick, GraphicsOffload,
        GraphicsOffloadEnabled, Label, Overlay, Picture, STYLE_PROVIDER_PRIORITY_APPLICATION, accelerator_get_label,
        gdk::{Display, Key, ModifierType},
        glib::{Propagation, object::Cast, translate::IntoGlib},
        prelude::*,
        style_context_add_provider_for_display,
    },
    prelude::*,
};
use std::{fmt, num::NonZeroU32, sync::Once, time::Duration, vec::Vec};
use tokio::{task::AbortHandle, time::sleep};

const INCH_TO_MM: f64 = 25.4;
const WINDOWS_DEFAULT_DPI: f64 = 96.;
const DEFAULT_MM_PER_PIXEL: f64 = INCH_TO_MM / WINDOWS_DEFAULT_DPI;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputCaptureMode {
    Locked,
    Seamless,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalingMode {
    ResizeGuest,
    FixedGuest,
}

#[derive(Debug)]
pub enum Message {
    Qemu(QemuEvent),
    CanvasResize(NonZeroU32, NonZeroU32),
    MouseMove { x: f64, y: f64 },
    MouseButton { button: u32, pressed: bool },
    Scroll { dy: f64 },
    Key { keyval: u32, keycode: u32, pressed: bool },
    UpdateMonitorInfo { mm_per_pixel: f64 },
    SetScalingMode(ScalingMode),
    ToggleCapture(bool, Option<(f64, f64)>),
    HideCaptureHint,
    UpdateCaptureView, // 通知视图层（View）根据当前的捕获状态（Captured/Uncaptured）重绘光标和提示 UI
    CheckCaptureState,
    WaylandPoll,
    MouseLeave,
    SetInputCaptureMode(InputCaptureMode),
}

pub struct VmDisplayModel {
    pub screen: Screen,
    pub changes: UpdateFlags,
    console_ctrl: ConsoleController,
    mouse_ctrl: MouseController,
    keyboard_ctrl: KeyboardController,
    canvas_size: Option<(f64, f64)>,
    last_sent_mouse: Option<(u32, u32)>,
    scaling_mode: ScalingMode,
    mm_per_pixel: f64,
    resize_timer: Option<AbortHandle>,
    scroll_acc_y: f64,
    is_captured: bool,
    hint_visible: bool,
    hint_timer: Option<AbortHandle>,
    input_widget: DrawingArea,
    wayland_lock: Option<WaylandLock>,
    input_mode: InputCaptureMode,
    is_mouse_over: bool,
}

pub struct VmDisplayWidgets {
    pub aspect_frame: AspectFrame,
    pub view_stack: Overlay,
    pub vm_picture: Picture,
    pub input_plane: DrawingArea,
    pub cursor_layer: Fixed,
    pub cursor_picture: Picture,
    pub controllers: Box<[EventController]>,
    pub capture_hint: Label,
}

pub struct VmDisplayInit {
    pub rx: AsyncReceiver<QemuEvent>,
    pub console_ctrl: ConsoleController,
    pub mouse_ctrl: MouseController,
    pub keyboard_ctrl: KeyboardController,
    pub grab_shortcut: GrabShortcut,
}

impl VmDisplayModel {
    #[inline]
    fn try_cancel_hint_timer(&mut self) -> bool {
        if let Some(handle) = self.hint_timer.take() {
            handle.abort();
            return true;
        }
        false
    }

    #[inline]
    fn reset_hint_timer(&mut self, sender: ComponentSender<Self>) {
        self.try_cancel_hint_timer();
        self.hint_timer = Some(
            relm4::spawn(async move {
                sleep(Duration::from_secs(3)).await;
                sender.input(Message::HideCaptureHint);
            })
            .abort_handle(),
        );
    }

    #[inline]
    fn try_hide_hint(&mut self) -> bool {
        self.hint_visible = false;
        self.try_cancel_hint_timer()
    }

    #[inline]
    fn reset_resize_timer(&mut self, w: NonZeroU32, h: NonZeroU32) {
        if let Some(handle) = self.resize_timer.take() {
            handle.abort();
        }
        let mm_per_pixel = self.mm_per_pixel;
        let console = self.console_ctrl.clone();
        let w = w.get();
        let h = h.get();
        self.resize_timer = Some(
            relm4::spawn(async move {
                sleep(Duration::from_millis(200)).await;
                let w_mm = (w as f64 * mm_per_pixel) as u16;
                let h_mm = (h as f64 * mm_per_pixel) as u16;
                info!("Resize debounced: {w}x{h} ({w_mm}mm x {h_mm}mm)");
                if let Err(e) = console.set_ui_info(w_mm, h_mm, 0, 0, w, h).await {
                    error!(error:? = e; "Failed to send resize command");
                }
            })
            .abort_handle(),
        );
    }

    #[inline]
    fn ensure_wayland_lock(&mut self) -> Option<&WaylandLock> {
        if self.wayland_lock.is_some() {
            return self.wayland_lock.as_ref();
        }
        if let Some(display) = Display::default() {
            if let Ok(wl_display) = display.downcast::<WaylandDisplay>() {
                info!("Wayland environment detected. Initializing pointer lock lazily.");
                let lock = WaylandLock::new_from_gdk(&wl_display, self.mouse_ctrl.clone());
                self.wayland_lock = Some(lock);
            } else {
                error!("Pointer lock requested but not running on Wayland.");
            }
        }
        self.wayland_lock.as_ref()
    }

    #[inline]
    fn map_input_to_vm(&self, x: f64, y: f64) -> Option<(u32, u32)> {
        let (vm_w, vm_h) = self.screen.resolution();
        let (canvas_w, canvas_h) = self.canvas_size?;

        if vm_w == 0 || vm_h == 0 || canvas_w <= 0. || canvas_h <= 0. {
            return None;
        }

        let raw_x = (x / canvas_w) * vm_w as f64;
        let raw_y = (y / canvas_h) * vm_h as f64;
        let target_x = raw_x.clamp(0., vm_w as f64 - 1.) as u32;
        let target_y = raw_y.clamp(0., vm_h as f64 - 1.) as u32;

        Some((target_x, target_y))
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
        // todo lazy loading
        // let wayland_lock = if let Some(display) = Display::default() {
        //     if let Ok(wl_display) = display.downcast::<WaylandDisplay>() {
        //         info!("Detecting Wayland environment. Initializing native pointer lock.");
        //         Some(WaylandLock::new_from_gdk(&wl_display, init.mouse_ctrl.clone()))
        //     } else {
        //         info!("Not running on Wayland. Pointer lock disabled.");
        //         None
        //     }
        // } else {
        //     None
        // };

        let input_plane =
            DrawingArea::builder().focusable(true).focus_on_click(true).hexpand(true).vexpand(true).build();
        input_plane.set_content_width(0);
        input_plane.set_content_height(0);

        let model = VmDisplayModel {
            screen: Screen::new(),
            changes: UpdateFlags::default(),
            console_ctrl: init.console_ctrl,
            mouse_ctrl: init.mouse_ctrl,
            keyboard_ctrl: init.keyboard_ctrl,
            canvas_size: None,
            last_sent_mouse: None,
            scaling_mode: ScalingMode::ResizeGuest,
            mm_per_pixel: DEFAULT_MM_PER_PIXEL,
            resize_timer: None,
            scroll_acc_y: 0.,
            is_captured: false,
            hint_visible: false,
            hint_timer: None,
            input_widget: input_plane.clone(),
            wayland_lock: None,
            input_mode: InputCaptureMode::Seamless,
            is_mouse_over: false,
        };
        let aspect_frame = AspectFrame::builder()
            .halign(Align::Fill)
            .valign(Align::Fill)
            .hexpand(true)
            .vexpand(true)
            .xalign(0.5)
            .yalign(0.5)
            .ratio(4. / 3.)
            .obey_child(false)
            .build();
        let view_stack = Overlay::builder().hexpand(true).vexpand(true).build();
        let offload = GraphicsOffload::builder().enabled(GraphicsOffloadEnabled::Enabled).build();
        let vm_picture = Picture::builder()
            .can_shrink(true)
            .content_fit(ContentFit::Fill)
            .hexpand(true)
            .vexpand(true)
            .can_focus(false)
            .can_target(false)
            .build();
        offload.set_child(Some(&vm_picture));

        let sender_clone = sender.clone();
        let update_monitor_info = move |widget: &DrawingArea| {
            let display = widget.display();
            let Some(native) = widget.native() else { return };
            let Some(surface) = native.surface() else { return };
            let Some(monitor) = display.monitor_at_surface(&surface) else { return };
            let geometry = monitor.geometry();
            let width_mm = monitor.width_mm();
            let geometry_width = geometry.width();
            if width_mm > 0 && geometry_width > 0 {
                let mm_per_pixel = width_mm as f64 / geometry_width as f64;
                sender_clone.input(UpdateMonitorInfo { mm_per_pixel });
            }
        };

        let updater = update_monitor_info.clone();
        input_plane.connect_realize(move |widget| updater(widget));
        // root.connect_realize(move |_root| {});

        let mut controllers = Vec::new();

        let motion_ctrl = EventControllerMotion::new();
        let sender_clone = sender.clone();
        let input_plane_clone = input_plane.clone();
        motion_ctrl.connect_motion(move |_, x, y| {
            let scale = input_plane_clone.scale_factor() as f64;
            sender_clone.input(MouseMove { x: x * scale, y: y * scale });
        });
        let sender_c = sender.clone();
        motion_ctrl.connect_leave(move |_| sender_c.input(MouseLeave));
        input_plane.add_controller(motion_ctrl.clone());
        controllers.push(motion_ctrl.upcast());

        let click = GestureClick::new();
        click.set_button(0);
        let sender_clone = sender.clone();
        let input_plane_clone = input_plane.clone();
        click.connect_pressed(move |gesture, _, x, y| {
            input_plane_clone.grab_focus();
            let scale = input_plane_clone.scale_factor() as f64;
            sender_clone.input(ToggleCapture(true, Some((x * scale, y * scale))));
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
                sender_for_release.input(ToggleCapture(false, None));
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

        let cursor_layer = Fixed::builder().can_target(false).hexpand(true).vexpand(true).build();
        let cursor_picture = Picture::builder()
            .can_shrink(true)
            .content_fit(ContentFit::Fill)
            .can_target(false)
            .halign(Align::Start)
            .valign(Align::Start)
            .build();
        cursor_layer.put(&cursor_picture, 0., 0.);

        let capture_hint = Label::builder()
            .label(format!("Press {grab_shortcut} to release mouse"))
            .halign(Align::Center)
            .valign(Align::Start)
            .margin_top(20)
            .css_classes(["toast-label", "toast-hidden"])
            .selectable(false)
            .can_target(false)
            .build();

        let resize_handler = {
            let updater = update_monitor_info.clone();
            let sender = sender.clone();
            move |widget: &DrawingArea| {
                updater(widget);
                let scale = widget.scale_factor();
                let width = widget.width();
                let height = widget.height();
                let phy_w = width * scale;
                let phy_h = height * scale;
                if let (Some(w), Some(h)) = (NonZeroU32::new(phy_w.max(1) as u32), NonZeroU32::new(phy_h.max(1) as u32))
                {
                    sender.input(CanvasResize(w, h));
                }
            }
        };
        let handler_clone = resize_handler.clone();
        input_plane.connect_resize(move |widget, _, _| handler_clone(widget));
        let handler_clone = resize_handler.clone();
        input_plane.connect_scale_factor_notify(move |widget| handler_clone(widget));

        view_stack.set_child(Some(&offload));
        view_stack.add_overlay(&input_plane);
        view_stack.add_overlay(&cursor_layer);
        view_stack.add_overlay(&capture_hint);
        aspect_frame.set_child(Some(&view_stack));
        root.set_child(Some(&aspect_frame));

        relm4::spawn(async move {
            while let Ok(event) = init.rx.recv().await {
                sender.input(Qemu(event));
            }
            warn!("VM display channel closed");
            sender.input(Qemu(QemuEvent::Disable));
        });
        let controllers = controllers.into_boxed_slice();
        let widgets = VmDisplayWidgets {
            aspect_frame,
            view_stack,
            vm_picture,
            input_plane,
            cursor_layer,
            cursor_picture,
            controllers,
            capture_hint,
        };
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        use InputCaptureMode::*;
        use Message::*;
        use ScalingMode::*;
        match msg {
            SetInputCaptureMode(mode) => {
                if self.input_mode == mode {
                    return;
                }
                info!("Switching Input Mode to: {mode:?}");
                if self.input_mode == Locked
                    && self.is_captured
                    && let Some(ref mut wl_lock) = self.wayland_lock
                {
                    wl_lock.unlock_pointer();
                    info!("Mode switch: Wayland pointer unlocked");
                }
                self.input_mode = mode;
                match mode {
                    Seamless => {
                        self.is_captured = self.is_mouse_over;
                        self.try_hide_hint();
                    }
                    Locked => {
                        self.is_captured = false;
                        self.try_hide_hint();
                    }
                }
                sender.input(UpdateCaptureView);
            }

            MouseLeave => {
                self.is_mouse_over = false;
                if self.input_mode == Seamless {
                    self.is_captured = false;
                    sender.input(UpdateCaptureView);
                }
            }

            MouseMove { x, y } => {
                if !self.is_mouse_over {
                    self.is_mouse_over = true;
                    if self.input_mode == Seamless {
                        self.is_captured = true;
                        sender.input(UpdateCaptureView);
                    }
                }
                match self.input_mode {
                    Locked => {}
                    Seamless => {
                        if let Some((target_x, target_y)) = self.map_input_to_vm(x, y) {
                            let new_mouse_pos = (target_x, target_y);
                            if self.last_sent_mouse.is_none_or(|old| old != new_mouse_pos) {
                                self.last_sent_mouse = Some(new_mouse_pos);
                                if let Err(e) = self.mouse_ctrl.try_set_abs_position(target_x, target_y) {
                                    error!(error:? = e; "Failed to set mouse position");
                                }
                            }
                        }
                    }
                }
            }

            ToggleCapture(should_capture, click_pos) => {
                if self.input_mode == Seamless || self.is_captured == should_capture {
                    if should_capture {
                        self.reset_hint_timer(sender.clone());
                    }
                    return;
                }

                let wl_surface_proxy = self.input_widget.native()
                    .and_then(|n| n.surface())
                    .and_then(|s| s.downcast::<WaylandSurface>().ok())
                    .and_then(|ws| ws.wl_surface());

                let target_vm_coords = if should_capture {
                    click_pos.and_then(|(x, y)| self.map_input_to_vm(x, y))
                } else {
                    None
                };

                self.ensure_wayland_lock();
                if let Some(ref mut wl_lock) = self.wayland_lock {
                    wl_lock.dispatch_pending();
                    if should_capture {
                        if let Some((target_x, target_y)) = target_vm_coords {
                            let _ = self.mouse_ctrl.try_set_abs_position(target_x, target_y);
                            self.last_sent_mouse = Some((target_x, target_y));
                        }
                        if let Some(proxy) = &wl_surface_proxy {
                            wl_lock.lock_pointer(proxy);
                            sender.input(WaylandPoll);
                            info!("Pointer locked");
                        }
                    } else {
                        let cursor = &self.screen.cursor;
                        let (vm_w, vm_h) = self.screen.resolution();

                        if vm_w > 0 && vm_h > 0 {
                            if let Some(native) = self.input_widget.native() {
                                let bounds = self.input_widget.compute_bounds(&native).unwrap_or_default();

                                let widget_x = bounds.x() as f64;
                                let widget_y = bounds.y() as f64;
                                let widget_w = bounds.width() as f64;
                                let widget_h = bounds.height() as f64;

                                let internal_x = (cursor.x as f64 / vm_w as f64) * widget_w;
                                let internal_y = (cursor.y as f64 / vm_h as f64) * widget_h;

                                let final_hint_x = widget_x + internal_x;
                                let final_hint_y = widget_y + internal_y;

                                wl_lock.unlock_with_hint(wl_surface_proxy.as_ref(), final_hint_x, final_hint_y);
                                info!(
                                    "🎯 High-Precision Unlock: VM({:?}) bounds=({:.2},{:.2},{:.2}x{:.2}) internal=({:.2},{:.2}) surface=({:.2},{:.2})",
                                        (cursor.x, cursor.y),
                                        widget_x, widget_y, widget_w, widget_h,
                                        internal_x, internal_y,
                                        final_hint_x, final_hint_y
                                );
                            } else {
                                wl_lock.unlock_pointer();
                                info!("Pointer unlocked (no native for bounds calculation)");
                            }
                        } else {
                            wl_lock.unlock_pointer();
                            info!("Pointer unlocked (no valid VM resolution)");
                        }
                    }
                }
                self.is_captured = should_capture;
                self.hint_visible = should_capture;
                if should_capture {
                    self.reset_hint_timer(sender.clone());
                } else {
                    self.try_cancel_hint_timer();
                }
                sender.input(UpdateCaptureView);
            }

            WaylandPoll => {
                if self.input_mode == Locked
                    && self.is_captured
                    && let Some(wl_lock) = &self.wayland_lock
                {
                    wl_lock.dispatch_pending();
                    let sender = sender.clone();
                    relm4::spawn(async move {
                        // 1000ms / 8ms ≈ 125 Hz
                        sleep(Duration::from_millis(8)).await;
                        sender.input(WaylandPoll);
                    });
                }
            }

            Qemu(event) => {
                if let Ok(flags) = self.screen.handle_event(event) {
                    self.changes.cursor |= flags.cursor;
                    self.changes.frame |= flags.frame;
                }
            }

            CanvasResize(w, h) => {
                self.canvas_size = Some((w.get() as f64, h.get() as f64));
                self.changes.cursor = true;
                if self.scaling_mode == ResizeGuest {
                    self.reset_resize_timer(w, h);
                }
            }

            SetScalingMode(mode) => {
                self.scaling_mode = mode;
                if mode == ResizeGuest
                    && let Some((w, h)) = self.canvas_size
                    && let (Some(w_nz), Some(h_nz)) = (NonZeroU32::new(w as u32), NonZeroU32::new(h as u32))
                {
                    self.reset_resize_timer(w_nz, h_nz);
                }
            }

            CheckCaptureState => {
                if self.is_captured {
                    sender.input(UpdateCaptureView);
                }
            }

            HideCaptureHint => {
                self.hint_visible = false;
                self.try_cancel_hint_timer();
                self.changes.cursor = true;
            }

            UpdateCaptureView => {
                self.changes.cursor = true;
            }

            MouseButton { button, pressed } => {
                let Some(btn) = QemuButton::from_xorg(button) else { return };
                let ctrl = self.mouse_ctrl.clone();
                relm4::spawn(async move {
                    let res = if pressed {
                        ctrl.press(btn).await
                    } else {
                        ctrl.release(btn).await
                    };
                    if let Err(e) = res {
                        error!(error:? = e; "Failed to {} mouse button", if pressed { "press" } else { "release" });
                    }
                });
            }

            Scroll { dy } => {
                if !self.is_captured {
                    return;
                }
                self.scroll_acc_y += dy;
                if self.scroll_acc_y.abs() >= 1. {
                    let steps = self.scroll_acc_y.trunc();
                    self.scroll_acc_y -= steps;
                    let steps = steps as i64;
                    let ctrl = self.mouse_ctrl.clone();
                    relm4::spawn(async move {
                        for _ in 0..steps.abs() {
                            let btn = if steps.is_positive() {
                                QemuButton::WheelDown
                            } else {
                                QemuButton::WheelUp
                            };
                            if let Err(e) = ctrl.press(btn).await {
                                error!(error:? = e; "Failed to press mouse button");
                            }
                            if let Err(e) = ctrl.release(btn).await {
                                error!(error:? = e; "Failed to release mouse button");
                            }
                        }
                    });
                }
            }

            Key { keyval: _, keycode, pressed } => {
                if !self.is_captured {
                    return;
                }
                let qnum = Qnum::from_xorg_keycode(keycode);
                let ctrl = self.keyboard_ctrl.clone();
                relm4::spawn(async move {
                    if pressed {
                        if let Err(e) = ctrl.press(qnum).await {
                            error!(error:? = e; "Failed to press key");
                        }
                    } else {
                        if let Err(e) = ctrl.release(qnum).await {
                            error!(error:? = e; "Failed to release key");
                        }
                    }
                });
            }

            UpdateMonitorInfo { mm_per_pixel } => {
                self.mm_per_pixel = mm_per_pixel;
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
        widgets.input_plane.set_cursor_from_name(self.is_captured.then_some("none"));
        if !self.changes.any() {
            return;
        }
        if self.changes.frame {
            widgets.vm_picture.set_paintable(self.screen.get_background_texture());
        }
        let ui_scale = widgets.input_plane.scale_factor() as f64;
        let (vm_w, vm_h) = self.screen.resolution();
        let Some((canvas_w, canvas_h)) = self.canvas_size else { return };
        let cursor = &self.screen.cursor;
        let show_vm_cursor = cursor.visible
            && match self.input_mode {
                InputCaptureMode::Locked => true,
                InputCaptureMode::Seamless => self.is_mouse_over,
            };
        widgets.cursor_picture.set_visible(show_vm_cursor);
        if !show_vm_cursor {
            widgets.cursor_picture.set_paintable(None::<&Texture>);
            return;
        }
        if vm_w == 0 || vm_h == 0 || canvas_w <= 0. || canvas_h <= 0. {
            return;
        }
        widgets.aspect_frame.set_ratio(vm_w as f32 / vm_h as f32);
        let scale_x = canvas_w / vm_w as f64;
        let scale_y = canvas_h / vm_h as f64;
        let scale = scale_x.min(scale_y);
        let final_x_phy = (cursor.x - cursor.hot_x) as f64 * scale;
        let final_y_phy = (cursor.y - cursor.hot_y) as f64 * scale;
        widgets.cursor_layer.move_(&widgets.cursor_picture, final_x_phy / ui_scale, final_y_phy / ui_scale);

        let Some(tex) = &cursor.texture else { return };
        widgets.cursor_picture.set_paintable(Some(tex));
        let cursor_w_phy = tex.width() as f64 * scale;
        let cursor_h_phy = tex.height() as f64 * scale;
        let cursor_w_logical = (cursor_w_phy / ui_scale).ceil() as i32;
        let cursor_h_logical = (cursor_h_phy / ui_scale).ceil() as i32;
        widgets.cursor_picture.set_width_request(cursor_w_logical);
        widgets.cursor_picture.set_height_request(cursor_h_logical);
    }
}
