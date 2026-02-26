use crate::{
    dbus::{
        keyboard::{KeyboardController, KeyboardSession},
        mouse::{Button, MouseController, MouseSession},
        multitouch::{Kind, MultiTouchController, MultiTouchSession},
    },
    display::coordinate::Coordinate,
    keymaps::Qnum,
};
use log::{debug, error, warn};
use typed_builder::TypedBuilder;

#[derive(TypedBuilder)]
pub struct InputHandler {
    #[builder(default = 0.)]
    scroll_accumulator_y: f64,
    #[builder(default)]
    pub mouse: Option<(MouseController, MouseSession)>,
    #[builder(default)]
    pub keyboard: Option<(KeyboardController, KeyboardSession)>,
    #[builder(default)]
    pub multitouch: Option<(MultiTouchController, MultiTouchSession)>,
    #[builder(default = true)]
    pub is_absolute: bool,
    /// 保存控件上光标的位置用于计算相对向量
    #[builder(default = None)]
    last_widget_cursor: Option<(f32, f32)>,
}

impl InputHandler {
    #[inline]
    pub fn mouse_ctrl(&self) -> Option<&MouseController> { self.mouse.as_ref().map(|(ctrl, _)| ctrl) }

    #[inline]
    pub fn keyboard_ctrl(&self) -> Option<&KeyboardController> { self.keyboard.as_ref().map(|(ctrl, _)| ctrl) }

    #[inline]
    pub fn multitouch_ctrl(&self) -> Option<&MultiTouchController> { self.multitouch.as_ref().map(|(ctrl, _)| ctrl) }

    #[inline]
    pub fn move_mouse_to(&mut self, widget_x: f32, widget_y: f32, coord: &Coordinate) {
        let Some(ctrl) = self.mouse_ctrl() else {
            warn!("No mouse controller available");
            return;
        };
        debug!(
            "move_mouse_to: widget=({:.1}, {:.1}), last={:?}, is_absolute={}",
            widget_x, widget_y, self.last_widget_cursor, self.is_absolute
        );
        if self.is_absolute {
            let Some((guest_x, guest_y)) = coord.widget_to_guest(widget_x, widget_y) else {
                warn!("failed map widget pos to guest pos: {widget_x},{widget_y}");
                return;
            };
            if let Err(e) = ctrl.try_set_abs_position(guest_x, guest_y) {
                warn!(error:? = e; "Lost mouse absolute move event")
            }
        } else {
            // 相对模式：直接使用 widget 坐标系的 delta，不做缩放转换
            // 相对移动是 delta 值，用户移动多少像素就发送给 guest 多少像素
            if let Some((last_x, last_y)) = self.last_widget_cursor {
                let dx = widget_x - last_x;
                let dy = widget_y - last_y;
                let int_dx = dx.trunc() as i32;
                let int_dy = dy.trunc() as i32;
                if int_dx != 0 || int_dy != 0 {
                    debug!("Sending rel_motion({int_dx}, {int_dy})");
                    if let Err(e) = ctrl.rel_motion(int_dx, int_dy) {
                        warn!(error:? = e; "Lost mouse relative move event")
                    }
                    // 仅扣除已经发送的整数部分，保留亚像素精度
                    self.last_widget_cursor = Some((last_x + int_dx as f32, last_y + int_dy as f32));
                } else {
                    debug!("Accumulated but not sent (dx={:.2}, dy={:.2})", dx, dy);
                }
            } else {
                self.last_widget_cursor = Some((widget_x, widget_y));
            }
        }
    }

    /// 用于重置
    pub fn reset_widget_cursor_pos(&mut self) { self.last_widget_cursor = None; }

    pub const fn cache_mouse_scroll(&mut self, dy: f64) -> i64 {
        let acc = &mut self.scroll_accumulator_y;
        *acc += dy;
        let steps = acc.trunc() as i64;
        *acc -= steps as f64;
        steps
    }

    pub fn scroll_mouse(&self, steps: i64) {
        let Some(ctrl) = self.mouse_ctrl() else {
            warn!("No mouse controller available");
            return;
        };
        for _ in 0..steps.abs() {
            let btn = if steps.is_positive() {
                Button::WheelDown
            } else {
                Button::WheelUp
            };
            let Err(e) = ctrl.press(btn) else {
                let Err(e) = ctrl.release(btn) else { continue };
                error!(error:? = e; "Failed to release mouse button");
                continue;
            };
            error!(error:? = e; "Failed to press mouse button");
        }
    }

    /// 处理键盘事件
    pub fn press_keyboard(&self, keycode: u32, pressed: bool) {
        let Some(ctrl) = self.keyboard_ctrl() else {
            warn!("No keyboard controller available");
            return;
        };
        let qnum = Qnum::from_xorg_keycode(keycode);
        let Err(e) = (if pressed { ctrl.press(qnum) } else { ctrl.release(qnum) }) else {
            return;
        };
        error!(error:? = e; "Failed to {} keyboard key", if pressed { "press" } else { "release" });
    }

    pub fn press_mouse_button(&self, button: u32, pressed: bool) {
        let Some(ctrl) = self.mouse_ctrl() else {
            warn!("No mouse controller available");
            return;
        };
        let Some(btn) = Button::from_xorg(button) else {
            warn!("Unmapped mouse button {button}, ignore");
            return;
        };
        let Err(e) = (if pressed { ctrl.press(btn) } else { ctrl.release(btn) }) else {
            return;
        };
        error!(error:? = e; "Failed to {} mouse button", if pressed { "press" } else { "release" });
    }

    /// 处理触摸事件
    pub fn touch(&self, kind: Kind, num_slot: u64, x: f64, y: f64) {
        use Kind::*;
        let Some(ctrl) = self.multitouch_ctrl() else {
            warn!("No multitouch controller available");
            return;
        };
        let res = match kind {
            Begin => ctrl.begin(num_slot, x, y),
            Update => ctrl.update(num_slot, x, y),
            End => ctrl.end(num_slot, x, y),
            Cancel => ctrl.cancel(num_slot, x, y),
        };
        let Err(e) = res else { return };
        error!(error:? = e; "Failed to send touch event");
    }
}
