use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::agent::AgentType;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct StageId(pub String);

impl StageId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl fmt::Display for StageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for StageId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipelineStage {
    pub id: StageId,
    pub agent_type: AgentType,
    pub prompt_template: String,
    pub depends_on: Vec<StageId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipelineError {
    DuplicateStage(StageId),
    UnknownDependency { stage: StageId, missing: StageId },
    CycleDetected(Vec<StageId>),
    EmptyPipeline,
    StageNotFound(StageId),
    InvalidTransition { stage: StageId, from: StageState, event: &'static str },
    DependenciesNotMet(StageId),
}

impl fmt::Display for PipelineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PipelineError::DuplicateStage(id) => write!(f, "duplicate stage id: {id}"),
            PipelineError::UnknownDependency { stage, missing } => {
                write!(f, "stage {stage} depends on unknown stage {missing}")
            }
            PipelineError::CycleDetected(path) => {
                let ids: Vec<&str> = path.iter().map(|s| s.0.as_str()).collect();
                write!(f, "cycle detected: {}", ids.join(" -> "))
            }
            PipelineError::EmptyPipeline => write!(f, "pipeline has no stages"),
            PipelineError::StageNotFound(id) => write!(f, "stage not found: {id}"),
            PipelineError::InvalidTransition { stage, from, event } => {
                write!(f, "invalid transition for stage {stage}: {event} from {from}")
            }
            PipelineError::DependenciesNotMet(id) => {
                write!(f, "dependencies not met for stage {id}")
            }
        }
    }
}

impl std::error::Error for PipelineError {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pipeline {
    pub id: String,
    pub name: String,
    pub stages: Vec<PipelineStage>,
}

impl Pipeline {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            stages: Vec::new(),
        }
    }

    pub fn add_stage(&mut self, stage: PipelineStage) {
        self.stages.push(stage);
    }

    pub fn validate(&self) -> Result<(), PipelineError> {
        if self.stages.is_empty() {
            return Err(PipelineError::EmptyPipeline);
        }

        let mut seen: BTreeSet<&StageId> = BTreeSet::new();
        for stage in &self.stages {
            if !seen.insert(&stage.id) {
                return Err(PipelineError::DuplicateStage(stage.id.clone()));
            }
        }

        for stage in &self.stages {
            for dep in &stage.depends_on {
                if !seen.contains(dep) {
                    return Err(PipelineError::UnknownDependency {
                        stage: stage.id.clone(),
                        missing: dep.clone(),
                    });
                }
            }
        }

        detect_cycle(&self.stages)?;
        Ok(())
    }

    pub fn topological_order(&self) -> Result<Vec<StageId>, PipelineError> {
        self.validate()?;
        topological_sort(&self.stages)
    }

    pub fn ready_stages(&self, completed: &BTreeSet<StageId>) -> Vec<StageId> {
        self.stages
            .iter()
            .filter(|s| {
                !completed.contains(&s.id)
                    && s.depends_on.iter().all(|d| completed.contains(d))
            })
            .map(|s| s.id.clone())
            .collect()
    }

    pub fn stage(&self, id: &StageId) -> Option<&PipelineStage> {
        self.stages.iter().find(|s| &s.id == id)
    }
}

fn topological_sort(stages: &[PipelineStage]) -> Result<Vec<StageId>, PipelineError> {
    let mut in_degree: BTreeMap<&StageId, usize> = BTreeMap::new();
    let mut dependents: BTreeMap<&StageId, Vec<&StageId>> = BTreeMap::new();

    for stage in stages {
        in_degree.entry(&stage.id).or_insert(0);
        for dep in &stage.depends_on {
            *in_degree.entry(&stage.id).or_insert(0) += 1;
            dependents.entry(dep).or_default().push(&stage.id);
        }
    }

    let mut queue: VecDeque<&StageId> = in_degree
        .iter()
        .filter(|(_, &deg)| deg == 0)
        .map(|(&id, _)| id)
        .collect();
    let mut order: Vec<StageId> = Vec::with_capacity(stages.len());

    while let Some(id) = queue.pop_front() {
        order.push(id.clone());
        if let Some(deps) = dependents.get(id) {
            for dep in deps {
                let deg = in_degree.get_mut(dep).unwrap();
                *deg -= 1;
                if *deg == 0 {
                    queue.push_back(dep);
                }
            }
        }
    }

    if order.len() != stages.len() {
        let remaining: Vec<StageId> = stages
            .iter()
            .filter(|s| !order.contains(&s.id))
            .map(|s| s.id.clone())
            .collect();
        Err(PipelineError::CycleDetected(remaining))
    } else {
        Ok(order)
    }
}

