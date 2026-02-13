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

        let (console_ctrl, mouse_ctrl, kbd_ctrl, mouse_rx, console_rx, kbd_rx) = create_mock_controllers();

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

        tokio::spawn(mock_qemu_backend(tx, mouse_rx, console_rx, kbd_rx));

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
    kanal::AsyncReceiver<libmks_rs::dbus::keyboard::Command>,
) {
    let (console_tx, console_rx) = kanal::unbounded_async();
    let (mouse_tx, mouse_rx) = kanal::unbounded_async();
    let (kbd_tx, kbd_rx) = kanal::unbounded_async();

    let console_ctrl = ConsoleController::from(console_tx);
    let mouse_ctrl = MouseController::from(mouse_tx);
    let kbd_ctrl = KeyboardController::from(kbd_tx);

    (console_ctrl, mouse_ctrl, kbd_ctrl, mouse_rx, console_rx, kbd_rx)
}

fn generate_psychedelic_frame(width: u32, height: u32, time_offset: u32) -> Vec<u8> {
    let stride = width * 4;
    let mut data = vec![255u8; (stride * height) as usize];

    for y in 0..height {
        for x in 0..width {
            let offset = ((y * width + x) * 4) as usize;

            // 🎯 视觉验证核心：绘制 50px 的黑色网格线
            // 这与后端日志中的 % 50 逻辑对应
            if x % 50 == 0 || y % 50 == 0 {
                data[offset..offset + 4].copy_from_slice(&[0, 0, 0, 255]); // Black
            } else {
                // 生成动态迷幻色
                let r = ((x ^ y).wrapping_add(time_offset)) as u8;
                let g = x.wrapping_add(time_offset.wrapping_mul(2)) as u8;
                let b = y.wrapping_add(time_offset.wrapping_mul(3)) as u8;

                data[offset] = b; // B
                data[offset + 1] = g; // G
                data[offset + 2] = r; // R
                data[offset + 3] = 255; // Alpha
            }
        }
    }
    data
}

async fn mock_qemu_backend(
    tx: AsyncSender<Event>, mouse_rx: kanal::AsyncReceiver<mouse::Command>,
    console_rx: kanal::AsyncReceiver<console::Command>,
    kbd_rx: kanal::AsyncReceiver<libmks_rs::dbus::keyboard::Command>,
) {
    info!("🎨 Mock Backend Started");

    // 1. 初始化光标 (白色十字准星 with 黑色描边)
    let cursor_w = 64;
    let cursor_h = 64;
    let mut cursor_data = vec![0u8; (cursor_w * cursor_h * 4) as usize];
    for y in 0..cursor_h {
        for x in 0..cursor_w {
            let i = ((y * cursor_w + x) * 4) as usize;
            let is_center_line = x == 31 || y == 31;
            let is_border = (x >= 30 && x <= 32) || (y >= 30 && y <= 32);

            if is_center_line {
                cursor_data[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
            } else if is_border {
                cursor_data[i..i + 4].copy_from_slice(&[0, 0, 0, 255]);
            } else {
                cursor_data[i..i + 4].copy_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
    tx.send(Event::CursorDefine { width: cursor_w, height: cursor_h, hot_x: 31, hot_y: 31, data: cursor_data.into() })
        .await
        .ok();

    // 2. 状态变量
    let mut current_w = 800u32;
    let mut current_h = 600u32;
    let mut frame_timer = tokio::time::interval(Duration::from_millis(16)); // ~60 FPS
    let mut time_offset = 0u32;

    // 虚拟光标位置 (服务器端真值)
    let mut v_cursor_x = 400i32;
    let mut v_cursor_y = 300i32;

    loop {
        tokio::select! {
            // A. 渲染循环 (发送图像数据)
            _ = frame_timer.tick() => {
                time_offset = time_offset.wrapping_add(1);
                let data = generate_psychedelic_frame(current_w, current_h, time_offset);
                tx.send(Event::Scanout {
                    width: current_w,
                    height: current_h,
                    stride: current_w * 4,
                    pixman_format: 0x20028888,
                    data: data.into(),
                })
                .await
                .ok();
            }

            // B. 处理鼠标指令
            Ok(cmd) = mouse_rx.recv() => {
                match cmd {
                    // 无缝模式 / 初始捕获校准
                    mouse::Command::SetAbsPosition { x, y } => {
                        v_cursor_x = x as i32;
                        v_cursor_y = y as i32;
                        // 打印重置日志，方便调试 "跳变" 问题
                        info!("🔄 Cursor Reset: ({}, {})", v_cursor_x, v_cursor_y);
                        // 回显给前端绘制红色光标
                        tx.send(Event::MouseSet { x: v_cursor_x, y: v_cursor_y, on: 1 }).await.ok();
                    }

                    // 锁定模式 (相对移动)
                    mouse::Command::RelMotion { dx, dy } => {
                        // 1. 核心逻辑：累加 Delta 并限制在屏幕范围内
                        v_cursor_x = (v_cursor_x + dx).clamp(0, current_w as i32);
                        v_cursor_y = (v_cursor_y + dy).clamp(0, current_h as i32);

                        // 2. 网格命中检测 (Grid Hit Test)
                        let hit_x = v_cursor_x % 50 == 0;
                        let hit_y = v_cursor_y % 50 == 0;

                        // 3. 智能日志：只在关键时刻打印
                        if hit_x || hit_y {
                            info!("🎯 GRID HIT: ({:4}, {:4}) {}{}",
                                v_cursor_x, v_cursor_y,
                                if hit_x { " | COL" } else { "" }, // 撞到竖线
                                if hit_y { " - ROW" } else { "" }  // 撞到横线
                            );
                        } else if dx.abs() > 10 || dy.abs() > 10 {
                            // 快速甩动时偶尔打印
                            // info!("💨 Fast Move: ({}, {})", v_cursor_x, v_cursor_y);
                        }

                        // 4. 回显给前端
                        tx.send(Event::MouseSet { x: v_cursor_x, y: v_cursor_y, on: 1 }).await.ok();
                    }

                    mouse::Command::Press(btn) => info!("🖱️ Click: ({}, {}) - Btn {:?}", v_cursor_x, v_cursor_y, btn),
                    mouse::Command::Release(btn) => info!("🖱️ Release: {:?}", btn),
                }
            }

            // C. 处理 Resize 指令
            Ok(cmd) = console_rx.recv() => {
                if let console::Command::SetUiInfo { width, height, .. } = cmd {
                    if width > 0 && height > 0 && (width != current_w || height != current_h) {
                        info!("📏 Resize: {}x{}", width, height);
                        current_w = width;
                        current_h = height;
                        // Resize 后重置到中心，防止越界
                        v_cursor_x = (current_w / 2) as i32;
                        v_cursor_y = (current_h / 2) as i32;
                    }
                }
            }

            // D. 处理键盘指令 (Drain Channel)
            // 必须接收，否则发送端会 BrokenPipe 报错
            Ok(_cmd) = kbd_rx.recv() => {
                // 这里可以加 info! 来调试键盘
            }
        }
    }
}

fn main() {
    env_logger::Builder::from_default_env().filter_level(log::LevelFilter::Info).init();

    let app = RelmApp::new("com.falcon.display.xor");
    app.run::<AppModel>(());
}
