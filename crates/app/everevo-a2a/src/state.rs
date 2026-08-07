//! Task state machine — validates every state transition against the
//! A2A protocol specification. Terminal states are immutable; interrupted
//! states (input-required, auth-required) can resume.

use crate::types::TaskState;

impl TaskState {
    /// Check whether a transition from `self` to `next` is valid per the
    /// A2A state machine specification.
    ///
    /// ```text
    /// submitted   → working | rejected
    /// working     → completed | failed | canceled | input-required | auth-required
    /// input-required → working | failed
    /// auth-required  → working | failed
    /// terminal states → never (completed/failed/canceled/rejected are immutable)
    /// ```
    pub fn can_transition_to(&self, next: &TaskState) -> bool {
        use TaskState::*;
        matches!(
            (self, next),
            // submitted can start processing or be rejected
            (Submitted, Working | Rejected)
                // working can reach any terminal or pause
                | (Working, Completed | Failed | Canceled | InputRequired | AuthRequired)
                // interrupted states can resume or fail
                | (InputRequired, Working | Failed)
                | (AuthRequired, Working | Failed)
        )
        // Unknown allows no transitions (not matched above → returns false)
    }

    /// Returns `true` if this is a terminal (immutable) state.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            TaskState::Completed | TaskState::Failed | TaskState::Canceled | TaskState::Rejected | TaskState::Unknown
        )
    }

    /// Returns `true` if the task can be canceled from this state.
    pub fn is_cancelable(&self) -> bool {
        matches!(
            self,
            TaskState::Submitted | TaskState::Working | TaskState::InputRequired
        )
        // Unknown is NOT cancelable (treated as terminal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_transitions() {
        // submitted
        assert!(TaskState::Submitted.can_transition_to(&TaskState::Working));
        assert!(TaskState::Submitted.can_transition_to(&TaskState::Rejected));
        assert!(!TaskState::Submitted.can_transition_to(&TaskState::Completed));

        // working
        assert!(TaskState::Working.can_transition_to(&TaskState::Completed));
        assert!(TaskState::Working.can_transition_to(&TaskState::Failed));
        assert!(TaskState::Working.can_transition_to(&TaskState::Canceled));
        assert!(TaskState::Working.can_transition_to(&TaskState::InputRequired));
        assert!(!TaskState::Working.can_transition_to(&TaskState::Submitted));

        // terminal — all transitions blocked
        for terminal in &[
            TaskState::Completed,
            TaskState::Failed,
            TaskState::Canceled,
            TaskState::Rejected,
        ] {
            assert!(!terminal.can_transition_to(&TaskState::Working));
            assert!(!terminal.can_transition_to(&TaskState::Submitted));
        }

        // input-required can resume
        assert!(TaskState::InputRequired.can_transition_to(&TaskState::Working));
        assert!(TaskState::InputRequired.can_transition_to(&TaskState::Failed));
    }

    #[test]
    fn test_terminal_detection() {
        assert!(TaskState::Completed.is_terminal());
        assert!(TaskState::Failed.is_terminal());
        assert!(TaskState::Canceled.is_terminal());
        assert!(TaskState::Rejected.is_terminal());
        assert!(!TaskState::Working.is_terminal());
    }

    #[test]
    fn test_cancelable() {
        assert!(TaskState::Submitted.is_cancelable());
        assert!(TaskState::Working.is_cancelable());
        assert!(!TaskState::Completed.is_cancelable());
        assert!(!TaskState::Failed.is_cancelable());
    }
}
