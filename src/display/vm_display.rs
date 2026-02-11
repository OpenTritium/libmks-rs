use crate::{
    dbus::{
        console::ConsoleController,
        keyboard::KeyboardController,
        listener::Event as QemuEvent,
        mouse::{Button as QemuButton, MouseController},
    },
    display::screen::{Screen, UpdateFlags},
    keymaps::Qnum,
};
use kanal::{AsyncReceiver, AsyncSender};
use log::{error, info, warn};
use relm4::{
    gtk::{
        Align, AspectFrame, ContentFit, CssProvider, DrawingArea, EventController, EventControllerFocus,
        EventControllerKey, EventControllerMotion, EventControllerScroll, EventControllerScrollFlags, Fixed,
        GestureClick, GraphicsOffload, GraphicsOffloadEnabled, Label, Overlay, Picture,
        STYLE_PROVIDER_PRIORITY_APPLICATION, accelerator_get_label,
        gdk::{Cursor, Display, Key, MemoryFormat, MemoryTexture, ModifierType},
        glib::{Bytes, Object, Propagation, object::Cast, translate::IntoGlib},
        prelude::*,
        style_context_add_provider_for_display,
    },
    prelude::*,
};
use std::{fmt, num::NonZeroU32, time::Duration, vec::Vec};
use tokio::{select, time::sleep};

const INCH_TO_MM: f64 = 25.4;
const WINDOWS_DEFAULT_DPI: f64 = 96.;
const DEFAULT_MM_PER_PIXEL: f64 = INCH_TO_MM / WINDOWS_DEFAULT_DPI;

/// Configurable keyboard shortcut to release mouse capture
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrabShortcut {
    pub mask: ModifierType,
    pub key: Key,
}

impl Default for GrabShortcut {
    fn default() -> Self { Self { mask: ModifierType::CONTROL_MASK | ModifierType::ALT_MASK, key: Key::g } }
}

