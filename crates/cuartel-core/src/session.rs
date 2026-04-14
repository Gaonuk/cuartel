use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SessionState {
    Created,
    Booting,
    Ready,
    Running,
    Paused,
    Checkpointed,
    Reviewing,
    Error(String),
    Destroyed,
}

impl SessionState {
    pub fn can_transition_to(&self, next: &SessionState) -> bool {
        use SessionState::*;
        matches!(
            (self, next),
            (Created, Booting)
                | (Booting, Ready)
                | (Booting, Error(_))
                | (Ready, Running)
                | (Ready, Checkpointed)
                | (Ready, Reviewing)
                | (Ready, Destroyed)
                | (Running, Ready)
                | (Running, Paused)
                | (Running, Error(_))
                | (Paused, Running)
                | (Checkpointed, Ready)
                | (Reviewing, Ready)
                | (Error(_), Ready)
                | (Error(_), Destroyed)
        )
    }
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
}

impl Session {
    pub fn new(
        id: String,
        workspace_id: String,
        server_id: String,
        agent_type: String,
    ) -> Self {
        Self {
            id,
            workspace_id,
            server_id,
            agent_type,
            rivet_vm_id: None,
            rivet_session_id: None,
            state: SessionState::Created,
        }
    }

    pub fn transition(&mut self, next: SessionState) -> Result<(), String> {
        if self.state.can_transition_to(&next) {
            self.state = next;
            Ok(())
        } else {
            Err(format!(
                "invalid transition from {:?} to {:?}",
                self.state, next
            ))
        }
    }
}
