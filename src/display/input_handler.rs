use super::coordinate::Coordinate;
use crate::{
    dbus::{
        keyboard::{KeyboardController, KeyboardSession},
        mouse::{Button, MouseController, MouseSession},
        multitouch::{Kind, MultiTouchController, MultiTouchSession},
    },
    keymaps::Qnum,
    mks_debug, mks_error, mks_warn,
};
use typed_builder::TypedBuilder;

const LOG_TARGET: &str = "mks.display.input";

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
}

impl InputHandler {
    #[inline]
    pub fn mouse_ctrl(&self) -> Option<&MouseController> { self.mouse.as_ref().map(|(ctrl, _)| ctrl) }

    #[inline]
    pub fn keyboard_ctrl(&self) -> Option<&KeyboardController> { self.keyboard.as_ref().map(|(ctrl, _)| ctrl) }

    #[inline]
    pub fn multitouch_ctrl(&self) -> Option<&MultiTouchController> { self.multitouch.as_ref().map(|(ctrl, _)| ctrl) }

    #[inline]
    pub const fn set_mouse_mode(&mut self, is_absolute: bool) { self.is_absolute = is_absolute; }

    #[inline]
    pub fn move_mouse_to(&mut self, widget_x: f32, widget_y: f32, coord: &Coordinate) {
        let Some(ctrl) = self.mouse_ctrl() else {
            mks_warn!("No mouse controller available");
            return;
        };
        mks_debug!("move_mouse_to: widget=({:.1}, {:.1}), is_absolute={}", widget_x, widget_y, self.is_absolute);
        if self.is_absolute {
            let Some((guest_x, guest_y)) = coord.widget_to_guest(widget_x, widget_y) else {
                mks_warn!("failed map widget pos to guest pos: {widget_x},{widget_y}");
                return;
            };
            if let Err(e) = ctrl.try_set_abs_position(guest_x, guest_y) {
                mks_warn!(error:? = e; "Lost mouse absolute move event")
            }
            return;
        }
        mks_debug!("Ignoring widget motion in relative mode; expecting native Wayland relative events");
    }

    pub const fn cache_mouse_scroll(&mut self, dy: f64) -> i64 {
        let acc = &mut self.scroll_accumulator_y;
        *acc += dy;
        let steps = acc.trunc() as i64;
        *acc -= steps as f64;
        steps
    }

    pub fn scroll_mouse(&self, steps: i64) {
        let Some(ctrl) = self.mouse_ctrl() else {
            mks_warn!("No mouse controller available");
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
                mks_error!(error:? = e; "Failed to release mouse button");
                continue;
            };
            mks_error!(error:? = e; "Failed to press mouse button");
        }
    }

    /// 处理键盘事件
    pub fn press_keyboard(&self, keycode: u32, pressed: bool) {
        let Some(ctrl) = self.keyboard_ctrl() else {
            mks_warn!("No keyboard controller available");
            return;
        };
        let qnum = Qnum::from_xorg_keycode(keycode);
        let Err(e) = (if pressed { ctrl.press(qnum) } else { ctrl.release(qnum) }) else {
            return;
        };
        mks_error!(error:? = e; "Failed to {} keyboard key", if pressed { "press" } else { "release" });
    }

    pub fn press_mouse_button(&self, button: u32, pressed: bool) {
        let Some(ctrl) = self.mouse_ctrl() else {
            mks_warn!("No mouse controller available");
            return;
        };
        let Some(btn) = Button::from_xorg(button) else {
            mks_warn!("Unmapped mouse button {button}, ignore");
            return;
        };
        let Err(e) = (if pressed { ctrl.press(btn) } else { ctrl.release(btn) }) else {
            return;
        };
        mks_error!(error:? = e; "Failed to {} mouse button", if pressed { "press" } else { "release" });
    }

    /// 处理触摸事件
    pub fn touch(&self, kind: Kind, num_slot: u64, x: f64, y: f64) {
        use Kind::*;
        let Some(ctrl) = self.multitouch_ctrl() else {
            mks_warn!("No multitouch controller available");
            return;
        };
        let res = match kind {
            Begin => ctrl.begin(num_slot, x, y),
            Update => ctrl.update(num_slot, x, y),
            End => ctrl.end(num_slot, x, y),
            Cancel => ctrl.cancel(num_slot, x, y),
        };
        let Err(e) = res else { return };
        mks_error!(error:? = e; "Failed to send touch event");
    }
}