impl fmt::Display for GrabShortcut {
    /// Format shortcut for display using GTK's native accelerator labeler
    /// Supports cross-platform display (e.g., "⌃⌥G" on macOS, "Ctrl+Alt+G" on Linux)
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = accelerator_get_label(self.key, self.mask);
        write!(f, "{label}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalingMode {
    ResizeGuest, //虚拟机内画面会根据宿主窗口调整
    FixedGuest,  //虚拟机遵从自己的缩放设置,宿主只负责缩放当前画面
}

#[derive(Debug, Clone, Copy)]
pub struct ResizeCommand {
    pub w: NonZeroU32,
    pub h: NonZeroU32,
    pub mm_per_pixel: f64,
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
    ToggleCapture(bool),
    HideCaptureHint,
    UpdateCaptureView,
    CheckCaptureState,
}

pub struct VmDisplayModel {
    pub screen: Screen,
    pub changes: UpdateFlags,
    mouse_ctrl: MouseController,
    keyboard_ctrl: KeyboardController,
    canvas_size: (f64, f64),
    last_sent_mouse: Option<(u32, u32)>,
    scaling_mode: ScalingMode,
    mm_per_pixel: f64,
    resize_tx: AsyncSender<ResizeCommand>,
    scroll_acc_y: f64,
    grab_shortcut: GrabShortcut,
    is_captured: bool,
    hint_visible: bool,
    hint_timer: Option<tokio::task::JoinHandle<()>>,
}

pub struct VmDisplayWidgets {
    pub aspect_frame: AspectFrame,
    pub view_stack: Overlay,
    pub vm_picture: Picture,
    pub input_plane: DrawingArea,
    pub cursor_layer: Fixed,
    pub cursor_picture: Picture,
    pub controllers: Box<[EventController]> ,
    pub capture_hint: Label,
    pub invisible_cursor: Cursor,
}

pub struct VmDisplayInit {
    pub rx: AsyncReceiver<QemuEvent>,
    pub console_ctrl: ConsoleController,
    pub mouse_ctrl: MouseController,
    pub keyboard_ctrl: KeyboardController,
    pub grab_shortcut: GrabShortcut,
}

/// debounce 高频的窗口变更消息，转发给终端控制器
fn spawn_resize_debouncer(rx: AsyncReceiver<ResizeCommand>, console: ConsoleController) {
    relm4::spawn(async move {
        const DEBOUNCE_MS: Duration = Duration::from_millis(200);
        let mut latest_cmd: Option<ResizeCommand> = None;
        loop {
            let recv_fut = rx.recv();
            if let Some(cmd) = latest_cmd {
                select! {
                    Ok(new_cmd) = recv_fut => {
                        latest_cmd = Some(new_cmd);
                    }
                    _ = sleep(DEBOUNCE_MS) => {
                        let w = cmd.w.get();
                        let h = cmd.h.get();
                        let mm_per_pixel = cmd.mm_per_pixel;
                        let w_mm = (w as f64 * mm_per_pixel) as u16;
                        let h_mm = (h as f64 * mm_per_pixel) as u16;
                        info!("Resize debounced: {}x{} ({}mm x {}mm)", w, h, w_mm, h_mm);
                        if let Err(e) = console.set_ui_info(w_mm, h_mm, 0, 0, w, h).await {
                            error!(error:? = e; "Failed to send resize command to console controller");
                        }
                        latest_cmd = None;
                    }
                }
            } else {
                if let Ok(cmd) = recv_fut.await {
                    latest_cmd = Some(cmd);
                } else {
                    break;
                }
            }
        }
    });
}

impl VmDisplayModel {
    fn reset_hint_timer(&mut self, sender: ComponentSender<Self>) {
        if let Some(handle) = self.hint_timer.take() {
            handle.abort();
        }
        self.hint_timer = Some(relm4::spawn(async move {
            sleep(Duration::from_secs(3)).await;
            sender.input(Message::HideCaptureHint);
        }));
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
        let css_provider = CssProvider::new();
        css_provider.load_from_string(include_str!("capture-hint.css"));
        if let Some(display) = &Display::default() {
            style_context_add_provider_for_display(display, &css_provider, STYLE_PROVIDER_PRIORITY_APPLICATION);
        }

        let (resize_tx, resize_rx) = kanal::bounded_async(8);
        spawn_resize_debouncer(resize_rx, init.console_ctrl.clone());
        let model = VmDisplayModel {
            screen: Screen::new(),
            changes: UpdateFlags::default(),
            mouse_ctrl: init.mouse_ctrl,
            keyboard_ctrl: init.keyboard_ctrl,
            canvas_size: (1., 1.),
            last_sent_mouse: None,
            scaling_mode: ScalingMode::ResizeGuest,
            mm_per_pixel: DEFAULT_MM_PER_PIXEL,
            resize_tx,
            scroll_acc_y: 0.,
            grab_shortcut: init.grab_shortcut,
            is_captured: false,
            hint_visible: false,
            hint_timer: None,
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
        let input_plane =
            DrawingArea::builder().focusable(true).focus_on_click(true).hexpand(true).vexpand(true).build();
        input_plane.set_content_width(0);
        input_plane.set_content_height(0);
        input_plane.set_draw_func(|_widget, _cr, _width, _height| {});

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
                sender_clone.input(Message::UpdateMonitorInfo { mm_per_pixel });
            }
        };

        let updater = update_monitor_info.clone();
        input_plane.connect_realize(move |widget| {
            updater(widget);
        });

        root.connect_realize(move |_root| {});
        let mut controllers = Vec::new();

        let motion = EventControllerMotion::new();
        let sender_clone = sender.clone();
        let input_plane_clone = input_plane.clone();
        motion.connect_motion(move |_, x, y| {
            let scale = input_plane_clone.scale_factor() as f64;
            sender_clone.input(Message::MouseMove { x: x * scale, y: y * scale });
        });
        input_plane.add_controller(motion.clone());
        controllers.push(motion.upcast());

        let leave_controller = EventControllerMotion::new();
        leave_controller.connect_leave(move |_| {
            info!("Mouse left window, retaining capture state.");
        });
        input_plane.add_controller(leave_controller.clone());
        controllers.push(leave_controller.upcast());

        let enter_controller = EventControllerMotion::new();
        let sender_clone = sender.clone();
        enter_controller.connect_enter(move |_, _, _| {
            sender_clone.input(Message::CheckCaptureState);
        });
        input_plane.add_controller(enter_controller.clone());
        controllers.push(enter_controller.upcast());

