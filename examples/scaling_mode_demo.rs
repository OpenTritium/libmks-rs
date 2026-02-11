//! Example demonstrating the new ScalingMode feature.
//!
//! This example shows how to switch between two scaling modes:
//! - ResizeGuest: Window resize triggers VM resolution change (default)
//! - FixedGuest: VM resolution stays fixed, window only scales the display
use kanal::AsyncSender;
use libmks_rs::{
    dbus::{
        console::{self, ConsoleController},
        keyboard::KeyboardController,
        listener::Event,
        mouse::{self, MouseController},
    },
    display::{
        ScalingMode,
        vm_display::{GrabShortcut, VmDisplayInit, VmDisplayModel},
    },
};
use log::info;
use relm4::{Controller, gtk::prelude::*, prelude::*};
use std::time::Duration;

struct AppModel {
    display: Controller<VmDisplayModel>,
}

#[relm4::component]
impl SimpleComponent for AppModel {
    type Init = ();
    type Input = AppMsg;
    type Output = ();

    view! {
        gtk::Window {
            set_title: Some("Psychedelic XOR Pattern Test (Interactive)"),
            set_default_width: 800,
            set_default_height: 600,
            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_spacing: 10,
                    set_margin_all: 5,
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
                    gtk::Label {
                        set_label: "Tip: Drag window to test resize. Move mouse to test loopback.",
                        set_opacity: 0.7,
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

    fn init(_: (), root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let (tx, rx) = kanal::unbounded_async::<Event>();

        let (console_ctrl, mouse_ctrl, kbd_ctrl, mouse_rx, console_rx) = create_mock_controllers();

        let display = VmDisplayModel::builder()
            .launch(VmDisplayInit {
                rx,
                console_ctrl,
                mouse_ctrl,
                keyboard_ctrl: kbd_ctrl,
                grab_shortcut: GrabShortcut::default(),
            })
            .forward(sender.input_sender(), |_| AppMsg::Ignore);

        let display_widget = display.widget().clone();

        tokio::spawn(mock_qemu_backend(tx, mouse_rx, console_rx));

        let model = AppModel { display };
        let widgets = view_output!();

        // Connect dropdown signal manually
        widgets.dropdown.connect_selected_item_notify(move |dropdown| {
            let mode = match dropdown.selected() {
                0 => ScalingMode::ResizeGuest,
                1 => ScalingMode::FixedGuest,
                _ => return,
            };
            info!("Switching to scaling mode: {:?}", mode);
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

#[derive(Debug)]
enum AppMsg {
    SetScalingMode(ScalingMode),
    Ignore,
}

fn create_mock_controllers() -> (
    ConsoleController,
    MouseController,
    KeyboardController,
    kanal::AsyncReceiver<mouse::Command>,
    kanal::AsyncReceiver<console::Command>,
) {
    let (console_tx, console_rx) = kanal::unbounded_async();
    let (mouse_tx, mouse_rx) = kanal::unbounded_async();
    let (kbd_tx, _) = kanal::unbounded_async();

    let console_ctrl = ConsoleController::from(console_tx);
    let mouse_ctrl = MouseController::from(mouse_tx);
    let kbd_ctrl = KeyboardController::from(kbd_tx);

    (console_ctrl, mouse_ctrl, kbd_ctrl, mouse_rx, console_rx)
}

fn generate_psychedelic_frame(width: u32, height: u32, time_offset: u32) -> Vec<u8> {
    let stride = width * 4;
    let mut data = vec![255u8; (stride * height) as usize];

    for y in 0..height {
        for x in 0..width {
            let offset = ((y * width + x) * 4) as usize;

            let r = ((x ^ y).wrapping_add(time_offset)) as u8;
            let g = x.wrapping_add(time_offset.wrapping_mul(2)) as u8;
            let b = y.wrapping_add(time_offset.wrapping_mul(3)) as u8;

            let is_grid = x % 50 == 0 || y % 50 == 0;

            if is_grid {
                data[offset] = 0;
                data[offset + 1] = 0;
                data[offset + 2] = 0;
                data[offset + 3] = 255;
            } else {
                data[offset] = b;
                data[offset + 1] = g;
                data[offset + 2] = r;
                data[offset + 3] = 255;
            }
        }
    }
    data
}

async fn mock_qemu_backend(
    tx: AsyncSender<Event>, mouse_rx: kanal::AsyncReceiver<mouse::Command>,
    console_rx: kanal::AsyncReceiver<console::Command>,
) {
    info!("🎨 Mock Backend Started - Initializing psychedelic display...");

    let cursor_w = 32;
    let cursor_h = 32;
    let mut cursor_data = vec![0u8; (cursor_w * cursor_h * 4) as usize];
    for y in 0..cursor_h {
        for x in 0..cursor_w {
            let idx = ((y * cursor_w + x) * 4) as usize;
            let is_border = x == 0 || x == cursor_w - 1 || y == 0 || y == cursor_h - 1;
            if is_border {
                cursor_data[idx..idx + 4].copy_from_slice(&[0, 0, 0, 255]);
            } else {
                cursor_data[idx..idx + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
    }
    tx.send(Event::CursorDefine { width: cursor_w, height: cursor_h, hot_x: 16, hot_y: 16, data: cursor_data.into() })
        .await
        .ok();

    let mut current_w = 800u32;
    let mut current_h = 600u32;
    let mut frame_timer = tokio::time::interval(Duration::from_millis(16));
    let mut time_offset = 0u32;

    info!("✅ Cursor defined. Starting animation loop...");

    loop {
        tokio::select! {
            _ = frame_timer.tick() => {
                time_offset = time_offset.wrapping_add(1);

                let data = generate_psychedelic_frame(current_w, current_h, time_offset);

                tx.send(Event::Scanout {
                    width: current_w,
                    height: current_h,
                    stride: current_w * 4,
                    pixman_format: 0x20028888,
                    data: data.into(),
                }).await.ok();
            }

            Ok(cmd) = mouse_rx.recv() => {
                match cmd {
                    mouse::Command::SetAbsPosition { x, y } => {
                        tx.send(Event::MouseSet { x: x as i32, y: y as i32, on: 1 }).await.ok();
                    }
                    mouse::Command::Press(btn) => {
                        info!("👆 Mouse Pressed: {:?}", btn);
                    }
                    mouse::Command::Release(btn) => {
                        info!("👇 Mouse Released: {:?}", btn);
                    }
                    mouse::Command::RelMotion { dx, dy } => {
                        if dy != 0 { info!("📜 Scroll Y: {}", dy); }
                        if dx != 0 { info!("📜 Scroll X: {}", dx); }
                    }
                }
            }

            Ok(cmd) = console_rx.recv() => {
                if let console::Command::SetUiInfo { width, height, .. } = cmd && width > 0 && height > 0 && (width != current_w || height != current_h) {
                    info!("📐 Guest Resizing: {}x{} → {}x{}", current_w, current_h, width, height);
                    current_w = width;
                    current_h = height;
                }
            }
        }
    }
}

fn main() {
    env_logger::Builder::from_default_env().filter_level(log::LevelFilter::Info).init();

    let app = RelmApp::new("com.falcon.display.xor");
    app.run::<AppModel>(());
}
