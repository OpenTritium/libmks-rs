//! Example demonstrating both ScalingMode and InputMode features with interactive UI.
//!
//! Features:
//! - Scaling: Resize vs Fixed
//! - Input: Seamless (Absolute) vs Locked (Relative/Wayland)
//! - Backend: A psychedelic pattern generator that responds to mouse input mock events.
//!
//! How to test:
//! 1. Launch the application - defaults to "Seamless (Tablet)" mode.
//! 2. **Testing Seamless mode**: Mouse enters window -> red semi-transparent cursor appears and follows. Mouse exits
//!    window -> cursor disappears. No clicking needed.
//! 3. **Switch mode**: Select "Locked (Gaming)" from dropdown menu.
//! 4. **Testing Locked mode**: Mouse enters window -> cursor stays as system arrow (not locked yet). **Click** window
//!    -> system cursor disappears, red cursor appears.
//! 5. **Move** mouse -> red cursor should move according to your movement (this is where the Mock Backend processes
//!    `RelMotion` events).
//! 6. **Release**: Press `Ctrl+Alt+G` -> system cursor reappears, red cursor stays in place.

use kanal::AsyncSender;
use libmks_rs::{
    dbus::{
        console::{self, ConsoleController},
        keyboard::KeyboardController,
        listener::Event,
        mouse::{self, MouseController},
    },
    display::vm_display::{GrabShortcut, InputMode, ScalingMode, VmDisplayInit, VmDisplayModel},
};
use log::info;
use relm4::{Controller, gtk::prelude::*, prelude::*};
use std::time::Duration;

struct AppModel {
    _display: Controller<VmDisplayModel>,
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
            set_title: Some("VM Display: Dual-Mode Input Test"),
            set_default_width: 1024,
            set_default_height: 768,

            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,

                // --- Toolbar ---
                gtk::Box {
                    set_orientation: gtk::Orientation::Horizontal,
                    set_spacing: 10,
                    set_margin_all: 10,

                    // Scaling Mode Dropdown
                    gtk::Label {
                        set_label: "Scaling:",
                    },
                    #[name = "scale_dropdown"]
                    gtk::DropDown {
                        set_model: Some(&gtk::StringList::new(&[
                            "Resize Guest (Auto)",
                            "Fixed Guest (Scaled)",
                        ])),
                        set_selected: 0,
                    },

                    gtk::Separator {
                        set_orientation: gtk::Orientation::Vertical,
                    },

                    // Input Mode Dropdown
                    gtk::Label {
                        set_label: "Input:",
                    },
                    #[name = "input_dropdown"]
                    gtk::DropDown {
                        set_model: Some(&gtk::StringList::new(&[
                            "Seamless (Office/Tablet)",
                            "Locked (Gaming/FPS)",
                        ])),
                        set_selected: 0, // Matches initial confine_state: None (Seamless mode)
                    },

                    gtk::Label {
                        set_label: "💡 Hint: In Locked mode, click to capture, Ctrl+Alt+G to release.",
                        set_opacity: 0.6,
                        set_margin_start: 10,
                    },
                },

                // --- Display Area ---
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

        let _display = VmDisplayModel::builder()
            .launch(VmDisplayInit {
                rx,
                console_ctrl,
                mouse_ctrl,
                keyboard_ctrl: kbd_ctrl,
                grab_shortcut: GrabShortcut::default(),
            })
            .forward(sender.input_sender(), |_| AppMsg::Ignore);

        let display_widget = _display.widget().clone();

        // Start enhanced Mock Backend with relative motion cursor tracking
        tokio::spawn(mock_qemu_backend(tx, mouse_rx, console_rx, kbd_rx));

        let model = AppModel { _display };
        let widgets = view_output!();

        // 1. Scaling Mode Logic
        let sender_clone = sender.clone();
        widgets.scale_dropdown.connect_selected_item_notify(move |dropdown| {
            let mode = match dropdown.selected() {
                0 => ScalingMode::ResizeGuest,
                1 => ScalingMode::FixedGuest,
                _ => return,
            };
            info!("UI: Switching scaling mode to {:?}", mode);
            sender_clone.input(AppMsg::SetScalingMode(mode));
        });

        // 2. Input Mode Logic (NEW)
        let sender_clone = sender.clone();
        widgets.input_dropdown.connect_selected_item_notify(move |dropdown| {
            let mode = match dropdown.selected() {
                0 => InputMode::Seamless,
                1 => InputMode::Confined,
                _ => return,
            };
            info!("UI: Switching input mode to {:?}", mode);
            sender_clone.input(AppMsg::SetInputMode(mode));
        });

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>) {
        match msg {
            AppMsg::SetScalingMode(mode) => {
                self._display.emit(libmks_rs::display::vm_display::Message::SetScalingMode(mode));
            }
            AppMsg::SetInputMode(mode) => {
                // Forward mode switch to VmDisplayModel
                self._display.emit(libmks_rs::display::vm_display::Message::SetInputCaptureMode(mode));
            }
            AppMsg::Ignore => {}
        }
    }
}

