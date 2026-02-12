use kanal::AsyncSender;
use libmks_rs::{
    dbus::{
        console::ConsoleController,
        keyboard::KeyboardController,
        listener::Event,
        mouse::{self, MouseController},
    },
    display::vm_display::{GrabShortcut, VmDisplayInit, VmDisplayModel},
};
use log::{debug, info, warn};
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
            set_title: Some("VM Display with Real-time Input Loopback"),
            set_default_width: 1024,
            set_default_height: 768,
            #[local_ref]
            display_widget -> gtk::Overlay {
                set_hexpand: true,
                set_vexpand: true,
            },
        }
    }

    fn init(_: (), root: Self::Root, _sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let (tx, rx) = kanal::unbounded_async::<Event>();

        let (console_ctrl, mouse_ctrl, kbd_ctrl, mouse_rx) = create_mock_controllers();

        let _display = VmDisplayModel::builder()
            .launch(VmDisplayInit {
                rx,
                console_ctrl,
                mouse_ctrl,
                keyboard_ctrl: kbd_ctrl,
                grab_shortcut: GrabShortcut::default(),
            })
            .detach();

        let display_widget = _display.widget().clone();

        tokio::spawn(mock_qemu_source(tx, mouse_rx));

        let model = AppModel { _display };
        let widgets = view_output!();

        ComponentParts { model, widgets }
    }
}