        let click = GestureClick::new();
        click.set_button(0);
        let sender_clone = sender.clone();
        let input_plane_clone = input_plane.clone();
        click.connect_pressed(move |gesture, _, _, _| {
            input_plane_clone.grab_focus();
            sender_clone.input(Message::ToggleCapture(true));
            sender_clone.input(Message::MouseButton { button: gesture.current_button(), pressed: true });
        });
        let sender_clone = sender.clone();
        click.connect_released(move |gesture, _, _, _| {
            sender_clone.input(Message::MouseButton { button: gesture.current_button(), pressed: false });
        });
        input_plane.add_controller(click.clone());
        controllers.push(click.upcast());

        let scroll = EventControllerScroll::new(EventControllerScrollFlags::VERTICAL);
        let sender_clone = sender.clone();
        scroll.connect_scroll(move |_, _dx, dy| {
            sender_clone.input(Message::Scroll { dy });
            Propagation::Proceed // 转发会将消息直接消费,我们其他控件还需要这个事件
        });
        input_plane.add_controller(scroll.clone());
        controllers.push(scroll.upcast());

        let key = EventControllerKey::new();

        let grab_shortcut = init.grab_shortcut;
        let sender_for_release = sender.clone();
        let sender_for_key = sender.clone();

        key.connect_key_pressed(move |_, keyval, keycode, modifiers| {
            let modifiers_match = modifiers.contains(grab_shortcut.mask);
            let key_match = keyval == grab_shortcut.key;

            if modifiers_match && key_match {
                sender_for_release.input(Message::ToggleCapture(false));
                return Propagation::Stop;
            }

            let keyval_raw: u32 = keyval.into_glib();
            sender_for_key.input(Message::Key { keyval: keyval_raw, keycode, pressed: true });
            Propagation::Stop
        });

        let sender_clone = sender.clone();
        key.connect_key_released(move |_, keyval, keycode, _| {
            let keyval_raw: u32 = keyval.into_glib();
            sender_clone.input(Message::Key { keyval: keyval_raw, keycode, pressed: false });
        });

        root.add_controller(key.clone());
        controllers.push(key.upcast());

        let focus = EventControllerFocus::new();
        root.add_controller(focus.clone());
        controllers.push(focus.upcast());
        offload.set_child(Some(&vm_picture));

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
            .label(format!("Press {} to release mouse", init.grab_shortcut))
            .halign(Align::Center)
            .valign(Align::Start)
            .margin_top(20)
            .css_classes(["toast-label", "toast-hidden"])
            .selectable(false)
            .can_target(false)
            .build();

