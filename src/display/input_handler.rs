use crate::{
    dbus::{
        keyboard::KeyboardController,
        mouse::{Button, MouseController},
        multitouch::{Kind, MultiTouchController},
    },
    display::coordinate::Coordinate,
    keymaps::Qnum,
};
use log::{error, warn};
use typed_builder::TypedBuilder;

#[derive(Clone, TypedBuilder)]
pub struct InputHandler {
    #[builder(default = 0.)]
    scroll_accumulator_y: f64,
    #[builder(default, setter(strip_option))]
    pub mouse: Option<MouseController>,
    #[builder(default, setter(strip_option))]
    pub keyboard: Option<KeyboardController>,
    #[builder(default, setter(strip_option))]
    pub multitouch: Option<MultiTouchController>,
}

impl InputHandler {
    #[inline]
    pub fn move_mouse_to(&mut self, x: f32, y: f32, coord: &Coordinate) {
        if let Some(ctrl) = &self.mouse
            && let Some((x, y)) = coord.widget_to_guest(x, y)
            && let Err(e) = ctrl.try_set_abs_position(x, y)
        {
            warn!(error:? = e; "Lost mouse move event")
        }
    }

    pub const fn cache_mouse_scroll(&mut self, dy: f64) -> i64 {
        let acc = &mut self.scroll_accumulator_y;
        *acc += dy;
        let steps = acc.trunc() as i64;
        *acc -= steps as f64;
        steps
    }

    pub async fn scroll_mouse(&self, steps: i64) {
        if let Some(ctrl) = &self.mouse {
            for _ in 0..steps.abs() {
                let btn = if steps.is_positive() {
                    Button::WheelDown
                } else {
                    Button::WheelUp
                };
                if let Err(e) = ctrl.press(btn).await {
                    error!(error:? = e; "Failed to press mouse button")
                };
                if let Err(e) = ctrl.release(btn).await {
                    error!(error:? = e; "Failed to release mouse button")
                };
            }
        }
    }

    /// 处理键盘事件
    pub async fn press_keyboard(&self, keycode: u32, pressed: bool) {
        if let Some(ctrl) = &self.keyboard {
            let qnum = Qnum::from_xorg_keycode(keycode);
            if pressed {
                if let Err(e) = ctrl.press(qnum).await {
                    error!(error:? = e; "Failed to press keyboard key")
                };
            } else {
                if let Err(e) = ctrl.release(qnum).await {
                    error!(error:? = e; "Failed to release keyboard key")
                };
            }
        }
    }

    pub async fn press_mouse_button(&self, button: u32, pressed: bool, mouse_ctrl: MouseController) {
        let Some(btn) = Button::from_xorg(button) else {
            warn!("Unmapped mouse button {button}, ignore");
            return;
        };
        if pressed {
            if let Err(e) = mouse_ctrl.press(btn).await {
                error!(error:? = e; "Failed to press mouse button")
            };
        } else {
            if let Err(e) = mouse_ctrl.release(btn).await {
                error!(error:? = e; "Failed to release mouse button")
            };
        };
    }

    /// 处理触摸事件
    pub async fn touch(&self, kind: Kind, num_slot: u64, x: f64, y: f64) {
        use Kind::*;
        if let Some(ctrl) = &self.multitouch {
            let res = match kind {
                Begin => ctrl.begin(num_slot, x, y).await,
                Update => ctrl.update(num_slot, x, y).await,
                End => ctrl.end(num_slot, x, y).await,
                Cancel => ctrl.cancel(num_slot, x, y).await,
            };
            if let Err(e) = res {
                error!(error:? = e; "Failed to send touch event")
            }
        }
    }
}
