use crate::{
    dbus::{
        console::ConsoleController,
        keyboard::KeyboardController,
        listener::QemuEvent,
        mouse::{Button as QemuButton, MouseController},
    },
    display::screen::{Screen, UpdateFlags},
};
use kanal::AsyncReceiver;
use log::{debug, info, warn};
use relm4::{
    gtk::{
        self, ContentFit, EventControllerFocus, EventControllerKey, EventControllerMotion, EventControllerScroll,
        EventControllerScrollFlags, Fixed, GestureClick, GraphicsOffload, GraphicsOffloadEnabled, Overlay, Picture,
        glib, prelude::*,
    },
    prelude::*,
};

// ==========================================
// 1. Messages
// ==========================================
#[derive(Debug)]
pub enum DisplayMsg {
    Qemu(QemuEvent),
    Resize(i32, i32),
    MouseMove { x: f64, y: f64 },
    MouseButton { button: u32, pressed: bool },
    Scroll { dx: f64, dy: f64 },
    Key { keyval: u32, keycode: u32, pressed: bool },
}

// ==========================================
// 2. Model & Init
// ==========================================
pub struct VmDisplayModel {
    pub screen: Screen,
    pub changes: UpdateFlags,
    console_ctrl: ConsoleController,
    mouse_ctrl: MouseController,
    keyboard_ctrl: KeyboardController,
    widget_size: (f64, f64),
    last_sent_mouse: Option<(u32, u32)>,
}

pub struct VmDisplayWidgets {
    pub vm_picture: Picture,
    pub cursor_layer: Fixed,
    pub cursor_picture: Picture,
    pub controllers: Vec<gtk::EventController>,
}

pub struct VmDisplayInit {
    pub rx: AsyncReceiver<QemuEvent>,
    pub console_ctrl: ConsoleController,
    pub mouse_ctrl: MouseController,
    pub keyboard_ctrl: KeyboardController,
}

impl Component for VmDisplayModel {
    type CommandOutput = ();
    type Init = VmDisplayInit;
    type Input = DisplayMsg;
    type Output = ();
    type Root = Overlay;
    type Widgets = VmDisplayWidgets;

    fn init_root() -> Self::Root {
        Overlay::builder().hexpand(true).vexpand(true).focusable(true).can_focus(true).build()
    }

