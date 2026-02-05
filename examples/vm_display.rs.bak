use kanal::AsyncSender;
use libmks_rs::{
    dbus::listener::QemuEvent,
    display::vm_display::{VmDisplayInit, VmDisplayModel},
};
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
            set_title: Some("VM Display Demo (Tokio + Relm4)"),
            set_default_size: (1024, 768),
            #[local_ref]
            display_widget -> gtk::Overlay {},
        }
    }

    fn init(_: (), root: Self::Root, _sender: ComponentSender<Self>) -> ComponentParts<Self> {
        // 1. 创建通信通道 (Kanal)
        let (tx, rx) = kanal::unbounded_async::<QemuEvent>();

        // 2. 启动组件 (传入 rx)
        let _display = VmDisplayModel::builder().launch(VmDisplayInit { rx }).detach();

        // 3. 获取 Widget 的 clone（变量名必须和 view! 宏中的 #[local_ref] 一致）
        let display_widget = _display.widget().clone();

        // 4. 启动后台模拟任务
        tokio::spawn(mock_qemu_source(tx));

        // 5. 创建 model
        let model = AppModel { _display };

        // 6. 生成 widgets（此时会自动把 display_widget 填入窗口）
        let widgets = view_output!();

        ComponentParts { model, widgets }
    }
}

// ==========================================
// 2. Mock 事件源 (模拟 QEMU 行为)
// ==========================================

// 辅助函数：生成带边框的纯色背景，方便观察缩放边界
// 边框颜色根据背景自动选择互补色
fn generate_frame(width: u32, height: u32, bg_r: u8, bg_g: u8, bg_b: u8) -> Vec<u8> {
    let stride = width * 4;
    let mut data = vec![0u8; (stride * height) as usize];

    // 自动计算高对比度边框颜色（互补色）
    let border_r = 255u8.saturating_sub(bg_r);
    let border_g = 255u8.saturating_sub(bg_g);
    let border_b = 255u8.saturating_sub(bg_b);

    for y in 0..height {
        for x in 0..width {
            let offset = ((y * width + x) * 4) as usize;

            // 绘制 10px 的对比色边框
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
    // --- 1. 定义光标 (黄色，与所有背景都有高对比度) ---
    let cursor_w = 64;
    let cursor_h = 64;
    let mut cursor_data = vec![0u8; (cursor_w * cursor_h * 4) as usize];
    for i in 0..(cursor_w * cursor_h) as usize {
        let offset = i * 4;
        cursor_data[offset] = 0; // B
        cursor_data[offset + 1] = 255; // G (Yellow)
        cursor_data[offset + 2] = 255; // R (Yellow)
        cursor_data[offset + 3] = 255; // A
    }

    // 发送光标定义
    tx.send(QemuEvent::CursorDefine { width: cursor_w, height: cursor_h, hot_x: 0, hot_y: 0, data: cursor_data })
        .await
        .ok();

    // --- 状态变量 ---
    let mut current_w = 800;
    let mut current_h = 600;
    let mut mouse_x = 0;
    let mut mouse_y = 0;
    let mut frame_count: u64 = 0;

    println!("Simulation Started: Phase 1 - 800x600 (Blue)");

    // 发送初始帧
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
        tokio::time::sleep(Duration::from_millis(16)).await; // ~60 FPS
        frame_count += 1;

        // 简单的鼠标移动逻辑 (弹球效果)
        mouse_x = (mouse_x + 5) % current_w as i32;
        mouse_y = (mouse_y + 3) % current_h as i32;

        // 只有在非 Disable 状态下发送鼠标移动
        if !(360..=480).contains(&frame_count) {
            tx.send(QemuEvent::MouseSet { x: mouse_x, y: mouse_y, on: 1 }).await.ok();
        }

        // === 时间轴控制 ===

        // [Phase 2] 第 180 帧 (约3秒): 切换分辨率到 1280x720 (16:9) -> 绿色
        if frame_count == 180 {
            println!("Phase 2: Resize to 1280x720 (Green) - Check Letterboxing (上下黑边)");
            current_w = 1280;
            current_h = 720;
            mouse_x = 0; // 重置鼠标
            mouse_y = 0;

            let bg = generate_frame(current_w, current_h, 0, 255, 0); // 绿色
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

        // [Phase 3] 第 360 帧 (约6秒): Disable 事件 -> 黑屏
        if frame_count == 360 {
            println!("Phase 3: Disable Event - Screen should be cleared");
            tx.send(QemuEvent::Disable).await.ok();
        }

        // [Phase 4] 第 480 帧 (约8秒): 恢复并切换到 400x600 (2:3) -> 红色
        if frame_count == 480 {
            println!("Phase 4: Re-enable 400x600 (Red) - Check Pillarboxing (左右黑边)");
            current_w = 400;
            current_h = 600;
            mouse_x = 0;
            mouse_y = 0;

            let bg = generate_frame(current_w, current_h, 255, 0, 0); // 红色
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

// ==========================================
// 3. 入口
// ==========================================
fn main() {
    let app = RelmApp::new("com.falcon.display.demo");
    app.run::<AppModel>(());
}
