use super::vm_display::InputMode::{self, *};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Capture {
    #[default]
    Idle, // 既未进入 Seamless 区域，也未在 Confined 模式下点击
    Seamless, // 处于 Seamless 模式且鼠标在画面内
    Confined, // 处于 Confined 模式且已点击捕获
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CaptureState {
    state: Capture,
}

impl CaptureState {
    pub fn new() -> Self { Self::default() }

    #[inline]
    pub const fn reset(&mut self) { self.state = Capture::Idle; }

    #[inline]
    pub fn on_mouse_enter(&mut self, mode: InputMode) {
        if mode == Seamless {
            self.state = Capture::Seamless;
        }
    }

    #[inline]
    pub const fn on_mouse_leave(&mut self) { self.reset(); }

    #[inline]
    pub fn on_click(&mut self, mode: InputMode) {
        if mode == Confined {
            self.state = Capture::Confined;
        }
    }

    #[inline]
    pub const fn on_release(&mut self) -> Capture {
        self.reset();
        self.current()
    }

    #[inline]
    pub fn should_forward(&self) -> bool {
        match self.state {
            Capture::Idle => false,
            Capture::Seamless | Capture::Confined => true,
        }
    }

    #[inline]
    pub const fn current(&self) -> Capture { self.state }
}
