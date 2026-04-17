use std::collections::BTreeMap;
use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::pipeline::StageId;

pub const HANDOFF_DIR: &str = "/handoff";

pub const OUTPUT_SUBDIR: &str = "output";
pub const INPUT_SUBDIR: &str = "input";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ArtifactId(pub String);

impl ArtifactId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl fmt::Display for ArtifactId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for ArtifactId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    File,
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    pub id: ArtifactId,
    pub kind: ArtifactKind,
    pub path: String,
    pub producer: StageId,
    pub size_bytes: Option<u64>,
    pub content_hash: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl Artifact {
    pub fn file(
        id: impl Into<ArtifactId>,
        path: impl Into<String>,
        producer: impl Into<StageId>,
    ) -> Self {
        Self {
            id: id.into(),
            kind: ArtifactKind::File,
            path: path.into(),
            producer: producer.into(),
            size_bytes: None,
            content_hash: None,
            created_at: Utc::now(),
        }
    }

    pub fn directory(
        id: impl Into<ArtifactId>,
        path: impl Into<String>,
        producer: impl Into<StageId>,
    ) -> Self {
        Self {
            id: id.into(),
            kind: ArtifactKind::Directory,
            path: path.into(),
            producer: producer.into(),
            size_bytes: None,
            content_hash: None,
            created_at: Utc::now(),
        }
    }

    pub fn with_size(mut self, size: u64) -> Self {
        self.size_bytes = Some(size);
        self
    }

    pub fn with_hash(mut self, hash: impl Into<String>) -> Self {
        self.content_hash = Some(hash.into());
        self
    }

    pub fn vm_output_path(&self) -> String {
        format!("{}/{}/{}", HANDOFF_DIR, OUTPUT_SUBDIR, self.path)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub artifact_id: ArtifactId,
    pub source_stage: StageId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageManifest {
    pub stage_id: StageId,
    pub produces: Vec<Artifact>,
    pub consumes: Vec<ArtifactRef>,
}

impl StageManifest {
    pub fn new(stage_id: impl Into<StageId>) -> Self {
        Self {
            stage_id: stage_id.into(),
            produces: Vec::new(),
            consumes: Vec::new(),
        }
    }

    pub fn produce(mut self, artifact: Artifact) -> Self {
        self.produces.push(artifact);
        self
    }

    pub fn consume(mut self, artifact_id: impl Into<ArtifactId>, source_stage: impl Into<StageId>) -> Self {
        self.consumes.push(ArtifactRef {
            artifact_id: artifact_id.into(),
            source_stage: source_stage.into(),
        });
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilePassingError {
    ArtifactNotFound { artifact_id: ArtifactId, source_stage: StageId },
    ProducerMismatch { artifact_id: ArtifactId, expected: StageId, actual: StageId },
    DuplicateArtifact(ArtifactId),
}

impl fmt::Display for FilePassingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FilePassingError::ArtifactNotFound { artifact_id, source_stage } => {
                write!(f, "artifact {artifact_id} not found from stage {source_stage}")
            }
            FilePassingError::ProducerMismatch { artifact_id, expected, actual } => {
                write!(
                    f,
                    "artifact {artifact_id} expected from {expected} but produced by {actual}"
                )
            }
            FilePassingError::DuplicateArtifact(id) => {
                write!(f, "duplicate artifact id: {id}")
            }
        }
    }
}

impl std::error::Error for FilePassingError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferEntry {
    pub artifact_id: ArtifactId,
    pub source_stage: StageId,
    pub target_stage: StageId,
    pub source_vm_path: String,
    pub target_vm_path: String,
    pub kind: ArtifactKind,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TransferPlan {
    pub transfers: Vec<TransferEntry>,
}

impl TransferPlan {
    pub fn is_empty(&self) -> bool {
        self.transfers.is_empty()
    }

    pub fn len(&self) -> usize {
        self.transfers.len()
    }
}

pub fn plan_transfers(
    manifests: &[StageManifest],
) -> Result<TransferPlan, FilePassingError> {
    let mut produced: BTreeMap<(&ArtifactId, &StageId), &Artifact> = BTreeMap::new();
    let mut ids_seen: BTreeMap<&ArtifactId, &StageId> = BTreeMap::new();

    for manifest in manifests {
        for artifact in &manifest.produces {
            if let Some(&existing_stage) = ids_seen.get(&artifact.id) {
                if existing_stage != &manifest.stage_id {
                    return Err(FilePassingError::DuplicateArtifact(artifact.id.clone()));
                }
            }
            ids_seen.insert(&artifact.id, &manifest.stage_id);
            produced.insert((&artifact.id, &manifest.stage_id), artifact);
        }
    }

    let mut transfers = Vec::new();
    for manifest in manifests {
        for consume in &manifest.consumes {
            let artifact = produced
                .get(&(&consume.artifact_id, &consume.source_stage))
                .ok_or_else(|| FilePassingError::ArtifactNotFound {
                    artifact_id: consume.artifact_id.clone(),
                    source_stage: consume.source_stage.clone(),
                })?;

            if artifact.producer != consume.source_stage {
                return Err(FilePassingError::ProducerMismatch {
                    artifact_id: consume.artifact_id.clone(),
                    expected: consume.source_stage.clone(),
                    actual: artifact.producer.clone(),
                });
            }

            let source_vm_path = artifact.vm_output_path();
            let target_vm_path = format!(
                "{}/{}/{}/{}",
                HANDOFF_DIR,
                INPUT_SUBDIR,
                consume.source_stage.0,
                artifact.path
            );

            transfers.push(TransferEntry {
                artifact_id: consume.artifact_id.clone(),
                source_stage: consume.source_stage.clone(),
                target_stage: manifest.stage_id.clone(),
                source_vm_path,
                target_vm_path,
                kind: artifact.kind.clone(),
            });
        }
    }

    Ok(TransferPlan { transfers })
}

pub fn vm_output_dir(_stage_id: &StageId) -> String {
    format!("{}/{}", HANDOFF_DIR, OUTPUT_SUBDIR)
}

pub fn vm_input_dir(_stage_id: &StageId, source_stage: &StageId) -> String {
    format!("{}/{}/{}", HANDOFF_DIR, INPUT_SUBDIR, source_stage.0)
}

pub fn vm_input_file(_stage_id: &StageId, source_stage: &StageId, relative_path: &str) -> String {
    format!(
        "{}/{}/{}/{}",
        HANDOFF_DIR, INPUT_SUBDIR, source_stage.0, relative_path
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_vm_output_path() {
        let a = Artifact::file("diff", "changes.patch", "coder");
        assert_eq!(a.vm_output_path(), "/handoff/output/changes.patch");
    }

    #[test]
    fn artifact_directory_vm_output_path() {
        let a = Artifact::directory("src", "src/", "coder");
        assert_eq!(a.vm_output_path(), "/handoff/output/src/");
    }

    #[test]
    fn stage_manifest_builder() {
        let m = StageManifest::new("coder")
            .produce(Artifact::file("diff", "changes.patch", "coder"))
            .consume("deps", "setup");
        assert_eq!(m.produces.len(), 1);
        assert_eq!(m.consumes.len(), 1);
        assert_eq!(m.consumes[0].source_stage, StageId::new("setup"));
    }

    #[test]
    fn plan_transfers_simple_pipeline() {
        let coder = StageManifest::new("coder")
            .produce(Artifact::file("diff", "changes.patch", "coder"));
        let reviewer = StageManifest::new("reviewer")
            .consume("diff", "coder");

        let plan = plan_transfers(&[coder, reviewer]).unwrap();
        assert_eq!(plan.len(), 1);

        let t = &plan.transfers[0];
        assert_eq!(t.artifact_id, ArtifactId::new("diff"));
        assert_eq!(t.source_stage, StageId::new("coder"));
        assert_eq!(t.target_stage, StageId::new("reviewer"));
        assert_eq!(t.source_vm_path, "/handoff/output/changes.patch");
        assert_eq!(t.target_vm_path, "/handoff/input/coder/changes.patch");
    }

    #[test]
    fn plan_transfers_fan_out() {
        let coder = StageManifest::new("coder")
            .produce(Artifact::file("diff", "changes.patch", "coder"));
        let reviewer = StageManifest::new("reviewer").consume("diff", "coder");
        let tester = StageManifest::new("tester").consume("diff", "coder");

        let plan = plan_transfers(&[coder, reviewer, tester]).unwrap();
        assert_eq!(plan.len(), 2);
        assert_eq!(plan.transfers[0].target_stage, StageId::new("reviewer"));
        assert_eq!(plan.transfers[1].target_stage, StageId::new("tester"));
    }

    #[test]
    fn plan_transfers_chain() {
        let a = StageManifest::new("a")
            .produce(Artifact::file("f1", "out.txt", "a"));
        let b = StageManifest::new("b")
            .consume("f1", "a")
            .produce(Artifact::file("f2", "result.json", "b"));
        let c = StageManifest::new("c")
            .consume("f2", "b");

        let plan = plan_transfers(&[a, b, c]).unwrap();
        assert_eq!(plan.len(), 2);
        assert_eq!(plan.transfers[0].source_stage, StageId::new("a"));
        assert_eq!(plan.transfers[0].target_stage, StageId::new("b"));
        assert_eq!(plan.transfers[1].source_stage, StageId::new("b"));
        assert_eq!(plan.transfers[1].target_stage, StageId::new("c"));
    }

    #[test]
    fn plan_transfers_no_consumers_is_empty() {
        let coder = StageManifest::new("coder")
            .produce(Artifact::file("diff", "changes.patch", "coder"));
        let plan = plan_transfers(&[coder]).unwrap();
        assert!(plan.is_empty());
    }

    #[test]
    fn plan_transfers_rejects_missing_artifact() {
        let reviewer = StageManifest::new("reviewer").consume("ghost", "coder");
        let err = plan_transfers(&[reviewer]).unwrap_err();
        assert!(matches!(err, FilePassingError::ArtifactNotFound { .. }));
    }

    #[test]
    fn plan_transfers_rejects_duplicate_artifact_across_stages() {
        let a = StageManifest::new("a")
            .produce(Artifact::file("same", "f.txt", "a"));
        let b = StageManifest::new("b")
            .produce(Artifact::file("same", "f.txt", "b"));
        let err = plan_transfers(&[a, b]).unwrap_err();
        assert!(matches!(err, FilePassingError::DuplicateArtifact(_)));
    }

    #[test]
    fn vm_directory_helpers() {
        let coder = StageId::new("coder");
        let reviewer = StageId::new("reviewer");
        assert_eq!(vm_output_dir(&coder), "/handoff/output");
        assert_eq!(vm_input_dir(&reviewer, &coder), "/handoff/input/coder");
        assert_eq!(
            vm_input_file(&reviewer, &coder, "changes.patch"),
            "/handoff/input/coder/changes.patch"
        );
    }

    #[test]
    fn artifact_builder_methods() {
        let a = Artifact::file("x", "f.txt", "s")
            .with_size(1024)
            .with_hash("sha256:abc123");
        assert_eq!(a.size_bytes, Some(1024));
        assert_eq!(a.content_hash.as_deref(), Some("sha256:abc123"));
    }

    #[test]
    fn transfer_plan_serde_roundtrip() {
        let coder = StageManifest::new("coder")
            .produce(Artifact::file("diff", "changes.patch", "coder"));
        let reviewer = StageManifest::new("reviewer").consume("diff", "coder");
        let plan = plan_transfers(&[coder, reviewer]).unwrap();

        let json = serde_json::to_string(&plan).unwrap();
        let back: TransferPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back.transfers[0].artifact_id, ArtifactId::new("diff"));
    }
}