// --- Controller Helpers ---

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

// --- Psychedelic Backend Logic ---

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

    // 1. 初始化光标 (白色大号 FPS 风格)
    // -------------------------------------------------------------
    // 增大尺寸到 128x128
    let cursor_w = 128;
    let cursor_h = 128;
    let cx = 64i32; // 中心点 X
    let cy = 64i32; // 中心点 Y

    // 样式参数 (按比例放大)
    let line_length = 24; // 十字线长度 (之前是12)
    let line_width = 3; // 线条半宽 (总宽 = 1 + 2*width = 7px)
    let circle_radius = 6; // 中心圆半径 (直径 13px)
    let border = 2; // 描边宽度 (更粗的黑边，增强白色在亮背景下的对比度)

    let mut cursor_data = vec![0u8; (cursor_w * cursor_h * 4) as usize];

    for y in 0..cursor_h {
        for x in 0..cursor_w {
            let offset = ((y * cursor_w + x) * 4) as usize;

            let dx = x as i32 - cx;
            let dy = y as i32 - cy;
            let dist_sq = dx * dx + dy * dy;

            // --- 形状判定 ---

            // 1. 中心圆
            let in_circle = dist_sq <= (circle_radius * circle_radius);

            // 2. 十字线
            let in_horz_line = dx.abs() <= line_length && dy.abs() <= line_width; // 粗线
            let in_vert_line = dy.abs() <= line_length && dx.abs() <= line_width; // 粗线

            let is_shape = in_circle || in_horz_line || in_vert_line;

            // --- 描边判定 ---
            let b_radius = circle_radius + border;
            let in_circle_border = dist_sq <= (b_radius * b_radius);

            let b_width = line_width + border;
            let b_len = line_length + border;
            let in_horz_border = dx.abs() <= b_len && dy.abs() <= b_width;
            let in_vert_border = dy.abs() <= b_len && dx.abs() <= b_width;

            let is_border = in_circle_border || in_horz_border || in_vert_border;

            // --- 颜色填充 ---
            if is_shape {
                // ✅ 改为白色
                cursor_data[offset..offset + 4].copy_from_slice(&[255, 255, 255, 255]);
            } else if is_border {
                // 黑色描边 (保持不变，用于对比)
                cursor_data[offset..offset + 4].copy_from_slice(&[0, 0, 0, 255]);
            } else {
                // 透明背景
                cursor_data[offset..offset + 4].copy_from_slice(&[0, 0, 0, 0]);
            }
        }
    }

    tx.send(Event::CursorDefine { width: cursor_w, height: cursor_h, hot_x: cx, hot_y: cy, data: cursor_data.into() })
        .await
        .ok();
    // -------------------------------------------------------------

    // 2. 状态变量
    let mut current_w = 800u32;
    let mut current_h = 600u32;
    let mut frame_timer = tokio::time::interval(Duration::from_millis(16));
    let mut time_offset = 0u32;

    // 虚拟光标位置
    let mut v_cursor_x = 400i32;
    let mut v_cursor_y = 300i32;

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
                        log::info!("🖱️ Mock: Abs Move to x={}, y={}", x, y);
                        v_cursor_x = x as i32;
                        v_cursor_y = y as i32;
                        tx.send(Event::MouseSet { x: v_cursor_x, y: v_cursor_y, on: 1 }).await.ok();
                    }
                    mouse::Command::RelMotion { dx, dy } => {
                        log::info!("🖱️ Mock: Rel Move by dx={}, dy={}", dx, dy);
                        v_cursor_x = (v_cursor_x + dx).clamp(0, current_w as i32);
                        v_cursor_y = (v_cursor_y + dy).clamp(0, current_h as i32);
                        tx.send(Event::MouseSet { x: v_cursor_x, y: v_cursor_y, on: 1 }).await.ok();
                    }
                    mouse::Command::Press(btn) => {
                        log::info!("🖱️ Mock: Button {:?} Pressed", btn);
                    }
                    mouse::Command::Release(btn) => {
                        log::info!("🖱️ Mock: Button {:?} Released", btn);
                    }
                }
            }

            Ok(cmd) = console_rx.recv() => {
                if let console::Command::SetUiInfo { width, height, .. } = cmd {
                    if width > 0 && height > 0 {
                        current_w = width;
                        current_h = height;
                        v_cursor_x = (current_w / 2) as i32;
                        v_cursor_y = (current_h / 2) as i32;
                    }
                }
            }

            Ok(_) = kbd_rx.recv() => {}
        }
    }
}

fn main() {
    env_logger::Builder::from_default_env().filter_level(log::LevelFilter::Info).init();
    let app = RelmApp::new("com.falcon.display.dual_mode");
    app.run::<AppModel>(());
}
