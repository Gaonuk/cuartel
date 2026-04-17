use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent::AgentType;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StepId(pub String);

impl StepId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl fmt::Display for StepId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for StepId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum StepAction {
    RunAgent {
        agent_type: AgentType,
        prompt: String,
    },
    Checkpoint {
        label: String,
    },
    WaitForSignal {
        signal_name: String,
    },
    Delay {
        seconds: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowStep {
    pub id: StepId,
    pub action: StepAction,
    pub timeout_seconds: Option<u64>,
    pub retry_count: u32,
}

impl WorkflowStep {
    pub fn agent(id: impl Into<StepId>, agent_type: AgentType, prompt: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            action: StepAction::RunAgent {
                agent_type,
                prompt: prompt.into(),
            },
            timeout_seconds: None,
            retry_count: 0,
        }
    }

    pub fn checkpoint(id: impl Into<StepId>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            action: StepAction::Checkpoint {
                label: label.into(),
            },
            timeout_seconds: None,
            retry_count: 0,
        }
    }

    pub fn wait_signal(id: impl Into<StepId>, signal_name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            action: StepAction::WaitForSignal {
                signal_name: signal_name.into(),
            },
            timeout_seconds: None,
            retry_count: 0,
        }
    }

    pub fn delay(id: impl Into<StepId>, seconds: u64) -> Self {
        Self {
            id: id.into(),
            action: StepAction::Delay { seconds },
            timeout_seconds: None,
            retry_count: 0,
        }
    }

    pub fn with_timeout(mut self, seconds: u64) -> Self {
        self.timeout_seconds = Some(seconds);
        self
    }

    pub fn with_retries(mut self, count: u32) -> Self {
        self.retry_count = count;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowDefinition {
    pub id: String,
    pub name: String,
    pub steps: Vec<WorkflowStep>,
    pub workspace_id: String,
}

impl WorkflowDefinition {
    pub fn new(id: impl Into<String>, name: impl Into<String>, workspace_id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            steps: Vec::new(),
            workspace_id: workspace_id.into(),
        }
    }

    pub fn add_step(&mut self, step: WorkflowStep) {
        self.steps.push(step);
    }

    pub fn step(&self, id: &StepId) -> Option<&WorkflowStep> {
        self.steps.iter().find(|s| &s.id == id)
    }

    pub fn step_index(&self, id: &StepId) -> Option<usize> {
        self.steps.iter().position(|s| &s.id == id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    Pending,
    Running,
    Completed,
    Failed,
    Skipped,
    WaitingForSignal,
    WaitingForDelay,
}

impl fmt::Display for StepState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StepState::Pending => write!(f, "pending"),
            StepState::Running => write!(f, "running"),
            StepState::Completed => write!(f, "completed"),
            StepState::Failed => write!(f, "failed"),
            StepState::Skipped => write!(f, "skipped"),
            StepState::WaitingForSignal => write!(f, "waiting_for_signal"),
            StepState::WaitingForDelay => write!(f, "waiting_for_delay"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowState {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
    Paused,
}

impl fmt::Display for WorkflowState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WorkflowState::Pending => write!(f, "pending"),
            WorkflowState::Running => write!(f, "running"),
            WorkflowState::Completed => write!(f, "completed"),
            WorkflowState::Failed => write!(f, "failed"),
            WorkflowState::Cancelled => write!(f, "cancelled"),
            WorkflowState::Paused => write!(f, "paused"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowError {
    StepNotFound(StepId),
    InvalidTransition { step: StepId, from: StepState, event: &'static str },
    AlreadyTerminal,
    NotRunning,
}

impl fmt::Display for WorkflowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WorkflowError::StepNotFound(id) => write!(f, "step not found: {id}"),
            WorkflowError::InvalidTransition { step, from, event } => {
                write!(f, "invalid {event} for step {step} in state {from}")
            }
            WorkflowError::AlreadyTerminal => write!(f, "workflow already in terminal state"),
            WorkflowError::NotRunning => write!(f, "workflow is not running"),
        }
    }
}

impl std::error::Error for WorkflowError {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepExecution {
    pub step_id: StepId,
    pub state: StepState,
    pub output: Option<Value>,
    pub error: Option<String>,
    pub attempts: u32,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowExecution {
    pub id: String,
    pub definition_id: String,
    pub rivet_workflow_id: Option<String>,
    pub state: WorkflowState,
    pub current_step_index: usize,
    pub steps: Vec<StepExecution>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl WorkflowExecution {
    pub fn new(id: String, definition: &WorkflowDefinition) -> Self {
        let now = Utc::now();
        let steps = definition
            .steps
            .iter()
            .map(|s| StepExecution {
                step_id: s.id.clone(),
                state: StepState::Pending,
                output: None,
                error: None,
                attempts: 0,
                started_at: None,
                completed_at: None,
            })
            .collect();
        Self {
            id,
            definition_id: definition.id.clone(),
            rivet_workflow_id: None,
            state: WorkflowState::Pending,
            current_step_index: 0,
            steps,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn start(&mut self) -> Result<(), WorkflowError> {
        if self.state != WorkflowState::Pending {
            return Err(WorkflowError::AlreadyTerminal);
        }
        self.state = WorkflowState::Running;
        self.updated_at = Utc::now();
        Ok(())
    }

    pub fn current_step(&self) -> Option<&StepExecution> {
        self.steps.get(self.current_step_index)
    }

    pub fn current_step_mut(&mut self) -> Option<&mut StepExecution> {
        self.steps.get_mut(self.current_step_index)
    }

    pub fn begin_step(&mut self) -> Result<&StepId, WorkflowError> {
        if self.state != WorkflowState::Running {
            return Err(WorkflowError::NotRunning);
        }
        let step = self
            .steps
            .get_mut(self.current_step_index)
            .ok_or_else(|| WorkflowError::StepNotFound(StepId::new("__out_of_bounds__")))?;
        if step.state != StepState::Pending {
            return Err(WorkflowError::InvalidTransition {
                step: step.step_id.clone(),
                from: step.state,
                event: "begin_step",
            });
        }
        step.state = StepState::Running;
        step.attempts += 1;
        step.started_at = Some(Utc::now());
        self.updated_at = Utc::now();
        Ok(&self.steps[self.current_step_index].step_id)
    }

    pub fn complete_step(&mut self, output: Value) -> Result<(), WorkflowError> {
        if self.state != WorkflowState::Running {
            return Err(WorkflowError::NotRunning);
        }
        let step = self
            .steps
            .get_mut(self.current_step_index)
            .ok_or_else(|| WorkflowError::StepNotFound(StepId::new("__out_of_bounds__")))?;
        if step.state != StepState::Running
            && step.state != StepState::WaitingForSignal
            && step.state != StepState::WaitingForDelay
        {
            return Err(WorkflowError::InvalidTransition {
                step: step.step_id.clone(),
                from: step.state,
                event: "complete_step",
            });
        }
        step.state = StepState::Completed;
        step.output = Some(output);
        step.completed_at = Some(Utc::now());
        self.current_step_index += 1;
        self.updated_at = Utc::now();

        if self.current_step_index >= self.steps.len() {
            self.state = WorkflowState::Completed;
        }
        Ok(())
    }

    pub fn fail_step(&mut self, error: String, definition: &WorkflowDefinition) -> Result<(), WorkflowError> {
        if self.state != WorkflowState::Running {
            return Err(WorkflowError::NotRunning);
        }
        let step_def = definition
            .steps
            .get(self.current_step_index);
        let max_retries = step_def.map(|s| s.retry_count).unwrap_or(0);

        let step = self
            .steps
            .get_mut(self.current_step_index)
            .ok_or_else(|| WorkflowError::StepNotFound(StepId::new("__out_of_bounds__")))?;

        if step.state != StepState::Running {
            return Err(WorkflowError::InvalidTransition {
                step: step.step_id.clone(),
                from: step.state,
                event: "fail_step",
            });
        }

        if step.attempts <= max_retries {
            step.state = StepState::Pending;
            step.error = Some(error);
            self.updated_at = Utc::now();
        } else {
            step.state = StepState::Failed;
            step.error = Some(error);
            step.completed_at = Some(Utc::now());
            self.state = WorkflowState::Failed;
            self.updated_at = Utc::now();

            for remaining in self.steps.iter_mut().skip(self.current_step_index + 1) {
                remaining.state = StepState::Skipped;
            }
        }
        Ok(())
    }

    pub fn pause_at_signal(&mut self) -> Result<(), WorkflowError> {
        if self.state != WorkflowState::Running {
            return Err(WorkflowError::NotRunning);
        }
        let step = self
            .steps
            .get_mut(self.current_step_index)
            .ok_or_else(|| WorkflowError::StepNotFound(StepId::new("__out_of_bounds__")))?;
        if step.state != StepState::Running {
            return Err(WorkflowError::InvalidTransition {
                step: step.step_id.clone(),
                from: step.state,
                event: "pause_at_signal",
            });
        }
        step.state = StepState::WaitingForSignal;
        self.state = WorkflowState::Paused;
        self.updated_at = Utc::now();
        Ok(())
    }

    pub fn resume_from_signal(&mut self, signal_output: Value) -> Result<(), WorkflowError> {
        if self.state != WorkflowState::Paused {
            return Err(WorkflowError::NotRunning);
        }
        self.state = WorkflowState::Running;
        self.complete_step(signal_output)
    }

    pub fn cancel(&mut self) {
        for step in &mut self.steps {
            if step.state == StepState::Pending || step.state == StepState::WaitingForSignal {
                step.state = StepState::Skipped;
            }
        }
        self.state = WorkflowState::Cancelled;
        self.updated_at = Utc::now();
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self.state,
            WorkflowState::Completed | WorkflowState::Failed | WorkflowState::Cancelled
        )
    }

    pub fn progress(&self) -> (usize, usize) {
        let completed = self
            .steps
            .iter()
            .filter(|s| s.state == StepState::Completed)
            .count();
        (completed, self.steps.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_definition() -> WorkflowDefinition {
        let mut def = WorkflowDefinition::new("wf-def-1", "Code Review Workflow", "ws-1");
        def.add_step(WorkflowStep::agent("code", AgentType::ClaudeCode, "Implement feature"));
        def.add_step(WorkflowStep::checkpoint("snap", "post-code"));
        def.add_step(WorkflowStep::wait_signal("approval", "human-review"));
        def.add_step(WorkflowStep::agent("test", AgentType::Pi, "Run test suite"));
        def
    }

    fn with_retries_definition() -> WorkflowDefinition {
        let mut def = WorkflowDefinition::new("wf-retry", "Retry test", "ws-1");
        def.add_step(
            WorkflowStep::agent("flaky", AgentType::Pi, "flaky task")
                .with_retries(2)
                .with_timeout(60),
        );
        def.add_step(WorkflowStep::agent("next", AgentType::Pi, "next"));
        def
    }

    #[test]
    fn workflow_definition_finds_steps() {
        let def = sample_definition();
        assert!(def.step(&"code".into()).is_some());
        assert!(def.step(&"missing".into()).is_none());
        assert_eq!(def.step_index(&"snap".into()), Some(1));
    }

    #[test]
    fn workflow_execution_happy_path() {
        let def = sample_definition();
        let mut exec = WorkflowExecution::new("exec-1".into(), &def);
        assert_eq!(exec.state, WorkflowState::Pending);
        assert_eq!(exec.steps.len(), 4);

        exec.start().unwrap();
        assert_eq!(exec.state, WorkflowState::Running);

        let step_id = exec.begin_step().unwrap().clone();
        assert_eq!(step_id, StepId::new("code"));
        exec.complete_step(json!("diff output")).unwrap();
        assert_eq!(exec.current_step_index, 1);

        exec.begin_step().unwrap();
        exec.complete_step(json!("checkpoint-id-123")).unwrap();

        exec.begin_step().unwrap();
        exec.pause_at_signal().unwrap();
        assert_eq!(exec.state, WorkflowState::Paused);

        exec.resume_from_signal(json!({"approved": true})).unwrap();
        assert_eq!(exec.state, WorkflowState::Running);

        exec.begin_step().unwrap();
        exec.complete_step(json!("all tests pass")).unwrap();

        assert_eq!(exec.state, WorkflowState::Completed);
        assert_eq!(exec.progress(), (4, 4));
    }

    #[test]
    fn workflow_execution_failure_skips_remaining() {
        let def = sample_definition();
        let mut exec = WorkflowExecution::new("exec-2".into(), &def);
        exec.start().unwrap();
        exec.begin_step().unwrap();
        exec.fail_step("compile error".into(), &def).unwrap();

        assert_eq!(exec.state, WorkflowState::Failed);
        assert_eq!(exec.steps[0].state, StepState::Failed);
        assert_eq!(exec.steps[1].state, StepState::Skipped);
        assert_eq!(exec.steps[2].state, StepState::Skipped);
        assert_eq!(exec.steps[3].state, StepState::Skipped);
    }

    #[test]
    fn workflow_execution_cancel() {
        let def = sample_definition();
        let mut exec = WorkflowExecution::new("exec-3".into(), &def);
        exec.start().unwrap();
        exec.begin_step().unwrap();
        exec.cancel();

        assert_eq!(exec.state, WorkflowState::Cancelled);
        assert_eq!(exec.steps[0].state, StepState::Running);
        assert_eq!(exec.steps[1].state, StepState::Skipped);
    }

    #[test]
    fn workflow_retry_resets_to_pending() {
        let def = with_retries_definition();
        let mut exec = WorkflowExecution::new("exec-4".into(), &def);
        exec.start().unwrap();

        exec.begin_step().unwrap();
        assert_eq!(exec.steps[0].attempts, 1);
        exec.fail_step("transient error".into(), &def).unwrap();
        assert_eq!(exec.steps[0].state, StepState::Pending);
        assert_eq!(exec.state, WorkflowState::Running);

        exec.begin_step().unwrap();
        assert_eq!(exec.steps[0].attempts, 2);
        exec.fail_step("transient again".into(), &def).unwrap();
        assert_eq!(exec.steps[0].state, StepState::Pending);

        exec.begin_step().unwrap();
        assert_eq!(exec.steps[0].attempts, 3);
        exec.fail_step("final failure".into(), &def).unwrap();
        assert_eq!(exec.steps[0].state, StepState::Failed);
        assert_eq!(exec.state, WorkflowState::Failed);
    }

    #[test]
    fn workflow_progress_tracks_completion() {
        let def = sample_definition();
        let mut exec = WorkflowExecution::new("exec-5".into(), &def);
        exec.start().unwrap();
        assert_eq!(exec.progress(), (0, 4));

        exec.begin_step().unwrap();
        exec.complete_step(json!(null)).unwrap();
        assert_eq!(exec.progress(), (1, 4));
    }

    #[test]
    fn workflow_rejects_double_start() {
        let def = sample_definition();
        let mut exec = WorkflowExecution::new("exec-6".into(), &def);
        exec.start().unwrap();
        assert!(matches!(exec.start(), Err(WorkflowError::AlreadyTerminal)));
    }

    #[test]
    fn workflow_is_terminal_for_completed_failed_cancelled() {
        let def = sample_definition();

        let mut exec = WorkflowExecution::new("t1".into(), &def);
        exec.state = WorkflowState::Completed;
        assert!(exec.is_terminal());

        exec.state = WorkflowState::Failed;
        assert!(exec.is_terminal());

        exec.state = WorkflowState::Cancelled;
        assert!(exec.is_terminal());

        exec.state = WorkflowState::Running;
        assert!(!exec.is_terminal());
    }

    #[test]
    fn step_constructors_produce_correct_actions() {
        let agent = WorkflowStep::agent("a", AgentType::Pi, "do it");
        assert!(matches!(agent.action, StepAction::RunAgent { .. }));

        let cp = WorkflowStep::checkpoint("c", "snap");
        assert!(matches!(cp.action, StepAction::Checkpoint { .. }));

        let sig = WorkflowStep::wait_signal("s", "approval");
        assert!(matches!(sig.action, StepAction::WaitForSignal { .. }));

        let delay = WorkflowStep::delay("d", 30);
        assert!(matches!(delay.action, StepAction::Delay { seconds: 30 }));
    }

    #[test]
    fn step_builder_methods() {
        let step = WorkflowStep::agent("a", AgentType::Pi, "x")
            .with_timeout(120)
            .with_retries(3);
        assert_eq!(step.timeout_seconds, Some(120));
        assert_eq!(step.retry_count, 3);
    }

    #[test]
    fn workflow_serde_roundtrip() {
        let def = sample_definition();
        let json = serde_json::to_string(&def).unwrap();
        let back: WorkflowDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(back.steps.len(), 4);
        assert_eq!(back.steps[0].id, StepId::new("code"));
    }
}
