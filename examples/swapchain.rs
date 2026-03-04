use libmks_rs::display::{pixman_4cc::Pixman, software_rasterizer::Swapchain};
use relm4::{
    ComponentParts, ComponentSender, RelmWidgetExt, SimpleComponent,
    gtk::{self, gdk::Texture, glib, prelude::*},
};
use std::{
    cmp::{max, min},
    time::Duration,
};

const PIXMAN_FORMAT_A8R8G8B8: u32 = 0x20028888;

struct AppModel {
    swapchain: Swapchain,
    current_texture: Option<Texture>,
    canvas_w: u32,
    canvas_h: u32,
    box_x: i32,
    box_y: i32,
    velocity_x: i32,
    velocity_y: i32,
    frame_count: u64,
}

#[derive(Debug)]
enum AppMsg {
    Tick,
}

#[relm4::component]
impl SimpleComponent for AppModel {
    type Init = ();
    type Input = AppMsg;
    type Output = ();

    view! {
        gtk::Window {
            set_title: Some("UDMABUF Tearing Test - High Speed"),
            set_default_width: 800,
            set_default_height: 600,

            gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_spacing: 0,
                set_margin_all: 0,

                #[name = "picture"]
                gtk::Picture {
                    set_hexpand: true,
                    set_vexpand: true,
                    set_content_fit: gtk::ContentFit::Fill,

                    #[watch]
                    set_paintable: model.current_texture.as_ref(),
                }
            }
        }
    }

    fn init(_: Self::Init, root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let canvas_w = 800;
        let canvas_h = 600;

        let mut model = AppModel {
            swapchain: Swapchain::new(),
            current_texture: None,
            canvas_w,
            canvas_h,
            box_x: 0,
            box_y: 0,
            // 设置一个较快的速度来引发撕裂
            velocity_x: 15,
            velocity_y: 8,
            frame_count: 0,
        };

        let stride = canvas_w * 4;
        let mut full_buf = vec![0u8; (stride * canvas_h) as usize];
        draw_pattern(&mut full_buf, 0, 0, canvas_w, canvas_h, stride, canvas_w, canvas_h, 0);

        let format = Pixman::from(PIXMAN_FORMAT_A8R8G8B8);
        model.current_texture = Some(
            model
                .swapchain
                .full_update_texture(canvas_w, canvas_h, stride, format, &full_buf)
                .expect("初始化失败！请检查 /dev/udmabuf 是否存在以及权限是否正确"),
        );

        // 2. 启动高频定时器
        // 16ms ~ 60fps. 如果你想测试极致性能和撕裂，可以尝试更短的时间 (如 8ms)
        glib::timeout_add_local(Duration::from_millis(16), move || {
            sender.input(AppMsg::Tick);
            glib::ControlFlow::Continue
        });

        let widgets = view_output!();
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>) {
        match msg {
            AppMsg::Tick => {
                self.frame_count += 1;
                let box_w = 200;
                let box_h = 200;

                // --- [修复核心]：动态修正位置，防止被困在墙外 ---
                // 计算当前画布允许的最大坐标 (如果画布比方块还小，则最大坐标为0)
                let max_allowed_x = self.canvas_w.saturating_sub(box_w) as i32;
                let max_allowed_y = self.canvas_h.saturating_sub(box_h) as i32;

                // 强制将坐标拉回合法范围内 (Clamp)
                if self.box_x > max_allowed_x {
                    self.box_x = max_allowed_x;
                    // 可选：如果在墙外被拉回，通常意味着撞墙了，确保速度指向内部
                    if self.velocity_x > 0 {
                        self.velocity_x = -self.velocity_x;
                    }
                }
                if self.box_y > max_allowed_y {
                    self.box_y = max_allowed_y;
                    if self.velocity_y > 0 {
                        self.velocity_y = -self.velocity_y;
                    }
                }
                // ---------------------------------------------

                // 1. 记录旧位置 (现在的位置一定是合法的了)
                let old_x = self.box_x;
                let old_y = self.box_y;

                // 计算 X 轴最大允许坐标
                let max_x = self.canvas_w.saturating_sub(box_w) as i32;

                if max_x == 0 {
                    // 空间不足（窗口比方块窄）：强制固定在 0，不更新速度
                    self.box_x = 0;
                } else {
                    // 空间充足：正常移动
                    self.box_x += self.velocity_x;

                    // X 轴反弹检测
                    if self.box_x <= 0 {
                        self.box_x = 0;
                        self.velocity_x = self.velocity_x.abs(); // 向右
                    } else if self.box_x >= max_x {
                        self.box_x = max_x;
                        self.velocity_x = -self.velocity_x.abs(); // 向左
                    }
                }

                // 计算 Y 轴最大允许坐标
                let max_y = self.canvas_h.saturating_sub(box_h) as i32;

                if max_y == 0 {
                    // 空间不足：强制固定在 0
                    self.box_y = 0;
                } else {
                    // 空间充足：正常移动
                    self.box_y += self.velocity_y;

                    // Y 轴反弹检测
                    if self.box_y <= 0 {
                        self.box_y = 0;
                        self.velocity_y = self.velocity_y.abs(); // 向下
                    } else if self.box_y >= max_y {
                        self.box_y = max_y;
                        self.velocity_y = -self.velocity_y.abs(); // 向上
                    }
                }
                // 3. 计算脏矩形 (Dirty Rect)
                // 必须覆盖 旧位置 (用于擦除) 和 新位置 (用于绘制)
                // 这样我们可以一次性提交，保证原子性，同时测试 partial_update 对大块数据的处理
                let min_x = min(old_x, self.box_x);
                let min_y = min(old_y, self.box_y);
                let max_x = max(old_x + box_w as i32, self.box_x + box_w as i32);
                let max_y = max(old_y + box_h as i32, self.box_y + box_h as i32);

                let dirty_x = min_x as u32;
                let dirty_y = min_y as u32;
                let dirty_w = (max_x - min_x) as u32;
                let dirty_h = (max_y - min_y) as u32;
                let dirty_stride = dirty_w * 4;

                // 4. 准备脏数据缓冲区
                let mut dirty_buf = vec![0u8; (dirty_stride * dirty_h) as usize];

                // 5. 在脏缓冲区内绘图
                // 我们传递 dirty_x/y 作为偏移量，以便生成正确的背景纹理（对齐网格）
                fill_dirty_area(
                    &mut dirty_buf,
                    dirty_x,
                    dirty_y,
                    dirty_w,
                    dirty_h,
                    dirty_stride,
                    self.box_x as u32,
                    self.box_y as u32,
                    box_w,
                    box_h,
                    self.canvas_w,
                    self.canvas_h,
                    self.frame_count,
                );

                // 6. 提交更新
                let format = Pixman::from(PIXMAN_FORMAT_A8R8G8B8);
                match self.swapchain.partial_update_texture(
                    dirty_x,
                    dirty_y,
                    dirty_w,
                    dirty_h,
                    dirty_stride,
                    format,
                    &dirty_buf,
                ) {
                    Ok(tex) => self.current_texture = Some(tex),
                    Err(e) => eprintln!("Update failed: {:?}", e),
                }
            }
        }
    }
}

