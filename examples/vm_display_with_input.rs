use kanal::AsyncSender;
use libmks_rs::{
    dbus::{
        console::{self, ConsoleController},
        listener::Event,
    },
    display::{
        input_handler::{Capability, InputHandler},
        vm_display::{GrabShortcut, VmDisplayInit, VmDisplayModel},
    },
};
use log::info;
use relm4::{Controller, gtk::prelude::*, prelude::*};
use std::time::Duration;

struct AppModel {
    _display: Controller<VmDisplayModel>,
}

#[relm4::component]
impl SimpleComponent for AppModel {
    type Init = ();
    type Input = ();
    type Output = ();

    view! {
        gtk::Window {
            set_title: Some("VM Display with Input Bus"),
            set_default_width: 1024,
            set_default_height: 768,
            #[local_ref]
            display_widget -> gtk::Overlay {
                set_hexpand: true,
                set_vexpand: true,
            },
        }
    }

    fn init(_: (), _root: Self::Root, _sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let (tx, rx) = kanal::unbounded_async::<Event>();
        let (console_ctrl, console_rx) = create_mock_console_controller();

        let input_handler =
            InputHandler::builder().capability(Capability { keyboard: false, mouse: false, multitouch: false }).build();

        let _display = VmDisplayModel::builder()
            .launch(VmDisplayInit { rx, console_ctrl, input_handler, grab_shortcut: GrabShortcut::default() })
            .detach();

        let display_widget = _display.widget().clone();

        tokio::spawn(async move {
            while let Ok(cmd) = console_rx.recv().await {
                info!("[Console] Command: {:?}", cmd);
            }
        });
        tokio::spawn(mock_qemu_source(tx));

        let model = AppModel { _display };
        let widgets = view_output!();

        ComponentParts { model, widgets }
    }
}

fn create_mock_console_controller() -> (ConsoleController, kanal::AsyncReceiver<console::Command>) {
    let (console_tx, console_rx) = kanal::unbounded_async();
    (ConsoleController::from(console_tx), console_rx)
}

fn generate_frame(width: u32, height: u32, bg_r: u8, bg_g: u8, bg_b: u8) -> Vec<u8> {
    let stride = width * 4;
    let mut data = vec![0u8; (stride * height) as usize];

    for y in 0..height {
        for x in 0..width {
            let offset = ((y * width + x) * 4) as usize;
            data[offset] = bg_b;
            data[offset + 1] = bg_g;
            data[offset + 2] = bg_r;
            data[offset + 3] = 255;
        }
    }
    data
}

async fn mock_qemu_source(tx: AsyncSender<Event>) {
    let cursor_w = 64;
    let cursor_h = 64;
    let mut cursor_data = vec![0u8; (cursor_w * cursor_h * 4) as usize];
    for y in 0..cursor_h {
        for x in 0..cursor_w {
            let i = ((y * cursor_w + x) * 4) as usize;
            if x == 31 || y == 31 {
                cursor_data[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
            } else {
                cursor_data[i..i + 4].copy_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
    let _ = tx
        .send(Event::CursorDefine { width: cursor_w, height: cursor_h, hot_x: 31, hot_y: 31, data: cursor_data.into() })
        .await;

    let mut current_w = 800;
    let mut current_h = 600;
    let mut frame_count: u64 = 0;
    let mut interval = tokio::time::interval(Duration::from_millis(33));

    loop {
        interval.tick().await;
        frame_count += 1;

        if frame_count == 180 {
            current_w = 1280;
            current_h = 720;
        }
        if frame_count == 360 {
            current_w = 400;
            current_h = 600;
            frame_count = 0;
        }

        let color_phase = (frame_count % 120) as u8;
        let bg_data = generate_frame(current_w, current_h, color_phase, 255u8.saturating_sub(color_phase), 80);
        let _ = tx
            .send(Event::Scanout {
                width: current_w,
                height: current_h,
                stride: current_w * 4,
                pixman_format: 0x20028888,
                data: bg_data.into(),
            })
            .await;

        let _ = tx.send(Event::MouseSet { x: (current_w / 2) as i32, y: (current_h / 2) as i32, on: 1 }).await;
    }
}

fn main() {
    env_logger::Builder::from_default_env().filter_level(log::LevelFilter::Info).init();

    let app = RelmApp::new("com.falcon.display.loopback");
    app.run::<AppModel>(());
}