    fn init(init: Self::Init, root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let model = VmDisplayModel {
            screen: Screen::new(),
            changes: UpdateFlags::default(),
            console_ctrl: init.console_ctrl,
            mouse_ctrl: init.mouse_ctrl,
            keyboard_ctrl: init.keyboard_ctrl,
            widget_size: (0.0, 0.0),
            last_sent_mouse: None,
        };

        let offload = GraphicsOffload::builder().enabled(GraphicsOffloadEnabled::Enabled).build();
        
        // Fix: Force Picture to fill available space even without content
        // This prevents widget size from being 0x0 during initialization
        let vm_picture = Picture::builder()
            .can_shrink(true)
            .content_fit(ContentFit::Contain)
            .hexpand(true)
            .vexpand(true)
            .build();

        let mut controllers = Vec::new();

        // Fix: Monitor root widget allocation changes
        // Use "allocation" not "surface" (surface only exists on Window/Native widgets)
        let sender_clone = sender.clone();
        root.connect_realize(move |root| {
            let alloc = root.allocation();
            #[allow(deprecated)]
            sender_clone.input(DisplayMsg::Resize(alloc.width(), alloc.height()));
        });
        
        // Also monitor for size changes after realization
        let sender_clone = sender.clone();
        root.connect_notify_local(Some("allocation"), move |obj, _| {
            // Allocation changes when widget is resized
            let alloc = obj.allocation();
            #[allow(deprecated)]
            sender_clone.input(DisplayMsg::Resize(alloc.width(), alloc.height()));
        });

        // =========================================================
        // CRITICAL FIX: Attach ALL input controllers to root instead of vm_picture
        // This ensures mouse coordinates are in the same coordinate system
        // as our size calculations (root.allocation), fixing coordinate mismatch
        // =========================================================

        // Motion (attached to root)
        let motion = EventControllerMotion::new();
        let sender_clone = sender.clone();
        motion.connect_motion(move |_, x, y| {
            sender_clone.input(DisplayMsg::MouseMove { x, y });
        });
        root.add_controller(motion.clone());
        controllers.push(motion.upcast());

        // Click + Focus (attached to root)
        let click = GestureClick::new();
        click.set_button(0);
        let sender_clone = sender.clone();
        let root_clone = root.clone(); 
        click.connect_pressed(move |gesture, _, _, _| {
            root_clone.grab_focus();
            sender_clone.input(DisplayMsg::MouseButton { button: gesture.current_button(), pressed: true });
        });
        let sender_clone = sender.clone();
        click.connect_released(move |gesture, _, _, _| {
            sender_clone.input(DisplayMsg::MouseButton { button: gesture.current_button(), pressed: false });
        });
        root.add_controller(click.clone());
        controllers.push(click.upcast());

        // Scroll (attached to root)
        let scroll = EventControllerScroll::new(EventControllerScrollFlags::BOTH_AXES);
        let sender_clone = sender.clone();
        scroll.connect_scroll(move |_, dx, dy| {
            sender_clone.input(DisplayMsg::Scroll { dx, dy });
            glib::Propagation::Proceed
        });
        root.add_controller(scroll.clone());
        controllers.push(scroll.upcast());

        let key = EventControllerKey::new();
        let sender_clone = sender.clone();
        key.connect_key_pressed(move |_, keyval, keycode, _| {
            let keyval_raw: u32 = unsafe { std::mem::transmute(keyval) };
            sender_clone.input(DisplayMsg::Key { keyval: keyval_raw, keycode, pressed: true });
            glib::Propagation::Proceed
        });
        let sender_clone = sender.clone();
        key.connect_key_released(move |_, keyval, keycode, _| {
            let keyval_raw: u32 = unsafe { std::mem::transmute(keyval) };
            sender_clone.input(DisplayMsg::Key { keyval: keyval_raw, keycode, pressed: false });
        });
        root.add_controller(key.clone());
        controllers.push(key.upcast());

        let focus = EventControllerFocus::new();
        focus.connect_leave(move |_| {
            debug!("VM Display lost focus");
        });
        root.add_controller(focus.clone());
        controllers.push(focus.upcast());

        offload.set_child(Some(&vm_picture));
        root.add_overlay(&offload);

        let cursor_layer = Fixed::builder().can_target(false).hexpand(true).vexpand(true).build();
        let cursor_picture = Picture::builder().can_shrink(true).content_fit(ContentFit::Fill).build();
        cursor_layer.put(&cursor_picture, 0.0, 0.0);
        root.add_overlay(&cursor_layer);

        relm4::spawn(async move {
            while let Ok(event) = init.rx.recv().await {
                sender.input(DisplayMsg::Qemu(event));
            }
            warn!("VM display channel closed");
            sender.input(DisplayMsg::Qemu(QemuEvent::Disable));
        });

        let widgets = VmDisplayWidgets { vm_picture, cursor_layer, cursor_picture, controllers };
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            DisplayMsg::Qemu(event) => {
                if let Ok(flags) = self.screen.handle_event(event) {
                    // IMPORTANT: Use bitwise OR accumulation (|=) not assignment (=)
                    //
                    // Why: QEMU sends CursorDefine and Scanout events almost simultaneously.
                    // CursorDefine sets cursor flag, Scanout sets frame flag.
                    // If we use assignment (=), the second event overwrites the first,
                    // causing the cursor to never be rendered (texture exists but not applied).
                    //
                    // With accumulation (|=), both flags are preserved until update_view consumes them.
                    // This ensures the cursor is always visible after initial definition.
                    self.changes.cursor |= flags.cursor;
                    self.changes.frame |= flags.frame;
                }
            }

            DisplayMsg::Resize(w, h) => {
                // Diagnostic: Log ALL resize events to verify they're arriving
                static RESIZE_COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
                let count = RESIZE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if count < 3 {
                    info!("[UI] Resize Event #{}: {}x{}", count + 1, w, h);
                }

                self.changes.cursor = true;
                self.changes.frame = true;
                self.widget_size = (w as f64, h as f64);

                if w > 0 && h > 0 {
                    let console = self.console_ctrl.clone();
                    let w_mm = (w as f64 * 0.264) as u16;
                    let h_mm = (h as f64 * 0.264) as u16;
                    relm4::spawn(async move {
                        if let Err(e) = console.set_ui_info(w_mm, h_mm, 0, 0, w as u32, h as u32).await {
                            warn!("Failed to set UI info: {}", e);
                        }
                    });
                }
            }

            DisplayMsg::MouseMove { x, y } => {
                let (vm_w, vm_h) = self.screen.resolution();
                let (w, h) = self.widget_size;

                // WORKAROUND: If widget size is 0x0, try to get it from root
                // This handles the case where resize events haven't fired yet
                let (w, h) = if (w <= 0.0 || h <= 0.0) && vm_w > 0 && vm_h > 0 {
                    // Get size from root widget via the overlay reference
                    // We need to pass root to update, but we don't have it here
                    // For now, just use VM resolution as fallback
                    info!("[UI] Widget size not set yet, using VM resolution {}x{} as fallback", vm_w, vm_h);
                    (vm_w as f64, vm_h as f64)
                } else {
                    (w, h)
                };

                // Diagnostic: Identify why mouse moves are being dropped
                if vm_w == 0 || vm_h == 0 {
                    static VM_WARN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
                    if VM_WARN.load(std::sync::atomic::Ordering::Relaxed) && w > 0.0 {
                        warn!("[UI] DROP Move: VM Resolution is 0x0 (UI ready but no frame yet)");
                        VM_WARN.store(false, std::sync::atomic::Ordering::Relaxed);
                    }
                    return;
                }
                if w <= 0.0 || h <= 0.0 {
                    static WIDGET_WARN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
                    if WIDGET_WARN.load(std::sync::atomic::Ordering::Relaxed) {
                        warn!("[UI] DROP Move: Widget size is {:.0}x{:.0} (Picture not allocated yet)", w, h);
                        WIDGET_WARN.store(false, std::sync::atomic::Ordering::Relaxed);
                    }
                    return;
                }

                let scale = (w / vm_w as f64).min(h / vm_h as f64);
                let drawn_w = vm_w as f64 * scale;
                let drawn_h = vm_h as f64 * scale;
                let offset_x = (w - drawn_w) / 2.0;
                let offset_y = (h - drawn_h) / 2.0;

                let vm_input_x = (x - offset_x) / scale;
                let vm_input_y = (y - offset_y) / scale;

                if vm_input_x >= 0.0 && vm_input_x < vm_w as f64 && vm_input_y >= 0.0 && vm_input_y < vm_h as f64 {
                    let target_x = vm_input_x as u32;
                    let target_y = vm_input_y as u32;

                    if self.last_sent_mouse != Some((target_x, target_y)) {
                        self.last_sent_mouse = Some((target_x, target_y));

                        let mouse = self.mouse_ctrl.clone();
                        relm4::spawn(async move {
                            if let Err(e) = mouse.set_abs_position(target_x, target_y).await {
                                warn!("Failed to set mouse position: {}", e);
                            }
                        });
                    }
                } else {
                    // Diagnostic: Mouse is in the black bars (letterbox/pillarbox area)
                    static BOUNDS_WARN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
                    if BOUNDS_WARN.load(std::sync::atomic::Ordering::Relaxed) {
                        debug!("[UI] DROP Move: Out of bounds ({:.1}, {:.1}) - in black bars", vm_input_x, vm_input_y);
                        debug!("[UI] VM resolution: {}x{}, Drawn area: {:.0}x{:.0} at offset ({:.1}, {:.1})",
                               vm_w, vm_h, drawn_w, drawn_h, offset_x, offset_y);
                        BOUNDS_WARN.store(false, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }

            DisplayMsg::MouseButton { button, pressed } => {
                let mouse = self.mouse_ctrl.clone();
                let btn = match button {
                    1 => Some(QemuButton::Left),
                    2 => Some(QemuButton::Middle),
                    3 => Some(QemuButton::Right),
                    _ => None,
                };
                if let Some(b) = btn {
                    relm4::spawn(async move {
                        let result = if pressed {
                            mouse.press(b).await
                        } else {
                            mouse.release(b).await
                        };
                        if let Err(e) = result {
                            warn!("Failed to send mouse button event: {}", e);
                        }
                    });
                }
            }

            DisplayMsg::Scroll { dx: _, dy } => {
                let mouse = self.mouse_ctrl.clone();
                if dy.abs() > 0.1 {
                    relm4::spawn(async move {
                        let btn = if dy > 0.0 {
                            QemuButton::WheelDown
                        } else {
                            QemuButton::WheelUp
                        };
                        if let Err(e) = mouse.press(btn).await {
                            warn!("Failed to send scroll press: {}", e);
                        }
                        if let Err(e) = mouse.release(btn).await {
                            warn!("Failed to send scroll release: {}", e);
                        }
                    });
                }
            }

            DisplayMsg::Key { keyval, keycode, pressed } => {
                let kbd = self.keyboard_ctrl.clone();

                const Q_KEY_RET: u32 = 0x1c;
                const Q_KEY_ESC: u32 = 0x01;
                const Q_KEY_BACKSPACE: u32 = 0x0e;
                const Q_KEY_SPACE: u32 = 0x39;

                let qcode = match keyval {
                    0xFF0D => Q_KEY_RET,
                    0xFF1B => Q_KEY_ESC,
                    0xFF08 => Q_KEY_BACKSPACE,
                    0x020 => Q_KEY_SPACE,
                    _ => keycode.saturating_sub(8),
                };

                relm4::spawn(async move {
                    let result = if pressed {
                        kbd.press(qcode).await
                    } else {
                        kbd.release(qcode).await
                    };
                    if let Err(e) = result {
                        warn!("Failed to send keyboard event: {}", e);
                    }
                });
            }
        }
    }

    fn update_view(&self, widgets: &mut Self::Widgets, _sender: ComponentSender<Self>) {
        if self.changes.frame {
            if let Some(texture) = self.screen.get_background_texture() {
                widgets.vm_picture.set_paintable(Some(texture));
            } else {
                widgets.vm_picture.set_paintable(None::<&gtk::gdk::Texture>);
            }
        }

        if self.changes.cursor || self.changes.frame {
            let cursor = &self.screen.cursor;
            widgets.cursor_picture.set_visible(cursor.visible);
            if cursor.visible {
                let widget_w = widgets.vm_picture.width() as f64;
                let widget_h = widgets.vm_picture.height() as f64;
                let (vm_w, vm_h) = self.screen.resolution();
                if vm_w != 0 && vm_h != 0 {
                    let vm_w = vm_w as f64;
                    let vm_h = vm_h as f64;
                    let scale_x = widget_w / vm_w;
                    let scale_y = widget_h / vm_h;
                    let scale = scale_x.min(scale_y);
                    let drawn_w = vm_w * scale;
                    let drawn_h = vm_h * scale;
                    let offset_x = (widget_w - drawn_w) / 2.;
                    let offset_y = (widget_h - drawn_h) / 2.;
                    let final_x = offset_x + ((cursor.x - cursor.hot_x) as f64 * scale);
                    let final_y = offset_y + ((cursor.y - cursor.hot_y) as f64 * scale);
                    widgets.cursor_layer.move_(&widgets.cursor_picture, final_x, final_y);
                    if let Some(tex) = &cursor.texture {
                        widgets.cursor_picture.set_paintable(Some(tex));
                        let cursor_w = (tex.width() as f64 * scale).ceil() as i32;
                        widgets.cursor_picture.set_width_request(cursor_w);
                        let cursor_h = (tex.height() as f64 * scale).ceil() as i32;
                        widgets.cursor_picture.set_height_request(cursor_h);
                    }
                } else {
                    let x = (cursor.x - cursor.hot_x) as f64;
                    let y = (cursor.y - cursor.hot_y) as f64;
                    widgets.cursor_layer.move_(&widgets.cursor_picture, x, y);
                    if let Some(tex) = &cursor.texture {
                        widgets.cursor_picture.set_paintable(Some(tex));
                    }
                }
            }
        }
    }
}