fn detect_cycle(stages: &[PipelineStage]) -> Result<(), PipelineError> {
    topological_sort(stages).map(|_| ())
}

pub fn interpolate_prompt(
    template: &str,
    outputs: &HashMap<StageId, String>,
) -> String {
    let mut result = template.to_string();
    for (stage_id, output) in outputs {
        let placeholder = format!("{{{{{}}}}}", stage_id.0);
        result = result.replace(&placeholder, output);
    }
    result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageState {
    Pending,
    Running,
    Completed,
    Failed,
    Skipped,
}

impl fmt::Display for StageState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StageState::Pending => write!(f, "pending"),
            StageState::Running => write!(f, "running"),
            StageState::Completed => write!(f, "completed"),
            StageState::Failed => write!(f, "failed"),
            StageState::Skipped => write!(f, "skipped"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineRunState {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl fmt::Display for PipelineRunState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PipelineRunState::Pending => write!(f, "pending"),
            PipelineRunState::Running => write!(f, "running"),
            PipelineRunState::Completed => write!(f, "completed"),
            PipelineRunState::Failed => write!(f, "failed"),
            PipelineRunState::Cancelled => write!(f, "cancelled"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageRun {
    pub stage_id: StageId,
    pub state: StageState,
    pub session_id: Option<String>,
    pub output: Option<String>,
    pub error: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineRun {
    pub id: String,
    pub pipeline_id: String,
    pub state: PipelineRunState,
    pub stages: BTreeMap<StageId, StageRun>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl PipelineRun {
    pub fn new(id: String, pipeline: &Pipeline) -> Self {
        let now = Utc::now();
        let stages = pipeline
            .stages
            .iter()
            .map(|s| {
                (
                    s.id.clone(),
                    StageRun {
                        stage_id: s.id.clone(),
                        state: StageState::Pending,
                        session_id: None,
                        output: None,
                        error: None,
                        started_at: None,
                        completed_at: None,
                    },
                )
            })
            .collect();
        Self {
            id,
            pipeline_id: pipeline.id.clone(),
            state: PipelineRunState::Pending,
            stages,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn start(&mut self) -> Result<(), PipelineError> {
        if self.state != PipelineRunState::Pending {
            return Err(PipelineError::InvalidTransition {
                stage: StageId::new("__pipeline__"),
                from: StageState::Pending,
                event: "start",
            });
        }
        self.state = PipelineRunState::Running;
        self.updated_at = Utc::now();
        Ok(())
    }

    pub fn start_stage(
        &mut self,
        stage_id: &StageId,
        session_id: String,
        pipeline: &Pipeline,
    ) -> Result<(), PipelineError> {
        let stage_def = pipeline
            .stage(stage_id)
            .ok_or_else(|| PipelineError::StageNotFound(stage_id.clone()))?;
        for dep in &stage_def.depends_on {
            let dep_run = self
                .stages
                .get(dep)
                .ok_or_else(|| PipelineError::StageNotFound(dep.clone()))?;
            if dep_run.state != StageState::Completed {
                return Err(PipelineError::DependenciesNotMet(stage_id.clone()));
            }
        }

        let run = self
            .stages
            .get_mut(stage_id)
            .ok_or_else(|| PipelineError::StageNotFound(stage_id.clone()))?;
        if run.state != StageState::Pending {
            return Err(PipelineError::InvalidTransition {
                stage: stage_id.clone(),
                from: run.state,
                event: "start_stage",
            });
        }
        run.state = StageState::Running;
        run.session_id = Some(session_id);
        run.started_at = Some(Utc::now());
        self.updated_at = Utc::now();
        Ok(())
    }

    pub fn complete_stage(
        &mut self,
        stage_id: &StageId,
        output: String,
    ) -> Result<(), PipelineError> {
        let run = self
            .stages
            .get_mut(stage_id)
            .ok_or_else(|| PipelineError::StageNotFound(stage_id.clone()))?;
        if run.state != StageState::Running {
            return Err(PipelineError::InvalidTransition {
                stage: stage_id.clone(),
                from: run.state,
                event: "complete_stage",
            });
        }
        run.state = StageState::Completed;
        run.output = Some(output);
        run.completed_at = Some(Utc::now());
        self.updated_at = Utc::now();

        if self.stages.values().all(|s| s.state == StageState::Completed) {
            self.state = PipelineRunState::Completed;
        }
        Ok(())
    }

    pub fn fail_stage(
        &mut self,
        stage_id: &StageId,
        error: String,
    ) -> Result<(), PipelineError> {
        let run = self
            .stages
            .get_mut(stage_id)
            .ok_or_else(|| PipelineError::StageNotFound(stage_id.clone()))?;
        if run.state != StageState::Running {
            return Err(PipelineError::InvalidTransition {
                stage: stage_id.clone(),
                from: run.state,
                event: "fail_stage",
            });
        }
        run.state = StageState::Failed;
        run.error = Some(error);
        run.completed_at = Some(Utc::now());
        self.state = PipelineRunState::Failed;
        self.updated_at = Utc::now();

        for stage_run in self.stages.values_mut() {
            if stage_run.state == StageState::Pending {
                stage_run.state = StageState::Skipped;
            }
        }
        Ok(())
    }

    pub fn cancel(&mut self) {
        for stage_run in self.stages.values_mut() {
            if stage_run.state == StageState::Pending {
                stage_run.state = StageState::Skipped;
            }
        }
        self.state = PipelineRunState::Cancelled;
        self.updated_at = Utc::now();
    }

    pub fn completed_outputs(&self) -> HashMap<StageId, String> {
        self.stages
            .iter()
            .filter_map(|(id, run)| {
                run.output
                    .as_ref()
                    .map(|o| (id.clone(), o.clone()))
            })
            .collect()
    }

    pub fn runnable_stages(&self, pipeline: &Pipeline) -> Vec<StageId> {
        if self.state != PipelineRunState::Running {
            return Vec::new();
        }
        let completed: BTreeSet<StageId> = self
            .stages
            .iter()
            .filter(|(_, r)| r.state == StageState::Completed)
            .map(|(id, _)| id.clone())
            .collect();
        pipeline
            .stages
            .iter()
            .filter(|s| {
                self.stages
                    .get(&s.id)
                    .map_or(false, |r| r.state == StageState::Pending)
                    && s.depends_on.iter().all(|d| completed.contains(d))
            })
            .map(|s| s.id.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coder_reviewer_tester() -> Pipeline {
        let mut p = Pipeline::new("p1", "Code Review Pipeline");
        p.add_stage(PipelineStage {
            id: StageId::new("coder"),
            agent_type: AgentType::ClaudeCode,
            prompt_template: "Implement feature X".into(),
            depends_on: vec![],
        });
        p.add_stage(PipelineStage {
            id: StageId::new("reviewer"),
            agent_type: AgentType::ClaudeCode,
            prompt_template: "Review the changes from the coder: {{coder}}".into(),
            depends_on: vec![StageId::new("coder")],
        });
        p.add_stage(PipelineStage {
            id: StageId::new("tester"),
            agent_type: AgentType::Pi,
            prompt_template: "Write tests for: {{coder}}".into(),
            depends_on: vec![StageId::new("coder")],
        });
        p
    }

    fn diamond_pipeline() -> Pipeline {
        let mut p = Pipeline::new("p2", "Diamond");
        p.add_stage(PipelineStage {
            id: "start".into(),
            agent_type: AgentType::Pi,
            prompt_template: "Begin".into(),
            depends_on: vec![],
        });
        p.add_stage(PipelineStage {
            id: "left".into(),
            agent_type: AgentType::ClaudeCode,
            prompt_template: "Left path: {{start}}".into(),
            depends_on: vec!["start".into()],
        });
        p.add_stage(PipelineStage {
            id: "right".into(),
            agent_type: AgentType::Codex,
            prompt_template: "Right path: {{start}}".into(),
            depends_on: vec!["start".into()],
        });
        p.add_stage(PipelineStage {
            id: "merge".into(),
            agent_type: AgentType::Pi,
            prompt_template: "Merge {{left}} and {{right}}".into(),
            depends_on: vec!["left".into(), "right".into()],
        });
        p
    }

    #[test]
    fn validate_accepts_valid_dag() {
        coder_reviewer_tester().validate().unwrap();
        diamond_pipeline().validate().unwrap();
    }

    #[test]
    fn validate_rejects_empty_pipeline() {
        let p = Pipeline::new("e", "Empty");
        assert!(matches!(p.validate(), Err(PipelineError::EmptyPipeline)));
    }

    #[test]
    fn validate_rejects_duplicate_stage_ids() {
        let mut p = Pipeline::new("d", "Dup");
        p.add_stage(PipelineStage {
            id: "a".into(),
            agent_type: AgentType::Pi,
            prompt_template: "".into(),
            depends_on: vec![],
        });
        p.add_stage(PipelineStage {
            id: "a".into(),
            agent_type: AgentType::Pi,
            prompt_template: "".into(),
            depends_on: vec![],
        });
        assert!(matches!(
            p.validate(),
            Err(PipelineError::DuplicateStage(_))
        ));
    }

    #[test]
    fn validate_rejects_unknown_dependency() {
        let mut p = Pipeline::new("u", "Unknown dep");
        p.add_stage(PipelineStage {
            id: "a".into(),
            agent_type: AgentType::Pi,
            prompt_template: "".into(),
            depends_on: vec!["ghost".into()],
        });
        assert!(matches!(
            p.validate(),
            Err(PipelineError::UnknownDependency { .. })
        ));
    }

    #[test]
    fn validate_rejects_cycle() {
        let mut p = Pipeline::new("c", "Cycle");
        p.add_stage(PipelineStage {
            id: "a".into(),
            agent_type: AgentType::Pi,
            prompt_template: "".into(),
            depends_on: vec!["b".into()],
        });
        p.add_stage(PipelineStage {
            id: "b".into(),
            agent_type: AgentType::Pi,
            prompt_template: "".into(),
            depends_on: vec!["a".into()],
        });
        assert!(matches!(
            p.validate(),
            Err(PipelineError::CycleDetected(_))
        ));
    }

    #[test]
    fn topological_order_respects_dependencies() {
        let order = coder_reviewer_tester().topological_order().unwrap();
        let pos = |id: &str| order.iter().position(|s| s.0 == id).unwrap();
        assert!(pos("coder") < pos("reviewer"));
        assert!(pos("coder") < pos("tester"));
    }

    #[test]
    fn topological_order_diamond_has_merge_last() {
        let order = diamond_pipeline().topological_order().unwrap();
        let pos = |id: &str| order.iter().position(|s| s.0 == id).unwrap();
        assert!(pos("start") < pos("left"));
        assert!(pos("start") < pos("right"));
        assert!(pos("left") < pos("merge"));
        assert!(pos("right") < pos("merge"));
    }

    #[test]
    fn ready_stages_returns_roots_initially() {
        let p = coder_reviewer_tester();
        let ready = p.ready_stages(&BTreeSet::new());
        assert_eq!(ready, vec![StageId::new("coder")]);
    }

    #[test]
    fn ready_stages_unlocks_dependents_after_completion() {
        let p = coder_reviewer_tester();
        let completed: BTreeSet<StageId> = [StageId::new("coder")].into();
        let mut ready = p.ready_stages(&completed);
        ready.sort();
        assert_eq!(
            ready,
            vec![StageId::new("reviewer"), StageId::new("tester")]
        );
    }

    #[test]
    fn ready_stages_diamond_merge_needs_both() {
        let p = diamond_pipeline();
        let completed: BTreeSet<StageId> =
            [StageId::new("start"), StageId::new("left")].into();
        let ready = p.ready_stages(&completed);
        assert_eq!(ready, vec![StageId::new("right")]);

        let completed: BTreeSet<StageId> = [
            StageId::new("start"),
            StageId::new("left"),
            StageId::new("right"),
        ]
        .into();
        let ready = p.ready_stages(&completed);
        assert_eq!(ready, vec![StageId::new("merge")]);
    }

    #[test]
    fn interpolate_prompt_replaces_placeholders() {
        let mut outputs = HashMap::new();
        outputs.insert(StageId::new("coder"), "diff: +foo -bar".into());
        let result = interpolate_prompt("Review: {{coder}}", &outputs);
        assert_eq!(result, "Review: diff: +foo -bar");
    }

    #[test]
    fn interpolate_prompt_handles_multiple_placeholders() {
        let mut outputs = HashMap::new();
        outputs.insert(StageId::new("left"), "L".into());
        outputs.insert(StageId::new("right"), "R".into());
        let result = interpolate_prompt("Merge {{left}} and {{right}}", &outputs);
        assert_eq!(result, "Merge L and R");
    }

    #[test]
    fn interpolate_prompt_leaves_unknown_placeholders() {
        let result = interpolate_prompt("{{unknown}} stays", &HashMap::new());
        assert_eq!(result, "{{unknown}} stays");
    }

    #[test]
    fn pipeline_run_lifecycle_happy_path() {
        let p = coder_reviewer_tester();
        let mut run = PipelineRun::new("run-1".into(), &p);
        assert_eq!(run.state, PipelineRunState::Pending);
        assert_eq!(run.stages.len(), 3);

        run.start().unwrap();
        assert_eq!(run.state, PipelineRunState::Running);

        let runnable = run.runnable_stages(&p);
        assert_eq!(runnable, vec![StageId::new("coder")]);

        run.start_stage(&StageId::new("coder"), "sess-1".into(), &p)
            .unwrap();
        assert_eq!(
            run.stages[&StageId::new("coder")].state,
            StageState::Running
        );
        assert!(run.runnable_stages(&p).is_empty());

        run.complete_stage(&StageId::new("coder"), "diff output".into())
            .unwrap();
        assert_eq!(run.state, PipelineRunState::Running);

        let mut runnable = run.runnable_stages(&p);
        runnable.sort();
        assert_eq!(
            runnable,
            vec![StageId::new("reviewer"), StageId::new("tester")]
        );

        run.start_stage(&StageId::new("reviewer"), "sess-2".into(), &p)
            .unwrap();
        run.start_stage(&StageId::new("tester"), "sess-3".into(), &p)
            .unwrap();
        run.complete_stage(&StageId::new("reviewer"), "lgtm".into())
            .unwrap();
        run.complete_stage(&StageId::new("tester"), "all pass".into())
            .unwrap();

        assert_eq!(run.state, PipelineRunState::Completed);
    }

    #[test]
    fn pipeline_run_failure_skips_pending_stages() {
        let p = coder_reviewer_tester();
        let mut run = PipelineRun::new("run-2".into(), &p);
        run.start().unwrap();
        run.start_stage(&StageId::new("coder"), "sess-1".into(), &p)
            .unwrap();
        run.fail_stage(&StageId::new("coder"), "compile error".into())
            .unwrap();

        assert_eq!(run.state, PipelineRunState::Failed);
        assert_eq!(
            run.stages[&StageId::new("reviewer")].state,
            StageState::Skipped
        );
        assert_eq!(
            run.stages[&StageId::new("tester")].state,
            StageState::Skipped
        );
        assert!(run.runnable_stages(&p).is_empty());
    }

    #[test]
    fn pipeline_run_cancel_skips_pending() {
        let p = coder_reviewer_tester();
        let mut run = PipelineRun::new("run-3".into(), &p);
        run.start().unwrap();
        run.start_stage(&StageId::new("coder"), "sess-1".into(), &p)
            .unwrap();
        run.cancel();

        assert_eq!(run.state, PipelineRunState::Cancelled);
        assert_eq!(
            run.stages[&StageId::new("coder")].state,
            StageState::Running
        );
        assert_eq!(
            run.stages[&StageId::new("reviewer")].state,
            StageState::Skipped
        );
    }

    #[test]
    fn start_stage_rejects_unmet_dependencies() {
        let p = coder_reviewer_tester();
        let mut run = PipelineRun::new("run-4".into(), &p);
        run.start().unwrap();
        let err = run
            .start_stage(&StageId::new("reviewer"), "sess-1".into(), &p)
            .unwrap_err();
        assert!(matches!(err, PipelineError::DependenciesNotMet(_)));
    }

    #[test]
    fn complete_stage_rejects_non_running() {
        let p = coder_reviewer_tester();
        let mut run = PipelineRun::new("run-5".into(), &p);
        run.start().unwrap();
        let err = run
            .complete_stage(&StageId::new("coder"), "out".into())
            .unwrap_err();
        assert!(matches!(err, PipelineError::InvalidTransition { .. }));
    }

    #[test]
    fn completed_outputs_collects_finished_stage_outputs() {
        let p = coder_reviewer_tester();
        let mut run = PipelineRun::new("run-6".into(), &p);
        run.start().unwrap();
        run.start_stage(&StageId::new("coder"), "s1".into(), &p)
            .unwrap();
        run.complete_stage(&StageId::new("coder"), "the diff".into())
            .unwrap();

        let outputs = run.completed_outputs();
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[&StageId::new("coder")], "the diff");
    }

    #[test]
    fn single_stage_pipeline_completes_immediately() {
        let mut p = Pipeline::new("s", "Single");
        p.add_stage(PipelineStage {
            id: "only".into(),
            agent_type: AgentType::Pi,
            prompt_template: "do it".into(),
            depends_on: vec![],
        });
        p.validate().unwrap();

        let mut run = PipelineRun::new("r".into(), &p);
        run.start().unwrap();
        run.start_stage(&"only".into(), "s1".into(), &p).unwrap();
        run.complete_stage(&"only".into(), "done".into()).unwrap();
        assert_eq!(run.state, PipelineRunState::Completed);
    }

    #[test]
    fn pipeline_serde_roundtrip() {
        let p = coder_reviewer_tester();
        let json = serde_json::to_string(&p).unwrap();
        let back: Pipeline = serde_json::from_str(&json).unwrap();
        assert_eq!(back.stages.len(), 3);
        assert_eq!(back.stages[0].id, StageId::new("coder"));
    }
}
