use libmks_rs::display::{pixman_4cc::Pixman, software_rasterizer::Swapchain};
use relm4::{
    ComponentParts, ComponentSender, RelmWidgetExt, SimpleComponent,
    gtk::{self, gdk::Texture, glib, prelude::*},
};
use std::{
    cmp::{max, min},
    num::NonZeroU32,
    time::Duration,
};

const PIXMAN_FORMAT_A8R8G8B8: u32 = 0x20028888;

fn nz(value: u32) -> NonZeroU32 { NonZeroU32::new(value).expect("example always uses non-zero dimensions") }

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
            // Use a fast speed so tearing is easier to spot.
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
                .full_update_texture(nz(canvas_w), nz(canvas_h), nz(stride), format, &full_buf)
                .expect("Initialization failed. Check that /dev/udmabuf exists and is accessible."),
        );

        // Drive the demo at roughly 60 FPS.
        // Shorter intervals make tearing and bandwidth pressure easier to observe.
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

                // [Core safeguard] Keep the box inside the current canvas bounds.
                let max_allowed_x = self.canvas_w.saturating_sub(box_w) as i32;
                let max_allowed_y = self.canvas_h.saturating_sub(box_h) as i32;

                // Clamp stale coordinates before applying the next step.
                if self.box_x > max_allowed_x {
                    self.box_x = max_allowed_x;
                    // If we had drifted out of bounds, steer back into the canvas.
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

                // The old position is valid after the clamp above.
                let old_x = self.box_x;
                let old_y = self.box_y;

                // Maximum legal X position.
                let max_x = self.canvas_w.saturating_sub(box_w) as i32;

                if max_x == 0 {
                    // The canvas is narrower than the box, so pin it to the origin.
                    self.box_x = 0;
                } else {
                    self.box_x += self.velocity_x;

                    // Bounce on the horizontal bounds.
                    if self.box_x <= 0 {
                        self.box_x = 0;
                        self.velocity_x = self.velocity_x.abs(); // Move right.
                    } else if self.box_x >= max_x {
                        self.box_x = max_x;
                        self.velocity_x = -self.velocity_x.abs(); // Move left.
                    }
                }

                // Maximum legal Y position.
                let max_y = self.canvas_h.saturating_sub(box_h) as i32;

                if max_y == 0 {
                    // The canvas is shorter than the box, so pin it to the origin.
                    self.box_y = 0;
                } else {
                    self.box_y += self.velocity_y;

                    // Bounce on the vertical bounds.
                    if self.box_y <= 0 {
                        self.box_y = 0;
                        self.velocity_y = self.velocity_y.abs(); // Move down.
                    } else if self.box_y >= max_y {
                        self.box_y = max_y;
                        self.velocity_y = -self.velocity_y.abs(); // Move up.
                    }
                }
                // Cover both the old and new box positions so erase and redraw stay atomic.
                let min_x = min(old_x, self.box_x);
                let min_y = min(old_y, self.box_y);
                let max_x = max(old_x + box_w as i32, self.box_x + box_w as i32);
                let max_y = max(old_y + box_h as i32, self.box_y + box_h as i32);

                let dirty_x = min_x as u32;
                let dirty_y = min_y as u32;
                let dirty_w = (max_x - min_x) as u32;
                let dirty_h = (max_y - min_y) as u32;
                let dirty_stride = dirty_w * 4;

                // Build the dirty-region staging buffer.
                let mut dirty_buf = vec![0u8; (dirty_stride * dirty_h) as usize];

                // Pass the dirty-region origin so the background grid stays aligned.
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

                // Submit the partial update.
                let format = Pixman::from(PIXMAN_FORMAT_A8R8G8B8);
                match self.swapchain.partial_update_texture(
                    dirty_x,
                    dirty_y,
                    nz(dirty_w),
                    nz(dirty_h),
                    nz(dirty_stride),
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

/// Draws one dirty rectangle by classifying each pixel as box or background.
#[allow(clippy::too_many_arguments)]
fn fill_dirty_area(
    buf: &mut [u8],
    dx: u32,
    dy: u32,
    dw: u32,
    dh: u32,
    d_stride: u32, // Dirty-rect stride in bytes.
    bx: u32,
    by: u32,
    bw: u32,
    bh: u32, // Box dimensions.
    _canvas_w: u32,
    _canvas_h: u32, // Canvas dimensions for grid alignment.
    frame: u64,
) {
    for y in 0..dh {
        for x in 0..dw {
            // Current pixel in full-canvas coordinates.
            let global_x = dx + x;
            let global_y = dy + y;

            // Check whether this pixel falls inside the moving box.
            let in_box = global_x >= bx && global_x < (bx + bw) && global_y >= by && global_y < (by + bh);

            let pixel_offset = (y * d_stride + x * 4) as usize;

            if in_box {
                // Animated diagonals make tearing obvious.
                let pattern = (global_x + global_y + (frame as u32 * 8)) % 64;

                let (r, g, b) = if pattern < 32 {
                    (255, 50, 50) // Red stripe.
                } else {
                    (255, 255, 255) // White stripe.
                };

                buf[pixel_offset] = b;
                buf[pixel_offset + 1] = g;
                buf[pixel_offset + 2] = r;
                buf[pixel_offset + 3] = 255;
            } else {
                // Restore the aligned grid behind the moving box.
                let grid_size = 50;
                let on_line = global_x.is_multiple_of(grid_size) || global_y.is_multiple_of(grid_size);

                let val = if on_line { 100 } else { 30 }; // Dark gray fill with lighter grid lines.

                buf[pixel_offset] = val; // B.
                buf[pixel_offset + 1] = val; // G.
                buf[pixel_offset + 2] = val; // R.
                buf[pixel_offset + 3] = 255; // A.
            }
        }
    }
}

/// Fills the initial full-frame background.
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