fn create_mock_controllers()
-> (ConsoleController, MouseController, KeyboardController, kanal::AsyncReceiver<mouse::Command>) {
    let (console_tx, console_rx) = kanal::unbounded_async();
    let (mouse_tx, mouse_rx) = kanal::unbounded_async();
    let (kbd_tx, kbd_rx) = kanal::unbounded_async();

    tokio::spawn(async move {
        while let Ok(cmd) = console_rx.recv().await {
            info!("[Console] Command: {:?}", cmd);
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

    (console_ctrl, mouse_ctrl, kbd_ctrl, mouse_rx)
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

async fn mock_qemu_source(tx: AsyncSender<Event>, mouse_cmd_rx: kanal::AsyncReceiver<mouse::Command>) {
    let cursor_w = 64;
    let cursor_h = 64;
    let mut cursor_data = vec![0u8; (cursor_w * cursor_h * 4) as usize];
    for y in 0..cursor_h {
        for x in 0..cursor_w {
            let i = ((y * cursor_w + x) * 4) as usize;
            let is_center_line = x == 31 || y == 31;
            let is_border = (x >= 30 && x <= 32) || (y >= 30 && y <= 32);

            if is_center_line {
                cursor_data[i..i+4].copy_from_slice(&[255, 255, 255, 255]);
            } else if is_border {
                cursor_data[i..i+4].copy_from_slice(&[0, 0, 0, 255]);
            } else {
                cursor_data[i..i+4].copy_from_slice(&[0, 0, 0, 0]);
            }
        }
    }

    tx.send(Event::CursorDefine { width: cursor_w, height: cursor_h, hot_x: 31, hot_y: 31, data: cursor_data.into() })
        .await
        .ok();

    let mut current_w = 800;
    let mut current_h = 600;
    let mut frame_count: u64 = 0;

    // === 新增：模拟光标的内部状态 ===
    let mut mock_cursor_x = 400; // 初始位置
    let mut mock_cursor_y = 300;

    info!("Simulation Started: Phase 1 - 800x600 (Blue)");
    info!("Move your mouse over the VM display to see the cursor follow!");

    let bg_data = generate_frame(current_w, current_h, 0, 0, 255);
    tx.send(Event::Scanout {
        width: current_w,
        height: current_h,
        stride: current_w * 4,
        pixman_format: 0x20028888,
        data: bg_data.into(),
    })
    .await
    .ok();

    let mut interval = tokio::time::interval(Duration::from_millis(16));

    loop {
        tokio::select! {
            _ = interval.tick() => {
                frame_count += 1;

                if frame_count == 180 {
                    info!("Phase 2: Resize to 1280x720 (Green) - Check coordinate accuracy");
                    current_w = 1280;
                    current_h = 720;

                    let bg = generate_frame(current_w, current_h, 0, 255, 0);
                    tx.send(Event::Scanout {
                        width: current_w,
                        height: current_h,
                        stride: current_w * 4,
                        pixman_format: 0x20028888,
                        data: bg.into(),
                    })
                    .await
                    .ok();
                }

                if frame_count == 360 {
                    info!("Phase 3: Disable Event - Cursor should disappear");
                    tx.send(Event::Disable).await.ok();
                }

                if frame_count == 480 {
                    info!("Phase 4: Re-enable 400x600 (Red) - Check pillarboxing");
                    current_w = 400;
                    current_h = 600;

                    let bg = generate_frame(current_w, current_h, 255, 0, 0);
                    tx.send(Event::Scanout {
                        width: current_w,
                        height: current_h,
                        stride: current_w * 4,
                        pixman_format: 0x20028888,
                        data: bg.into(),
                    })
                    .await
                    .ok();

                    // 重新定义光标（Disable 事件清空了光标纹理）
                    let mut cursor_data = vec![0u8; (cursor_w * cursor_h * 4) as usize];
                    for i in 0..(cursor_w * cursor_h) as usize {
                        let offset = i * 4;
                        cursor_data[offset] = 0;
                        cursor_data[offset + 1] = 255;
                        cursor_data[offset + 2] = 255;
                        cursor_data[offset + 3] = 255;
                    }
                    tx.send(Event::CursorDefine {
                        width: cursor_w,
                        height: cursor_h,
                        hot_x: 0,
                        hot_y: 0,
                        data: cursor_data.into(),
                    })
                    .await
                    .ok();
                }
            }

            Ok(cmd) = mouse_cmd_rx.recv() => {
                if !(360..=480).contains(&frame_count) {
                    match cmd {
                        mouse::Command::SetAbsPosition { x, y } => {
                            // 收到绝对坐标（非捕获模式）
                            mock_cursor_x = x as i32;
                            mock_cursor_y = y as i32;

                            tx.send(Event::MouseSet {
                                x: x as i32,
                                y: y as i32,
                                on: 1,
                            })
                            .await
                            .ok();
                        }
                        mouse::Command::RelMotion { dx, dy } => {
                            // === 核心修改：处理相对位移（Wayland 锁定模式）===

                            // 1. 更新模拟光标的内部位置
                            mock_cursor_x = (mock_cursor_x + dx).clamp(0, current_w as i32);
                            mock_cursor_y = (mock_cursor_y + dy).clamp(0, current_h as i32);

                            // 2. 发送 MouseSet 消息回 UI，让光标真的动起来
                            tx.send(Event::MouseSet {
                                x: mock_cursor_x,
                                y: mock_cursor_y,
                                on: 1,
                            })
                            .await
                            .ok();

                            // 3. 打印日志证明我们在使用相对移动
                            // 使用 debug 级别防止刷屏，或者使用 periodic log
                            if frame_count % 30 == 0 { // 减少日志刷屏
                                info!("[Mock] Wayland RelMotion ({}, {}) -> Pos ({}, {})", dx, dy, mock_cursor_x, mock_cursor_y);
                            } else {
                                debug!("[Mock] Wayland RelMotion ({}, {})", dx, dy);
                            }
                        }
                        mouse::Command::Press(btn) => {
                            info!("[Mock] Press: {:?}", btn);
                        }
                        mouse::Command::Release(btn) => {
                            info!("[Mock] Release: {:?}", btn);
                        }
                    }
                } else {
                    warn!("[Mock] Command ignored during Disable phase");
                }
            }
        }
    }
}

fn main() {
    env_logger::Builder::from_default_env().filter_level(log::LevelFilter::Debug).init();

    let app = RelmApp::new("com.falcon.display.loopback");
    app.run::<AppModel>(());
}
