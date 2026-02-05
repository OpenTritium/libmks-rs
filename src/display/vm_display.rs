use crate::{
    dbus::{
        console::ConsoleController,
        keyboard::KeyboardController,
        listener::QemuEvent,
        mouse::{Button as QemuButton, MouseController},
    },
    display::screen::{Screen, UpdateFlags},
    keymaps::xorg_keycode_to_qnum,
};
use kanal::AsyncReceiver;
use log::warn;
use num_enum::TryFromPrimitive;
use relm4::{
    gtk::{
        self, Align, AspectFrame, ContentFit, DrawingArea, EventController, EventControllerFocus, EventControllerKey,
        EventControllerMotion, EventControllerScroll, EventControllerScrollFlags, GestureClick, GraphicsOffload,
        GraphicsOffloadEnabled, Overlay, Picture,
        glib::{Propagation, translate::IntoGlib},
        prelude::*,
    },
    prelude::*,
};

#[derive(Debug)]
pub enum DisplayMsg {
    Qemu(QemuEvent),
    CanvasResize(i32, i32), // (width, height)
    MouseMove { x: f64, y: f64 },
    MouseButton { button: u32, pressed: bool },
    Scroll { dx: f64, dy: f64 }, // dx: horizontal scroll, dy: vertical scroll
    Key { keyval: u32, keycode: u32, pressed: bool },
}

pub struct VmDisplayModel {
    pub screen: Screen,
    pub changes: UpdateFlags,
    console_ctrl: ConsoleController,
    mouse_ctrl: MouseController,
    keyboard_ctrl: KeyboardController,
    canvas_size: (f64, f64),             // (width, height)
    last_sent_mouse: Option<(u32, u32)>, // (x, y)
}

pub struct VmDisplayWidgets {
    pub aspect_frame: AspectFrame,
    pub view_stack: Overlay,
    pub vm_picture: Picture,
    pub input_plane: DrawingArea,
    pub cursor_picture: Picture,
    pub controllers: Box<[EventController]>,
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

    #[inline]
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
            canvas_size: (1., 1.),
            last_sent_mouse: None,
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
        // 保护性初始化
        input_plane.set_content_width(0);
        input_plane.set_content_height(0);
        input_plane.set_draw_func(|_widget, _cr, _width, _height| {});
        root.connect_realize(move |_root| {});
        let mut controllers = Vec::new();

        let motion = EventControllerMotion::new();
        let sender_clone = sender.clone();
        motion.connect_motion(move |_, x, y| {
            sender_clone.input(DisplayMsg::MouseMove { x, y });
        });
        input_plane.add_controller(motion.clone());
        controllers.push(motion.upcast());

        let click = GestureClick::new();
        click.set_button(0); // 捕获所有按键
        let sender_clone = sender.clone();
        let input_plane_clone = input_plane.clone();
        click.connect_pressed(move |gesture, _, _, _| {
            input_plane_clone.grab_focus();
            sender_clone.input(DisplayMsg::MouseButton { button: gesture.current_button(), pressed: true });
        });
        let sender_clone = sender.clone();
        click.connect_released(move |gesture, _, _, _| {
            sender_clone.input(DisplayMsg::MouseButton { button: gesture.current_button(), pressed: false });
        });
        input_plane.add_controller(click.clone());
        controllers.push(click.upcast());

        let scroll = EventControllerScroll::new(EventControllerScrollFlags::BOTH_AXES);
        let sender_clone = sender.clone();
        scroll.connect_scroll(move |_, dx, dy| {
            sender_clone.input(DisplayMsg::Scroll { dx, dy });
            Propagation::Proceed // 转发会将消息直接消费,我们其他控件还需要这个事件
        });
        input_plane.add_controller(scroll.clone());
        controllers.push(scroll.upcast());

        let key = EventControllerKey::new();
        let sender_clone = sender.clone();
        key.connect_key_pressed(move |_, keyval, keycode, _| {
            let keyval_raw: u32 = keyval.into_glib();
            sender_clone.input(DisplayMsg::Key { keyval: keyval_raw, keycode, pressed: true });
            Propagation::Proceed
        });
        let sender_clone = sender.clone();
        key.connect_key_released(move |_, keyval, keycode, _| {
            let keyval_raw: u32 = keyval.into_glib();
            sender_clone.input(DisplayMsg::Key { keyval: keyval_raw, keycode, pressed: false });
        });
        root.add_controller(key.clone());
        controllers.push(key.upcast());

        let focus = EventControllerFocus::new();
        root.add_controller(focus.clone());
        controllers.push(focus.upcast());
        offload.set_child(Some(&vm_picture));

