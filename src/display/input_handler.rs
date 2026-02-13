use crate::{
    dbus::{
        keyboard::KeyboardController,
        mouse::{Button, MouseController},
    },
    display::{coord::CoordinateSystem, vm_display::InputMode},
    keymaps::Qnum,
};
use log::{error, warn};

#[derive(Clone)]
pub struct InputHandler {
    scroll_accumulator_y: f64,
    pub mouse_ctrl: MouseController,
    pub keyboard_ctrl: KeyboardController,
}

impl InputHandler {
    pub fn new(mouse_ctrl: MouseController, keyboard_ctrl: KeyboardController) -> Self {
        Self { scroll_accumulator_y: 0., mouse_ctrl, keyboard_ctrl }
    }

    #[inline]
    pub const fn should_forward(&self, mode: InputMode, is_captured: bool, is_mouse_over: bool) -> bool {
        match mode {
            InputMode::Confined => is_captured,
            InputMode::Seamless => is_mouse_over,
        }
    }

    #[inline]
    pub fn move_mouse_to(&mut self, x: f32, y: f32, coord: &CoordinateSystem) {
        if let Some((x, y)) = coord.widget_to_guest(x, y)
            && let Err(e) = self.mouse_ctrl.try_set_abs_position(x, y)
        {
            warn!(error:? = e; "Lost mouse move event")
        };
    }

    pub fn scroll_mouse(&mut self, dy: f64) -> i64 {
        self.scroll_accumulator_y += dy;
        let scroll_accumulator_y = self.scroll_accumulator_y;
        let steps = scroll_accumulator_y.trunc() as i64;
        self.scroll_accumulator_y -= steps as f64;
        steps
    }

    pub async fn send_scroll_events(&self, steps: i64) {
        for _ in 0..steps.abs() {
            let btn = if steps.is_positive() {
                Button::WheelDown
            } else {
                Button::WheelUp
            };
            if let Err(e) = self.mouse_ctrl.press(btn).await {
                error!(error:? = e; "Failed to press mouse button")
            };
            if let Err(e) = self.mouse_ctrl.release(btn).await {
                error!(error:? = e; "Failed to release mouse button")
            };
        }
    }

    /// 处理键盘事件
    pub async fn press_keyboard(&self, keycode: u32, pressed: bool) {
        let qnum = Qnum::from_xorg_keycode(keycode);
        if pressed {
            if let Err(e) = self.keyboard_ctrl.press(qnum).await {
                error!(error:? = e; "Failed to press keyboard key")
            };
        } else {
            if let Err(e) = self.keyboard_ctrl.release(qnum).await {
                error!(error:? = e; "Failed to release keyboard key")
            };
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
}
