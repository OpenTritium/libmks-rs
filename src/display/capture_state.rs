use super::vm_display::PointerPolicy::{self, *};

/// Current pointer capture status for display input forwarding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PointerState {
    #[default]
    /// Not in auto-tracking region and not in locked mode.
    Inactive,
    /// Auto mode while cursor is inside the viewport.
    Tracking,
    /// Locked mode after a capture click.
    Captured,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
/// Mutable state machine for pointer capture transitions.
pub struct CaptureState(PointerState);

impl CaptureState {
    /// Creates a new capture state in `PointerState::Inactive`.
    pub fn new() -> Self { Self::default() }

    #[inline]
    /// Resets capture state to `PointerState::Inactive`.
    const fn reset(&mut self) { self.0 = PointerState::Inactive; }

    #[inline]
    /// Enters capture for the given input policy when supported.
    pub fn enter(&mut self, policy: PointerPolicy) {
        if policy == Auto {
            self.0 = PointerState::Tracking;
        }
    }

    #[inline]
    /// Leaves the current capture session.
    pub const fn leave(&mut self) { self.reset(); }

    #[inline]
    /// Captures input for the given input policy when supported.
    pub fn capture(&mut self, policy: PointerPolicy) {
        if policy == Locked {
            self.0 = PointerState::Captured;
        }
    }

    #[inline]
    /// Releases the current capture session.
    pub const fn release(&mut self) { self.reset(); }

    #[inline]
    /// Returns whether input events should be forwarded.
    pub fn should_forward(&self) -> bool {
        match self.0 {
            PointerState::Inactive => false,
            PointerState::Tracking | PointerState::Captured => true,
        }
    }

    #[inline]
    /// Returns current pointer state.
    pub const fn current(&self) -> PointerState { self.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy)]
    enum PolicyEvent {
        Enter(PointerPolicy),
        Capture(PointerPolicy),
    }

    fn state_in(state: PointerState) -> CaptureState {
        let mut capture_state = CaptureState::new();
        match state {
            PointerState::Inactive => {}
            PointerState::Tracking => capture_state.enter(PointerPolicy::Auto),
            PointerState::Captured => capture_state.capture(PointerPolicy::Locked),
        }
        capture_state
    }

    fn assert_state(state: &CaptureState, expected: PointerState) {
        assert_eq!(state.current(), expected);
        assert_eq!(state.should_forward(), expected != PointerState::Inactive);
    }

    #[test]
    fn starts_in_inactive() {
        let from_default = CaptureState::default();
        let from_new = CaptureState::new();
        assert_state(&from_default, PointerState::Inactive);
        assert_state(&from_new, PointerState::Inactive);
    }

    #[test]
    fn mode_sensitive_events_follow_transition_matrix() {
        let cases = [
            (PointerState::Inactive, PolicyEvent::Enter(PointerPolicy::Auto), PointerState::Tracking),
            (PointerState::Inactive, PolicyEvent::Enter(PointerPolicy::Locked), PointerState::Inactive),
            (PointerState::Inactive, PolicyEvent::Capture(PointerPolicy::Auto), PointerState::Inactive),
            (PointerState::Inactive, PolicyEvent::Capture(PointerPolicy::Locked), PointerState::Captured),
            (PointerState::Tracking, PolicyEvent::Enter(PointerPolicy::Auto), PointerState::Tracking),
            (PointerState::Tracking, PolicyEvent::Enter(PointerPolicy::Locked), PointerState::Tracking),
            (PointerState::Tracking, PolicyEvent::Capture(PointerPolicy::Auto), PointerState::Tracking),
            (PointerState::Tracking, PolicyEvent::Capture(PointerPolicy::Locked), PointerState::Captured),
            (PointerState::Captured, PolicyEvent::Enter(PointerPolicy::Auto), PointerState::Tracking),
            (PointerState::Captured, PolicyEvent::Enter(PointerPolicy::Locked), PointerState::Captured),
            (PointerState::Captured, PolicyEvent::Capture(PointerPolicy::Auto), PointerState::Captured),
            (PointerState::Captured, PolicyEvent::Capture(PointerPolicy::Locked), PointerState::Captured),
        ];

        for (initial, event, expected) in cases {
            let mut state = state_in(initial);
            match event {
                PolicyEvent::Enter(policy) => state.enter(policy),
                PolicyEvent::Capture(policy) => state.capture(policy),
            }
            assert_state(&state, expected);
        }
    }

    #[test]
    fn mouse_leave_resets_from_any_state() {
        for initial in [PointerState::Inactive, PointerState::Tracking, PointerState::Captured] {
            let mut state = state_in(initial);
            state.leave();
            assert_state(&state, PointerState::Inactive);
        }
    }

    #[test]
    fn release_resets_from_any_state() {
        for initial in [PointerState::Inactive, PointerState::Tracking, PointerState::Captured] {
            let mut state = state_in(initial);
            state.release();
            assert_state(&state, PointerState::Inactive);
        }
    }

    #[test]
    fn reset_is_idempotent() {
        let mut state = state_in(PointerState::Captured);
        state.reset();
        assert_state(&state, PointerState::Inactive);
        state.reset();
        assert_state(&state, PointerState::Inactive);
    }

    #[test]
    fn typical_auto_flow() {
        let mut state = CaptureState::new();
        assert_state(&state, PointerState::Inactive);

        state.enter(PointerPolicy::Auto);
        assert_state(&state, PointerState::Tracking);

        state.enter(PointerPolicy::Auto);
        assert_state(&state, PointerState::Tracking);

        state.leave();
        assert_state(&state, PointerState::Inactive);
    }

    #[test]
    fn typical_locked_flow() {
        let mut state = CaptureState::new();
        assert_state(&state, PointerState::Inactive);

        state.enter(PointerPolicy::Locked);
        assert_state(&state, PointerState::Inactive);

        state.capture(PointerPolicy::Locked);
        assert_state(&state, PointerState::Captured);

        state.release();
        assert_state(&state, PointerState::Inactive);
    }
}
