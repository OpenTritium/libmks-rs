use kanal::{AsyncReceiver, AsyncSender};
use libmks_rs::{
    dbus::{
        console::ConsoleController,
        keyboard::{KeyboardController, Command as KbdCommand},
        mouse::{MouseController, Command as MouseCommand},
        listener::QemuEvent,
    },
    display::vm_display::{VmDisplayInit, VmDisplayModel},
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
            set_title: Some("VM Display with Input Test"),
            set_default_size: (1024, 768),
            #[local_ref]
            display_widget -> gtk::Overlay {},
        }
    }

    fn init(_: (), root: Self::Root, _sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let (tx, rx) = kanal::unbounded_async::<QemuEvent>();

        let (console_ctrl, mouse_ctrl, kbd_ctrl) = create_mock_controllers();

        let _display = VmDisplayModel::builder()
            .launch(VmDisplayInit {
                rx,
                console_ctrl,
                mouse_ctrl,
                keyboard_ctrl: kbd_ctrl,
            })
            .detach();

        let display_widget = _display.widget().clone();

        tokio::spawn(mock_qemu_source(tx));

        let model = AppModel { _display };
        let widgets = view_output!();

        ComponentParts { model, widgets }
    }
}

fn create_mock_controllers() -> (ConsoleController, MouseController, KeyboardController) {
    let (console_tx, mut console_rx) = kanal::unbounded_async();
    let (mouse_tx, mut mouse_rx) = kanal::unbounded_async();
    let (kbd_tx, mut kbd_rx) = kanal::unbounded_async();

    tokio::spawn(async move {
        while let Ok(cmd) = console_rx.recv().await {
            info!("[Console] Command: {:?}", cmd);
        }
    });

    tokio::spawn(async move {
        while let Ok(cmd) = mouse_rx.recv().await {
            info!("[Mouse] Command: {:?}", cmd);
        }
    });

    tokio::spawn(async move {
        while let Ok(cmd) = kbd_rx.recv().await {
            info!("[Keyboard] Command: {:?}", cmd);
        }
    });

    let console_ctrl = ConsoleController::from(console_tx);
    let mouse_ctrl = MouseController::from(mouse_tx);
    let kbd_ctrl = KeyboardController::from(kbd_tx);

    (console_ctrl, mouse_ctrl, kbd_ctrl)
}

fn generate_frame(width: u32, height: u32, bg_r: u8, bg_g: u8, bg_b: u8) -> Vec<u8> {
    let stride = width * 4;
    let mut data = vec![0u8; (stride * height) as usize];

    let border_r = 255u8.saturating_sub(bg_r);
    let border_g = 255u8.saturating_sub(bg_g);
    let border_b = 255u8.saturating_sub(bg_b);

    for y in 0..height {
        for x in 0..width {
            let offset = ((y * width + x) * 4) as usize;

            let is_border = x < 10 || x >= width - 10 || y < 10 || y >= height - 10;

            if is_border {
                data[offset] = border_b;
                data[offset + 1] = border_g;
                data[offset + 2] = border_r;
                data[offset + 3] = 255;
            } else {
                data[offset] = bg_b;
                data[offset + 1] = bg_g;
                data[offset + 2] = bg_r;
                data[offset + 3] = 255;
            }
        }
    }
    data
}

async fn mock_qemu_source(tx: AsyncSender<QemuEvent>) {
    let cursor_w = 64;
    let cursor_h = 64;
    let mut cursor_data = vec![0u8; (cursor_w * cursor_h * 4) as usize];
    for i in 0..(cursor_w * cursor_h) as usize {
        let offset = i * 4;
        cursor_data[offset] = 0;
        cursor_data[offset + 1] = 255;
        cursor_data[offset + 2] = 255;
        cursor_data[offset + 3] = 255;
    }

    tx.send(QemuEvent::CursorDefine { width: cursor_w, height: cursor_h, hot_x: 0, hot_y: 0, data: cursor_data })
        .await
        .ok();

    let mut current_w = 800;
    let mut current_h = 600;
    let mut mouse_x = 0;
    let mut mouse_y = 0;
    let mut frame_count: u64 = 0;

    info!("Simulation Started: Phase 1 - 800x600 (Blue)");

    let bg_data = generate_frame(current_w, current_h, 0, 0, 255);
    tx.send(QemuEvent::Scanout {
        width: current_w,
        height: current_h,
        stride: current_w * 4,
        pixman_format: 0x20028888,
        data: bg_data,
    })
    .await
    .ok();

    loop {
        tokio::time::sleep(Duration::from_millis(16)).await;
        frame_count += 1;

        mouse_x = (mouse_x + 5) % current_w as i32;
        mouse_y = (mouse_y + 3) % current_h as i32;

        if !(360..=480).contains(&frame_count) {
            tx.send(QemuEvent::MouseSet { x: mouse_x, y: mouse_y, on: 1 }).await.ok();
        }

        if frame_count == 180 {
            info!("Phase 2: Resize to 1280x720 (Green)");
            current_w = 1280;
            current_h = 720;
            mouse_x = 0;
            mouse_y = 0;

            let bg = generate_frame(current_w, current_h, 0, 255, 0);
            tx.send(QemuEvent::Scanout {
                width: current_w,
                height: current_h,
                stride: current_w * 4,
                pixman_format: 0x20028888,
                data: bg,
            })
            .await
            .ok();
        }

        if frame_count == 360 {
            info!("Phase 3: Disable Event");
            tx.send(QemuEvent::Disable).await.ok();
        }

        if frame_count == 480 {
            info!("Phase 4: Re-enable 400x600 (Red)");
            current_w = 400;
            current_h = 600;
            mouse_x = 0;
            mouse_y = 0;

            let bg = generate_frame(current_w, current_h, 255, 0, 0);
            tx.send(QemuEvent::Scanout {
                width: current_w,
                height: current_h,
                stride: current_w * 4,
                pixman_format: 0x20028888,
                data: bg,
            })
            .await
            .ok();
        }
    }
}

fn main() {
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();

    let app = RelmApp::new("com.falcon.display.test");
    app.run::<AppModel>(());
}