        let cursor_picture = Picture::builder()
            .can_shrink(true)
            .content_fit(ContentFit::Fill)
            .can_target(false)
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Start)
            .build();

        let sender_clone = sender.clone();
        input_plane.connect_resize(move |_widget, width, height| {
            if width > 0 && height > 0 {
                sender_clone.input(DisplayMsg::CanvasResize(width, height));
            }
        });

        view_stack.set_child(Some(&offload));
        view_stack.add_overlay(&cursor_picture);
        view_stack.add_overlay(&input_plane);

        aspect_frame.set_child(Some(&view_stack));
        root.set_child(Some(&aspect_frame));

        relm4::spawn(async move {
            while let Ok(event) = init.rx.recv().await {
                sender.input(DisplayMsg::Qemu(event));
            }
            warn!("VM display channel closed");
            sender.input(DisplayMsg::Qemu(QemuEvent::Disable));
        });
        let controllers = controllers.into_boxed_slice();
        let widgets =
            VmDisplayWidgets { aspect_frame, view_stack, vm_picture, input_plane, cursor_picture, controllers };
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>, _root: &Self::Root) {
        match msg {
            DisplayMsg::Qemu(event) => {
                if let Ok(flags) = self.screen.handle_event(event) {
                    self.changes.cursor |= flags.cursor;
                    self.changes.frame |= flags.frame;
                }
            }

            DisplayMsg::CanvasResize(w, h) => {
                if w > 0 && h > 0 {
                    self.canvas_size = (w as f64, h as f64);
                    self.changes.cursor = true;
                    let console = self.console_ctrl.clone();
                    let w_mm = (w as f64 * 0.264) as u16;
                    let h_mm = (h as f64 * 0.264) as u16;
                    relm4::spawn(async move {
                        if let Err(e) = console.set_ui_info(w_mm, h_mm, 0, 0, w as u32, h as u32).await {
                            log::error!("Failed to set UI info: {e}");
                        }
                    });
                }
            }

            DisplayMsg::MouseMove { x, y } => {
                let (vm_w, vm_h) = self.screen.resolution();
                let (canvas_w, canvas_h) = self.canvas_size;
                if vm_w == 0 || vm_h == 0 || canvas_w <= 0.0 || canvas_h <= 0.0 {
                    return;
                }
                let vm_input_x = (x / canvas_w) * vm_w as f64;
                let vm_input_y = (y / canvas_h) * vm_h as f64;
                let target_x = vm_input_x as u32;
                let target_y = vm_input_y as u32;
                if vm_input_x >= 0.0
                    && vm_input_x < vm_w as f64
                    && vm_input_y >= 0.0
                    && vm_input_y < vm_h as f64
                    && self.last_sent_mouse != Some((target_x, target_y))
                {
                    self.last_sent_mouse = Some((target_x, target_y));
                    let mouse = self.mouse_ctrl.clone();
                    relm4::spawn(async move {
                        mouse.set_abs_position(target_x, target_y).await.ok();
                    });
                }
            }

            DisplayMsg::MouseButton { button, pressed } => {
                let mouse = self.mouse_ctrl.clone();

                let qemu_button_idx = button.saturating_sub(1);
                if let Ok(btn) = QemuButton::try_from_primitive(qemu_button_idx) {
                    relm4::spawn(async move {
                        let result = if pressed {
                            mouse.press(btn).await
                        } else {
                            mouse.release(btn).await
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
                    let btn = if dy > 0.0 {
                        QemuButton::WheelDown
                    } else {
                        QemuButton::WheelUp
                    };
                    relm4::spawn(async move {
                        if let Err(e) = mouse.press(btn).await {
                            warn!("Failed to send scroll press: {}", e);
                        }
                        if let Err(e) = mouse.release(btn).await {
                            warn!("Failed to send scroll release: {}", e);
                        }
                    });
                }
            }

            DisplayMsg::Key { keyval: _, keycode, pressed } => {
                let kbd = self.keyboard_ctrl.clone();

                let qcode = xorg_keycode_to_qnum(keycode);

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

        if self.changes.frame || self.changes.cursor {
            let (vm_w, vm_h) = self.screen.resolution();
            let (canvas_w, canvas_h) = self.canvas_size;

            if vm_w != 0 && vm_h != 0 && canvas_w > 0.0 && canvas_h > 0.0 {
                widgets.aspect_frame.set_ratio(vm_w as f32 / vm_h as f32);

                let cursor = &self.screen.cursor;
                widgets.cursor_picture.set_visible(cursor.visible);

                if cursor.visible {
                    let scale_x = canvas_w / vm_w as f64;
                    let scale_y = canvas_h / vm_h as f64;
                    let scale = scale_x.min(scale_y);

                    let final_x = (cursor.x - cursor.hot_x) as f64 * scale;
                    let final_y = (cursor.y - cursor.hot_y) as f64 * scale;

                    widgets.cursor_picture.set_margin_start(final_x as i32);
                    widgets.cursor_picture.set_margin_top(final_y as i32);

                    if let Some(tex) = &cursor.texture {
                        widgets.cursor_picture.set_paintable(Some(tex));
                        let cursor_w = (tex.width() as f64 * scale).ceil() as i32;
                        let cursor_h = (tex.height() as f64 * scale).ceil() as i32;
                        widgets.cursor_picture.set_width_request(cursor_w);
                        widgets.cursor_picture.set_height_request(cursor_h);
                    }
                }
            }
        }
    }
}