/// 核心绘图逻辑：在脏矩形内，判断像素属于“方块”还是“背景”
#[allow(clippy::too_many_arguments)]
fn fill_dirty_area(
    buf: &mut [u8],
    dx: u32,
    dy: u32,
    dw: u32,
    dh: u32,
    d_stride: u32, // Dirty Rect 参数
    bx: u32,
    by: u32,
    bw: u32,
    bh: u32, // Box 参数
    _canvas_w: u32,
    _canvas_h: u32, // 画布参数 (用于生成对齐的网格)
    frame: u64,
) {
    for y in 0..dh {
        for x in 0..dw {
            // 当前像素在全局画布的坐标
            let global_x = dx + x;
            let global_y = dy + y;

            // 判断当前像素是否在 移动的方块 内部
            let in_box = global_x >= bx && global_x < (bx + bw) && global_y >= by && global_y < (by + bh);

            let pixel_offset = (y * d_stride + x * 4) as usize;

            if in_box {
                // === 绘制方块：动态斜线 ===
                // 斜线最容易看出撕裂 (Tearing)
                // 如果画面撕裂，斜线会断开或错位
                let pattern = (global_x + global_y + (frame as u32 * 8)) % 64;

                let (r, g, b) = if pattern < 32 {
                    (255, 50, 50) // 红色条纹
                } else {
                    (255, 255, 255) // 白色条纹
                };

                buf[pixel_offset] = b;
                buf[pixel_offset + 1] = g;
                buf[pixel_offset + 2] = r;
                buf[pixel_offset + 3] = 255;
            } else {
                // === 绘制背景：恢复网格 ===
                // 如果 sync_active_to_shadow 有问题，这里恢复的背景会和未更新区域的背景对不上
                let grid_size = 50;
                let on_line = global_x.is_multiple_of(grid_size) || global_y.is_multiple_of(grid_size);

                let val = if on_line { 100 } else { 30 }; // 深灰背景 + 浅灰网格

                buf[pixel_offset] = val; // B
                buf[pixel_offset + 1] = val; // G
                buf[pixel_offset + 2] = val; // R
                buf[pixel_offset + 3] = 255; // A
            }
        }
    }
}

/// 辅助函数：初始化全屏背景用
#[allow(clippy::too_many_arguments)]
fn draw_pattern(
    buf: &mut [u8], dx: u32, dy: u32, dw: u32, dh: u32, d_stride: u32, canvas_w: u32, canvas_h: u32, frame: u64,
) {
    fill_dirty_area(buf, dx, dy, dw, dh, d_stride, 10000, 10000, 0, 0, canvas_w, canvas_h, frame);
}

fn main() {
    let app_id = "rs.libmks.swapchain";
    let app = relm4::RelmApp::new(app_id);
    app.run::<AppModel>(());
}
