//! Interactive demo for scaling and input mode toggles.
use kanal::AsyncSender;
use libmks_rs::{
    dbus::{
        console::{self, ConsoleController},
        listener::Event,
    },
    display::{
        input_handler::{Capability, InputHandler},
        vm_display::{GrabShortcut, InputMode, ScalingMode, VmDisplayInit, VmDisplayModel},
    },
};
use log::info;
use relm4::{Controller, gtk::prelude::*, prelude::*};
use std::time::Duration;

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

        let input_handler =
            InputHandler::builder().capability(Capability { keyboard: false, mouse: false, multitouch: false }).build();

        let display = VmDisplayModel::builder()
            .launch(VmDisplayInit { rx, console_ctrl, input_handler, grab_shortcut: GrabShortcut::default() })
            .forward(sender.input_sender(), |_| AppMsg::Ignore);

        let display_widget = display.widget().clone();

        tokio::spawn(async move {
            while let Ok(cmd) = console_rx.recv().await {
                info!("[Console] Command: {:?}", cmd);
            }
        });
        tokio::spawn(mock_qemu_backend(tx));

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

async fn mock_qemu_backend(tx: AsyncSender<Event>) {
    let mut width = 800u32;
    let mut height = 600u32;
    let mut tick = 0u32;
    let mut timer = tokio::time::interval(Duration::from_millis(16));

    loop {
        timer.tick().await;
        tick = tick.wrapping_add(1);
        if tick.is_multiple_of(360) {
            if (width, height) == (800, 600) {
                width = 1280;
                height = 720;
            } else {
                width = 800;
                height = 600;
            }
        }

        let data = generate_frame(width, height, tick);
        let _ = tx
            .send(Event::Scanout { width, height, stride: width * 4, pixman_format: 0x20028888, data: data.into() })
            .await;
    }
}

fn main() {
    env_logger::Builder::from_default_env().filter_level(log::LevelFilter::Info).init();

    let app = RelmApp::new("com.falcon.display.interactive");
    app.run::<AppModel>(());
}
