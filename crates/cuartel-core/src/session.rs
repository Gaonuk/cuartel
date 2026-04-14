use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "data")]
pub enum SessionState {
    Created,
    Booting,
    Ready,
    Running,
    Paused,
    Checkpointed,
    Forked,
    Reviewing,
    Error(String),
    Destroyed,
}

impl SessionState {
    pub fn is_terminal(&self) -> bool {
        matches!(self, SessionState::Destroyed)
    }

    pub fn is_error(&self) -> bool {
        matches!(self, SessionState::Error(_))
    }

    fn discriminant(&self) -> &'static str {
        match self {
            SessionState::Created => "created",
            SessionState::Booting => "booting",
            SessionState::Ready => "ready",
            SessionState::Running => "running",
            SessionState::Paused => "paused",
            SessionState::Checkpointed => "checkpointed",
            SessionState::Forked => "forked",
            SessionState::Reviewing => "reviewing",
            SessionState::Error(_) => "error",
            SessionState::Destroyed => "destroyed",
        }
    }
}

impl fmt::Display for SessionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionState::Error(msg) => write!(f, "error({msg})"),
            other => f.write_str(other.discriminant()),
        }
    }
}

/// Events that drive the session state machine.
///
/// Transitions are only expressed via events — callers never set a state
/// directly. This keeps the machine auditable and makes every legal
/// transition a named, testable operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "event", content = "data")]
pub enum SessionEvent {
    /// User asked to boot the VM.
    Boot,
    /// VM finished booting and agent harness is installed.
    BootCompleted,
    /// User sent a prompt; agent is now executing.
    PromptSent,
    /// Agent finished processing the current prompt.
    PromptCompleted,
    /// User paused a running agent.
    Pause,
    /// User resumed a paused agent.
    Resume,
    /// Snapshot VM state.
    Checkpoint,
    /// Restore VM from a previously saved checkpoint.
    RestoreCheckpoint,
    /// Fork a checkpoint into a new branch.
    Fork,
    /// Forked branch is ready to run.
    ForkReady,
    /// Overlay FS detected pending changes — enter review mode.
    ChangesDetected,
    /// User accepted or rejected the review; return to ready.
    ReviewResolved,
    /// Something blew up. Payload is a human readable reason.
    Failed(String),
    /// Transient error cleared; return to a usable state.
    Recover,
    /// Final teardown.
    Destroy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransitionError {
    Invalid {
        from: SessionState,
        event: SessionEvent,
    },
    AlreadyTerminal,
}

impl fmt::Display for TransitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransitionError::Invalid { from, event } => {
                write!(f, "invalid event {event:?} from state {from}")
            }
            TransitionError::AlreadyTerminal => {
                write!(f, "session is destroyed and cannot accept further events")
            }
        }
    }
}

impl std::error::Error for TransitionError {}

/// One recorded step in a session's lifecycle. Used for audit UI and for
/// reconstructing state from persistence without replaying every event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransitionRecord {
    pub from: SessionState,
    pub to: SessionState,
    pub event: SessionEvent,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub workspace_id: String,
    pub server_id: String,
    pub agent_type: String,
    pub rivet_vm_id: Option<String>,
    pub rivet_session_id: Option<String>,
    pub state: SessionState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub history: Vec<TransitionRecord>,
}

impl Session {
    pub fn new(
        id: String,
        workspace_id: String,
        server_id: String,
        agent_type: String,
    ) -> Self {
        let now = Utc::now();
        Self {
            id,
            workspace_id,
            server_id,
            agent_type,
            rivet_vm_id: None,
            rivet_session_id: None,
            state: SessionState::Created,
            created_at: now,
            updated_at: now,
            history: Vec::new(),
        }
    }

    pub fn apply(&mut self, event: SessionEvent) -> Result<&SessionState, TransitionError> {
        self.apply_at(event, Utc::now())
    }

