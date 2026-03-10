//! Example demonstrating the ScalingMode feature.
use kanal::AsyncSender;
use libmks_rs::{
    dbus::{
        console::{self, ConsoleController},
        listener::Event,
    },
    display::{
        input_handler::{Capability, InputHandler},
        vm_display::{GrabShortcut, ScalingMode, VmDisplayInit, VmDisplayModel},
    },
};
use log::info;
use relm4::{Controller, gtk::prelude::*, prelude::*};
use std::{num::NonZeroU32, time::Duration};
use tokio::sync::watch;

struct AppModel {
    display: Controller<VmDisplayModel>,
}

#[derive(Debug)]
enum AppMsg {
    SetScalingMode(ScalingMode),
    Ignore,
}

#[relm4::component]
impl SimpleComponent for AppModel {
    type Init = ();
    type Input = AppMsg;
    type Output = ();

    view! {
        gtk::Window {
            set_title: Some("Scaling Mode Demo"),
            set_default_width: 960,
            set_default_height: 700,
            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_spacing: 10,
                    set_margin_all: 8,
                    gtk::Label {
                        set_label: "Mode:",
                    },
                    #[name = "dropdown"]
                    gtk::DropDown {
                        set_model: Some(&gtk::StringList::new(&[
                            "Resize Guest (Auto)",
                            "Fixed Guest (Scaled)",
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

        let input_handler =
            InputHandler::builder().capability(Capability { keyboard: false, mouse: false, multitouch: false }).build();

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
        tokio::spawn(mock_qemu_backend(tx, ui_size_rx));

        let model = AppModel { display };
        let widgets = view_output!();

        widgets.dropdown.connect_selected_item_notify(move |dropdown| {
            let mode = match dropdown.selected() {
                0 => ScalingMode::ResizeGuest,
                1 => ScalingMode::FixedGuest,
                _ => return,
            };
            sender.input(AppMsg::SetScalingMode(mode));
        });

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>) {
        match msg {
            AppMsg::SetScalingMode(mode) => {
                self.display.emit(libmks_rs::display::vm_display::Message::SetScalingMode(mode));
            }
            AppMsg::Ignore => {}
        }
    }
}

/// Creates a mock console controller pair for the demo.
///
/// - `ConsoleController`: sender exposed to the UI model.
/// - `AsyncReceiver<console::Command>`: backend-side command receiver.
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
            data[offset] = (x ^ y).wrapping_add(tick) as u8;
            data[offset + 1] = (x.wrapping_add(tick)) as u8;
            data[offset + 2] = (y.wrapping_add(tick / 2)) as u8;
            data[offset + 3] = 255;
        }
    }
    data
}

fn nz(value: u32) -> NonZeroU32 { NonZeroU32::new(value).expect("example always uses non-zero dimensions") }

async fn mock_qemu_backend(tx: AsyncSender<Event>, ui_size_rx: watch::Receiver<(u32, u32)>) {
    let mut width = 800u32;
    let mut height = 600u32;
    let mut tick = 0u32;
    let mut timer = tokio::time::interval(Duration::from_millis(33));
    let mut ui_size_rx = ui_size_rx;

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
    }
}

fn main() {
    env_logger::Builder::from_default_env().filter_level(log::LevelFilter::Info).init();

    let app = RelmApp::new("com.falcon.display.scaling");
    app.run::<AppModel>(());
}
