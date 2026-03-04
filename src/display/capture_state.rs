use super::vm_display::InputMode::{self, *};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
/// Current pointer capture status for display input forwarding.
pub enum Capture {
    #[default]
    Idle, // Not inside seamless region and not clicked in confined mode.
    Seamless, // Seamless mode while cursor is inside the frame.
    Confined, // Confined mode after a capture click.
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
/// Mutable state machine for pointer capture transitions.
pub struct CaptureState(Capture);

impl CaptureState {
    /// Creates a new capture state in `Capture::Idle`.
    pub fn new() -> Self { Self::default() }

    #[inline]
    /// Resets capture state to `Capture::Idle`.
    pub const fn reset(&mut self) { self.0 = Capture::Idle; }

    #[inline]
    /// Handles pointer-enter event for the given input mode.
    pub fn on_mouse_enter(&mut self, mode: InputMode) {
        if mode == Seamless {
            self.0 = Capture::Seamless;
        }
    }

    #[inline]
    /// Handles pointer-leave event.
    pub const fn on_mouse_leave(&mut self) { self.reset(); }

    #[inline]
    /// Handles click event for the given input mode.
    pub fn on_click(&mut self, mode: InputMode) {
        if mode == Confined {
            self.0 = Capture::Confined;
        }
    }

    #[inline]
    /// Handles release event and returns the resulting state.
    pub const fn on_release(&mut self) -> Capture {
        self.reset();
        self.current()
    }

    #[inline]
    /// Returns whether input events should be forwarded.
    pub fn should_forward(&self) -> bool {
        match self.0 {
            Capture::Idle => false,
            Capture::Seamless | Capture::Confined => true,
        }
    }

    #[inline]
    /// Returns current capture state.
    pub const fn current(&self) -> Capture { self.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy)]
    enum ModeEvent {
        Enter(InputMode),
        Click(InputMode),
    }

    fn state_in(capture: Capture) -> CaptureState {
        let mut state = CaptureState::new();
        match capture {
            Capture::Idle => {}
            Capture::Seamless => state.on_mouse_enter(InputMode::Seamless),
            Capture::Confined => state.on_click(InputMode::Confined),
        }
        state
    }

    fn assert_state(state: &CaptureState, expected: Capture) {
        assert_eq!(state.current(), expected);
        assert_eq!(state.should_forward(), expected != Capture::Idle);
    }

    #[test]
    fn starts_in_idle() {
        let from_default = CaptureState::default();
        let from_new = CaptureState::new();
        assert_state(&from_default, Capture::Idle);
        assert_state(&from_new, Capture::Idle);
    }

    #[test]
    fn mode_sensitive_events_follow_transition_matrix() {
        let cases = [
            (Capture::Idle, ModeEvent::Enter(InputMode::Seamless), Capture::Seamless),
            (Capture::Idle, ModeEvent::Enter(InputMode::Confined), Capture::Idle),
            (Capture::Idle, ModeEvent::Click(InputMode::Seamless), Capture::Idle),
            (Capture::Idle, ModeEvent::Click(InputMode::Confined), Capture::Confined),
            (Capture::Seamless, ModeEvent::Enter(InputMode::Seamless), Capture::Seamless),
            (Capture::Seamless, ModeEvent::Enter(InputMode::Confined), Capture::Seamless),
            (Capture::Seamless, ModeEvent::Click(InputMode::Seamless), Capture::Seamless),
            (Capture::Seamless, ModeEvent::Click(InputMode::Confined), Capture::Confined),
            (Capture::Confined, ModeEvent::Enter(InputMode::Seamless), Capture::Seamless),
            (Capture::Confined, ModeEvent::Enter(InputMode::Confined), Capture::Confined),
            (Capture::Confined, ModeEvent::Click(InputMode::Seamless), Capture::Confined),
            (Capture::Confined, ModeEvent::Click(InputMode::Confined), Capture::Confined),
        ];

        for (initial, event, expected) in cases {
            let mut state = state_in(initial);
            match event {
                ModeEvent::Enter(mode) => state.on_mouse_enter(mode),
                ModeEvent::Click(mode) => state.on_click(mode),
            }
            assert_state(&state, expected);
        }
    }

    #[test]
    fn mouse_leave_resets_from_any_state() {
        for initial in [Capture::Idle, Capture::Seamless, Capture::Confined] {
            let mut state = state_in(initial);
            state.on_mouse_leave();
            assert_state(&state, Capture::Idle);
        }
    }

    #[test]
    fn release_resets_and_returns_idle_from_any_state() {
        for initial in [Capture::Idle, Capture::Seamless, Capture::Confined] {
            let mut state = state_in(initial);
            let released = state.on_release();
            assert_eq!(released, Capture::Idle);
            assert_state(&state, Capture::Idle);
        }
    }

    #[test]
    fn reset_is_idempotent() {
        let mut state = state_in(Capture::Confined);
        state.reset();
        assert_state(&state, Capture::Idle);
        state.reset();
        assert_state(&state, Capture::Idle);
    }

    #[test]
    fn typical_seamless_flow() {
        let mut state = CaptureState::new();
        assert_state(&state, Capture::Idle);

        state.on_mouse_enter(InputMode::Seamless);
        assert_state(&state, Capture::Seamless);

        state.on_mouse_enter(InputMode::Seamless);
        assert_state(&state, Capture::Seamless);

        state.on_mouse_leave();
        assert_state(&state, Capture::Idle);
    }

    #[test]
    fn typical_confined_flow() {
        let mut state = CaptureState::new();
        assert_state(&state, Capture::Idle);

        state.on_mouse_enter(InputMode::Confined);
        assert_state(&state, Capture::Idle);

        state.on_click(InputMode::Confined);
        assert_state(&state, Capture::Confined);

        let released = state.on_release();
        assert_eq!(released, Capture::Idle);
        assert_state(&state, Capture::Idle);
    }
}
