//! Interactive demo for scaling and input mode toggles.
use kanal::AsyncSender;
use libmks_rs::{
    dbus::{
        console::{self, ConsoleController},
        listener::Event,
    },
    display::{
        input_daemon::InputCommand,
        input_handler::{Capability, InputHandler},
        vm_display::{GrabShortcut, InputMode, ScalingMode, VmDisplayInit, VmDisplayModel},
    },
};
use log::info;
use relm4::{Controller, gtk::prelude::*, prelude::*};
use std::{num::NonZeroU32, thread, time::Duration};
use tokio::sync::watch;

struct AppModel {
    display: Controller<VmDisplayModel>,
}

#[derive(Debug)]
enum AppMsg {
    SetScalingMode(ScalingMode),
    SetInputMode(InputMode),
    Ignore,
}

#[relm4::component]
impl SimpleComponent for AppModel {
    type Init = ();
    type Input = AppMsg;
    type Output = ();

    view! {
        gtk::Window {
            set_title: Some("VM Display Interactive Demo"),
            set_default_width: 1024,
            set_default_height: 768,

            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,

                gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_spacing: 10,
                    set_margin_all: 10,

                    gtk::Label { set_label: "Scaling:" },
                    #[name = "scale_dropdown"]
                    gtk::DropDown {
                        set_model: Some(&gtk::StringList::new(&[
                            "Resize Guest (Auto)",
                            "Fixed Guest (Scaled)",
                        ])),
                        set_selected: 0,
                    },

                    gtk::Separator { set_orientation: gtk::Orientation::Vertical },

                    gtk::Label { set_label: "Input:" },
                    #[name = "input_dropdown"]
                    gtk::DropDown {
                        set_model: Some(&gtk::StringList::new(&[
                            "Seamless",
                            "Confined",
                        ])),
                        set_selected: 0,
                    },
                },

                #[local_ref]
                display_widget -> gtk::Overlay {
                    set_hexpand: true,
                    set_vexpand: true,
                },
            },
        }
    }

    fn init(_: (), _root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let (tx, rx) = kanal::unbounded_async::<Event>();
        let (console_ctrl, console_rx) = create_mock_console_controller();
        let (ui_size_tx, ui_size_rx) = watch::channel((800u32, 600u32));
        let (mouse_pos_tx, mouse_pos_rx) = watch::channel((400i32, 300i32, true));
        let (input_cmd_tx, input_cmd_rx) = kanal::bounded::<InputCommand>(1024);

        let input_handler = InputHandler::builder()
            .input_cmd_tx(input_cmd_tx)
            .capability(Capability { keyboard: true, mouse: true, multitouch: false })
            .is_absolute(true)
            .build();

        let display = VmDisplayModel::builder()
            .launch(VmDisplayInit { rx, console_ctrl, input_handler, grab_shortcut: GrabShortcut::default() })
            .forward(sender.input_sender(), |_| AppMsg::Ignore);

        let display_widget = display.widget().clone();

        tokio::spawn(async move {
            while let Ok(cmd) = console_rx.recv().await {
                match cmd {
                    console::Command::SetUiInfo { width, height, .. } => {
                        info!("[Console] SetUiInfo => guest resize to {}x{}", width, height);
                        let _ = ui_size_tx.send((width.get(), height.get()));
                    }
                    other => {
                        info!("[Console] Command: {:?}", other);
                    }
                }
            }
        });
        tokio::spawn(mock_qemu_backend(tx, ui_size_rx, mouse_pos_rx));
        thread::spawn(move || {
            let mut mouse_x = 400i32;
            let mut mouse_y = 300i32;
            while let Ok(cmd) = input_cmd_rx.recv() {
                match cmd {
                    InputCommand::MouseSetAbs(x, y) => {
                        mouse_x = x as i32;
                        mouse_y = y as i32;
                        let _ = mouse_pos_tx.send((mouse_x, mouse_y, true));
                    }
                    InputCommand::MouseRel(dx, dy) => {
                        mouse_x = mouse_x.saturating_add(dx);
                        mouse_y = mouse_y.saturating_add(dy);
                        let _ = mouse_pos_tx.send((mouse_x, mouse_y, true));
                    }
                    _ => {}
                }
                info!("[Input] Command: {:?}", cmd);
            }
        });

        let model = AppModel { display };
        let widgets = view_output!();

        let sender_clone = sender.clone();
        widgets.scale_dropdown.connect_selected_item_notify(move |dropdown| {
            let mode = match dropdown.selected() {
                0 => ScalingMode::ResizeGuest,
                1 => ScalingMode::FixedGuest,
                _ => return,
            };
            sender_clone.input(AppMsg::SetScalingMode(mode));
        });

        widgets.input_dropdown.connect_selected_item_notify(move |dropdown| {
            let mode = match dropdown.selected() {
                0 => InputMode::Seamless,
                1 => InputMode::Confined,
                _ => return,
            };
            sender.input(AppMsg::SetInputMode(mode));
        });

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>) {
        match msg {
            AppMsg::SetScalingMode(mode) => {
                self.display.emit(libmks_rs::display::vm_display::Message::SetScalingMode(mode));
            }
            AppMsg::SetInputMode(mode) => {
                self.display.emit(libmks_rs::display::vm_display::Message::SetInputCaptureMode(mode));
            }
            AppMsg::Ignore => {}
        }
    }
}

