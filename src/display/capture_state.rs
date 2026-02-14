use super::vm_display::InputMode::{self, *};
use Capture::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Capture {
    #[default]
    Idle,
    Hover,
    Exclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CaptureState {
    state: Capture,
}

impl CaptureState {
    pub fn new() -> Self { Self { state: Capture::Idle } }

    #[inline]
    pub fn on_mouse_enter(&mut self, mode: InputMode) -> Capture {
        if mode == Seamless {
            self.state = Hover;
        }
        self.state
    }

    #[inline]
    pub fn on_mouse_leave(&mut self, mode: InputMode) -> Capture {
        if mode == Seamless {
            self.state = Idle;
        }
        self.state
    }

    #[inline]
    pub fn on_click(&mut self, mode: InputMode) -> Capture {
        if mode == Confined {
            self.state = Exclusive;
        }
        self.state
    }

    #[inline]
    pub fn on_release(&mut self) -> Capture {
        self.state = Idle;
        self.state
    }

    #[inline]
    pub fn should_forward(&self, mode: InputMode) -> bool {
        match self.state {
            Idle => false,
            Hover => mode == Seamless,
            Exclusive => true,
        }
    }

    #[inline]
    pub const fn current(&self) -> Capture { self.state }
}
