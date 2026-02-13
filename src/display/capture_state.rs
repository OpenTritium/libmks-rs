//! 指针捕获状态机
use super::vm_display::InputMode::{self, *};
use CaptureState::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaptureState {
    #[default]
    Idle,
    Hover,
    Exclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CaptureStateMachine {
    state: CaptureState,
}

impl CaptureStateMachine {
    pub fn new() -> Self { Self { state: CaptureState::Idle } }

    #[inline]
    pub fn on_mouse_enter(&mut self, mode: InputMode) -> CaptureState {
        if mode == Seamless {
            self.state = Hover;
        }
        self.state
    }

    #[inline]
    pub fn on_mouse_leave(&mut self, mode: InputMode) -> CaptureState {
        if mode == Seamless {
            self.state = Idle;
        }
        self.state
    }

    #[inline]
    pub fn on_click(&mut self, mode: InputMode) -> CaptureState {
        if mode == Confined {
            self.state = Exclusive;
        }
        self.state
    }

    #[inline]
    pub fn on_release(&mut self) -> CaptureState {
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
    pub const fn current(&self) -> CaptureState { self.state }
}