    /// Deterministic variant for tests and replay.
    pub fn apply_at(
        &mut self,
        event: SessionEvent,
        at: DateTime<Utc>,
    ) -> Result<&SessionState, TransitionError> {
        if self.state.is_terminal() {
            return Err(TransitionError::AlreadyTerminal);
        }
        let next = next_state(&self.state, &event).ok_or_else(|| TransitionError::Invalid {
            from: self.state.clone(),
            event: event.clone(),
        })?;
        let from = std::mem::replace(&mut self.state, next.clone());
        self.updated_at = at;
        self.history.push(TransitionRecord {
            from,
            to: next,
            event,
            at,
        });
        Ok(&self.state)
    }
}

/// Pure transition function. Returns `Some(next)` if the event is legal from
/// `state`, `None` otherwise. Kept separate from `Session::apply` so it can be
/// reused for dry-run validation and fuzz testing.
pub fn next_state(state: &SessionState, event: &SessionEvent) -> Option<SessionState> {
    use SessionEvent as E;
    use SessionState as S;

    // Failure and destruction are legal from any non-terminal state.
    match event {
        E::Failed(msg) if !matches!(state, S::Destroyed) => {
            return Some(S::Error(msg.clone()));
        }
        E::Destroy if !matches!(state, S::Destroyed) => {
            return Some(S::Destroyed);
        }
        _ => {}
    }

    let next = match (state, event) {
        (S::Created, E::Boot) => S::Booting,
        (S::Booting, E::BootCompleted) => S::Ready,
        (S::Ready, E::PromptSent) => S::Running,
        (S::Running, E::PromptCompleted) => S::Ready,
        (S::Running, E::Pause) => S::Paused,
        (S::Paused, E::Resume) => S::Running,
        (S::Ready, E::Checkpoint) => S::Checkpointed,
        (S::Checkpointed, E::RestoreCheckpoint) => S::Ready,
        (S::Checkpointed, E::Fork) => S::Forked,
        (S::Forked, E::ForkReady) => S::Ready,
        (S::Ready, E::ChangesDetected) => S::Reviewing,
        (S::Reviewing, E::ReviewResolved) => S::Ready,
        (S::Error(_), E::Recover) => S::Ready,
        _ => return None,
    };
    Some(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sess() -> Session {
        Session::new(
            "sess-1".into(),
            "ws-1".into(),
            "local".into(),
            "pi".into(),
        )
    }

    #[test]
    fn happy_path_create_boot_prompt_complete() {
        let mut s = sess();
        assert_eq!(s.state, SessionState::Created);
        s.apply(SessionEvent::Boot).unwrap();
        assert_eq!(s.state, SessionState::Booting);
        s.apply(SessionEvent::BootCompleted).unwrap();
        assert_eq!(s.state, SessionState::Ready);
        s.apply(SessionEvent::PromptSent).unwrap();
        assert_eq!(s.state, SessionState::Running);
        s.apply(SessionEvent::PromptCompleted).unwrap();
        assert_eq!(s.state, SessionState::Ready);
        assert_eq!(s.history.len(), 4);
    }

    #[test]
    fn pause_resume_cycle() {
        let mut s = sess();
        s.apply(SessionEvent::Boot).unwrap();
        s.apply(SessionEvent::BootCompleted).unwrap();
        s.apply(SessionEvent::PromptSent).unwrap();
        s.apply(SessionEvent::Pause).unwrap();
        assert_eq!(s.state, SessionState::Paused);
        s.apply(SessionEvent::Resume).unwrap();
        assert_eq!(s.state, SessionState::Running);
    }

    #[test]
    fn checkpoint_restore_and_fork() {
        let mut s = sess();
        s.apply(SessionEvent::Boot).unwrap();
        s.apply(SessionEvent::BootCompleted).unwrap();
        s.apply(SessionEvent::Checkpoint).unwrap();
        assert_eq!(s.state, SessionState::Checkpointed);
        s.apply(SessionEvent::RestoreCheckpoint).unwrap();
        assert_eq!(s.state, SessionState::Ready);
        s.apply(SessionEvent::Checkpoint).unwrap();
        s.apply(SessionEvent::Fork).unwrap();
        assert_eq!(s.state, SessionState::Forked);
        s.apply(SessionEvent::ForkReady).unwrap();
        assert_eq!(s.state, SessionState::Ready);
    }

    #[test]
    fn review_cycle() {
        let mut s = sess();
        s.apply(SessionEvent::Boot).unwrap();
        s.apply(SessionEvent::BootCompleted).unwrap();
        s.apply(SessionEvent::ChangesDetected).unwrap();
        assert_eq!(s.state, SessionState::Reviewing);
        s.apply(SessionEvent::ReviewResolved).unwrap();
        assert_eq!(s.state, SessionState::Ready);
    }

    #[test]
    fn failure_from_any_non_terminal_state_and_recover() {
        for mid in [
            SessionEvent::Boot,
            SessionEvent::BootCompleted,
            SessionEvent::PromptSent,
        ] {
            let mut s = sess();
            // advance to the state under test
            match mid {
                SessionEvent::Boot => {
                    s.apply(SessionEvent::Boot).unwrap();
                }
                SessionEvent::BootCompleted => {
                    s.apply(SessionEvent::Boot).unwrap();
                    s.apply(SessionEvent::BootCompleted).unwrap();
                }
                SessionEvent::PromptSent => {
                    s.apply(SessionEvent::Boot).unwrap();
                    s.apply(SessionEvent::BootCompleted).unwrap();
                    s.apply(SessionEvent::PromptSent).unwrap();
                }
                _ => unreachable!(),
            }
            s.apply(SessionEvent::Failed("boom".into())).unwrap();
            assert_eq!(s.state, SessionState::Error("boom".into()));
            s.apply(SessionEvent::Recover).unwrap();
            assert_eq!(s.state, SessionState::Ready);
        }
    }

    #[test]
    fn destroy_is_legal_from_any_non_terminal_state() {
        let mut s = sess();
        s.apply(SessionEvent::Destroy).unwrap();
        assert_eq!(s.state, SessionState::Destroyed);
        assert!(s.state.is_terminal());
    }

    #[test]
    fn cannot_apply_events_after_destroy() {
        let mut s = sess();
        s.apply(SessionEvent::Destroy).unwrap();
        let err = s.apply(SessionEvent::Boot).unwrap_err();
        assert_eq!(err, TransitionError::AlreadyTerminal);
    }

    #[test]
    fn illegal_transitions_are_rejected() {
        let mut s = sess();
        // Cannot send a prompt before booting.
        let err = s.apply(SessionEvent::PromptSent).unwrap_err();
        assert!(matches!(err, TransitionError::Invalid { .. }));
        // State unchanged after rejection.
        assert_eq!(s.state, SessionState::Created);
        assert!(s.history.is_empty());
    }

    #[test]
    fn cannot_resume_when_not_paused() {
        let mut s = sess();
        s.apply(SessionEvent::Boot).unwrap();
        s.apply(SessionEvent::BootCompleted).unwrap();
        assert!(s.apply(SessionEvent::Resume).is_err());
    }

    #[test]
    fn history_preserves_from_to_and_event() {
        let mut s = sess();
        let t = Utc::now();
        s.apply_at(SessionEvent::Boot, t).unwrap();
        let rec = &s.history[0];
        assert_eq!(rec.from, SessionState::Created);
        assert_eq!(rec.to, SessionState::Booting);
        assert_eq!(rec.event, SessionEvent::Boot);
        assert_eq!(rec.at, t);
        assert_eq!(s.updated_at, t);
    }

    #[test]
    fn next_state_is_pure_and_matches_apply() {
        let mut s = sess();
        let probe = next_state(&s.state, &SessionEvent::Boot).unwrap();
        s.apply(SessionEvent::Boot).unwrap();
        assert_eq!(probe, s.state);
    }

    #[test]
    fn state_serde_roundtrip() {
        let s = SessionState::Error("x".into());
        let j = serde_json::to_string(&s).unwrap();
        let back: SessionState = serde_json::from_str(&j).unwrap();
        assert_eq!(s, back);
    }
}