        let invisible_cursor = {
            static TRANSPARENT_PIXEL: [u8; 4] = [0, 0, 0, 0];
            let bytes = Bytes::from_static(&TRANSPARENT_PIXEL);
            let texture = MemoryTexture::new(
                1,
                1,
                MemoryFormat::R8g8b8a8,
                &bytes,
                4
            );
            Cursor::from_texture(&texture, 0, 0, None)
        };

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
                    info!("HiDPI: scale={scale}, logical={width}x{height}, physical={phy_w}x{phy_h}");
                    sender.input(Message::CanvasResize(w, h));
                }
            }
        };

        let handler_clone = resize_handler.clone();
        input_plane.connect_resize(move |widget, _, _| handler_clone(widget));

        let handler_clone = resize_handler.clone();
        input_plane.connect_scale_factor_notify(move |widget| {
            info!("Scale factor changed to {}", widget.scale_factor());
            handler_clone(widget);
        });

        view_stack.set_child(Some(&offload));
        view_stack.add_overlay(&input_plane);
        view_stack.add_overlay(&cursor_layer);
        view_stack.add_overlay(&capture_hint);

        aspect_frame.set_child(Some(&view_stack));
        root.set_child(Some(&aspect_frame));

        relm4::spawn(async move {
            while let Ok(event) = init.rx.recv().await {
                sender.input(Message::Qemu(event));
            }
            warn!("VM display channel closed");
            sender.input(Message::Qemu(QemuEvent::Disable));
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
            invisible_cursor,
        };
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, _root: &Self::Root) {
        use Message::*;
        match msg {
            Qemu(event) => {
                if let Ok(flags) = self.screen.handle_event(event) {
                    self.changes.cursor |= flags.cursor;
                    self.changes.frame |= flags.frame;
                }
            }
            CanvasResize(w, h) => {
                self.canvas_size = (w.get() as f64, h.get() as f64);
                self.changes.cursor = true;
                if self.scaling_mode == ScalingMode::ResizeGuest
                    && let Err(e) = self.resize_tx.try_send(ResizeCommand { w, h, mm_per_pixel: self.mm_per_pixel })
                {
                    error!(error:? =e; "Failed to send resize command");
                }
            }
            MouseMove { x, y } => {
                if !self.is_captured {
                    return;
                }
                let (vm_w, vm_h) = self.screen.resolution();
                let (canvas_w, canvas_h) = self.canvas_size;
                if vm_w == 0 || vm_h == 0 || canvas_w <= 0. || canvas_h <= 0. {
                    return;
                }
                let raw_x = (x / canvas_w) * vm_w as f64;
                let raw_y = (y / canvas_h) * vm_h as f64;
                let max_x = (vm_w.saturating_sub(1)) as f64;
                let max_y = (vm_h.saturating_sub(1)) as f64;
                let clamped_x = raw_x.clamp(0., max_x);
                let clamped_y = raw_y.clamp(0., max_y);
                let target_x = clamped_x as u32;
                let target_y = clamped_y as u32;
                if self.last_sent_mouse != Some((target_x, target_y)) {
                    self.last_sent_mouse = Some((target_x, target_y));
                    if let Err(e) = self.mouse_ctrl.try_set_abs_position(target_x, target_y) {
                        error!(error:? = e; "Failed to set mouse position");
                    }
                }
            }
            ToggleCapture(should_capture) => {
                if self.is_captured == should_capture {
                    if should_capture {
                        self.reset_hint_timer(sender.clone());
                    }
                    return;
                }
                self.is_captured = should_capture;
                if should_capture {
                    info!("🖱️ Mouse captured - {} to release", self.grab_shortcut);
                    self.hint_visible = true;
                    self.reset_hint_timer(sender.clone());
                } else {
                    info!("🖱️ Mouse released - Click to capture");
                    self.hint_visible = false;
                    if let Some(handle) = self.hint_timer.take() {
                        handle.abort();
                    }
                }

                sender.input(Message::UpdateCaptureView);
            }
            CheckCaptureState => {
                if self.is_captured {
                    sender.input(Message::UpdateCaptureView);
                }
            }
            HideCaptureHint => {
                self.hint_visible = false;
                self.hint_timer = None;
                self.changes.cursor = true;
            }
            UpdateCaptureView => {
                self.changes.cursor = true;
            }
            Message::MouseButton { button, pressed } => {
                let Some(btn) = QemuButton::from_xorg(button) else {
                    warn!("Ignored unsupported X11 mouse button {button}: no mapping to QEMU protocol.");
                    return;
                };
                let ctrl = self.mouse_ctrl.clone();
                relm4::spawn(async move {
                    let res = if pressed {
                        ctrl.press(btn).await
                    } else {
                        ctrl.release(btn).await
                    };
                    if let Err(e) = res {
                        error!(error:? = e; "Failed to send mouse button event");
                    }
                });
            }
            Scroll { dy } => {
                if !self.is_captured {
                    return;
                }
                self.scroll_acc_y += dy;

                while self.scroll_acc_y.abs() >= 1. {
                    let btn = if self.scroll_acc_y > 0. {
                        QemuButton::WheelDown
                    } else {
                        QemuButton::WheelUp
                    };
                    let ctrl = self.mouse_ctrl.clone();
                    relm4::spawn(async move {
                        if let Err(e) = ctrl.press(btn).await {
                            error!(error:? = e; "Failed to send scroll press");
                        }
                        if let Err(e) = ctrl.release(btn).await {
                            error!(error:? = e; "Failed to send scroll release");
                        }
                    });
                    self.scroll_acc_y -= if self.scroll_acc_y > 0. { 1. } else { -1. };
                }
            }
            Key { keyval: _, keycode, pressed } => {
                if !self.is_captured {
                    return;
                }
                let qnum = Qnum::from_xorg_keycode(keycode);
                let ctrl = self.keyboard_ctrl.clone();
                relm4::spawn(async move {
                    let res = if pressed {
                        ctrl.press(qnum).await
                    } else {
                        ctrl.release(qnum).await
                    };
                    if let Err(e) = res {
                        error!(error:? = e; "Failed to send keyboard event");
                    }
                });
            }
            UpdateMonitorInfo { mm_per_pixel } => {
                self.mm_per_pixel = mm_per_pixel;
                info!(
                    "Updated monitor DPI info: {:.6} mm/px (approx {:.1} DPI)",
                    mm_per_pixel,
                    INCH_TO_MM / mm_per_pixel
                );
            }
            SetScalingMode(mode) => {
                self.scaling_mode = mode;
                info!("Scaling mode set to: {:?}", mode);
                if mode == ScalingMode::ResizeGuest {
                    let (w, h) = self.canvas_size;
                    if w.floor() >= u32::MAX as f64 || h.floor() >= u32::MAX as f64 {
                        error!("Canvas size is too large");
                        return;
                    }
                    if let (Some(w_nz), Some(h_nz)) = (NonZeroU32::new(w as u32), NonZeroU32::new(h as u32))
                        && let Err(e) =
                            self.resize_tx.try_send(ResizeCommand { w: w_nz, h: h_nz, mm_per_pixel: self.mm_per_pixel })
                    {
                        error!(error:? =e; "Failed to send resize command");
                    }
                }
            }
        }
    }

    fn update_view(&self, widgets: &mut Self::Widgets, _sender: ComponentSender<Self>) {
        if self.changes.frame {
            if let Some(new_texture) = self.screen.get_background_texture() {
                let new_tex_ptr = new_texture.clone().upcast::<Object>().as_ptr();
                let should_update =
                    widgets.vm_picture.paintable().is_none_or(|p| p.upcast::<Object>().as_ptr() != new_tex_ptr);
                if should_update {
                    widgets.vm_picture.set_paintable(Some(new_texture));
                }
            } else if widgets.vm_picture.paintable().is_some() {
                widgets.vm_picture.set_paintable(None::<&gtk::gdk::Texture>);
            }
        }
        if self.hint_visible {
            widgets.capture_hint.add_css_class("toast-visible");
            widgets.capture_hint.remove_css_class("toast-hidden");
        } else {
            widgets.capture_hint.add_css_class("toast-hidden");
            widgets.capture_hint.remove_css_class("toast-visible");
        }
        if self.is_captured {
            widgets.input_plane.set_cursor(Some(&widgets.invisible_cursor));
        } else {
            widgets.input_plane.set_cursor(None);
        }
        if !self.changes.any() {
            return;
        }
        let ui_scale = widgets.input_plane.scale_factor() as f64;
        let (vm_w, vm_h) = self.screen.resolution();
        let (canvas_w, canvas_h) = self.canvas_size;
        if vm_w == 0 || vm_h == 0 || canvas_w <= 0. || canvas_h <= 0. {
            return;
        }
        widgets.aspect_frame.set_ratio(vm_w as f32 / vm_h as f32);
        let cursor = &self.screen.cursor;
        widgets.cursor_picture.set_visible(cursor.visible);
        if !cursor.visible {
            if widgets.cursor_picture.paintable().is_some() {
                widgets.cursor_picture.set_paintable(None::<&gtk::gdk::Texture>);
            }
            return;
        }
        let scale_x = canvas_w / vm_w as f64;
        let scale_y = canvas_h / vm_h as f64;
        let scale = scale_x.min(scale_y);
        let final_x_phy = (cursor.x - cursor.hot_x) as f64 * scale;
        let final_y_phy = (cursor.y - cursor.hot_y) as f64 * scale;
        widgets.cursor_layer.move_(&widgets.cursor_picture, final_x_phy / ui_scale, final_y_phy / ui_scale);
        let Some(tex) = &cursor.texture else { return };
        let tex_obj = tex.clone().upcast::<Object>();
        let tex_ptr = tex_obj.as_ptr();
        let should_update_texture =
            widgets.cursor_picture.paintable().is_none_or(|p| p.upcast::<Object>().as_ptr() != tex_ptr);
        if should_update_texture {
            widgets.cursor_picture.set_paintable(Some(tex));
        }
        let cursor_w_phy = tex.width() as f64 * scale;
        let cursor_h_phy = tex.height() as f64 * scale;
        let cursor_w_logical = (cursor_w_phy / ui_scale).ceil() as i32;
        let cursor_h_logical = (cursor_h_phy / ui_scale).ceil() as i32;
        if widgets.cursor_picture.width_request() != cursor_w_logical
            || widgets.cursor_picture.height_request() != cursor_h_logical
        {
            widgets.cursor_picture.set_width_request(cursor_w_logical);
            widgets.cursor_picture.set_height_request(cursor_h_logical);
        }
    }
}