fn create_mock_console_controller() -> (ConsoleController, kanal::AsyncReceiver<console::Command>) {
    let (console_tx, console_rx) = kanal::unbounded_async();
    (ConsoleController::from(console_tx), console_rx)
}

fn generate_frame(width: u32, height: u32, tick: u32) -> Vec<u8> {
    let stride = width * 4;
    let mut data = vec![255u8; (stride * height) as usize];
    for y in 0..height {
        for x in 0..width {
            let offset = ((y * width + x) * 4) as usize;
            if x % 50 == 0 || y % 50 == 0 {
                data[offset..offset + 4].copy_from_slice(&[0, 0, 0, 255]);
            } else {
                data[offset] = ((x ^ y).wrapping_add(tick)) as u8;
                data[offset + 1] = x.wrapping_add(tick / 2) as u8;
                data[offset + 2] = y.wrapping_add(tick) as u8;
                data[offset + 3] = 255;
            }
        }
    }
    data
}

fn nz(value: u32) -> NonZeroU32 { NonZeroU32::new(value).expect("example always uses non-zero dimensions") }

async fn mock_qemu_backend(
    tx: AsyncSender<Event>, ui_size_rx: watch::Receiver<(u32, u32)>, mouse_pos_rx: watch::Receiver<(i32, i32, bool)>,
) {
    let mut width = 800u32;
    let mut height = 600u32;
    let mut tick = 0u32;
    let mut timer = tokio::time::interval(Duration::from_millis(16));
    let mut ui_size_rx = ui_size_rx;
    let mut mouse_pos_rx = mouse_pos_rx;

    let cursor_w = 64u32;
    let cursor_h = 64u32;
    let cursor_hot_x = cursor_w / 2;
    let cursor_hot_y = cursor_h / 2;
    let cursor_hot_x_i32 = cursor_hot_x as i32;
    let cursor_hot_y_i32 = cursor_hot_y as i32;
    let mut cursor_data = vec![0u8; (cursor_w * cursor_h * 4) as usize];
    for y in 0..cursor_h {
        for x in 0..cursor_w {
            let i = ((y * cursor_w + x) * 4) as usize;
            let is_cross = x == cursor_w / 2 || y == cursor_h / 2;
            let is_center = x == cursor_w / 2 && y == cursor_h / 2;
            if is_center {
                cursor_data[i..i + 4].copy_from_slice(&[60, 200, 255, 255]);
            } else if is_cross {
                cursor_data[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
            } else {
                cursor_data[i..i + 4].copy_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
    let _ = tx
        .send(Event::CursorDefine {
            width: nz(cursor_w),
            height: nz(cursor_h),
            hot_x: cursor_hot_x,
            hot_y: cursor_hot_y,
            data: cursor_data.into(),
        })
        .await;

    loop {
        timer.tick().await;
        tick = tick.wrapping_add(1);

        let (target_w, target_h) = *ui_size_rx.borrow_and_update();
        if target_w > 0 && target_h > 0 && (target_w, target_h) != (width, height) {
            width = target_w;
            height = target_h;
            info!("[MockQemu] Applied resize request: {}x{}", width, height);
        }

        let data = generate_frame(width, height, tick);
        let stride = width.checked_mul(4).expect("example frame stride fits in u32");
        let _ = tx
            .send(Event::Scanout {
                width: nz(width),
                height: nz(height),
                stride: nz(stride),
                pixman_format: 0x20028888.into(),
                data: data.into(),
            })
            .await;

        let (x, y, visible) = *mouse_pos_rx.borrow_and_update();
        // VmDisplay currently positions guest cursor by image top-left, so convert center -> top-left.
        let min_top_left_x = -cursor_hot_x_i32;
        let min_top_left_y = -cursor_hot_y_i32;
        let max_top_left_x = width.saturating_sub(1) as i32 - cursor_hot_x_i32;
        let max_top_left_y = height.saturating_sub(1) as i32 - cursor_hot_y_i32;
        let top_left_x = (x - cursor_hot_x_i32).clamp(min_top_left_x, max_top_left_x);
        let top_left_y = (y - cursor_hot_y_i32).clamp(min_top_left_y, max_top_left_y);
        let _ = tx.send(Event::MouseSet { x: top_left_x, y: top_left_y, on: visible }).await;
    }
}

fn main() {
    env_logger::Builder::from_default_env().filter_level(log::LevelFilter::Info).init();

    let app = RelmApp::new("com.falcon.display.interactive");
    app.run::<AppModel>(());
}
