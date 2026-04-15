//! Rivet AgentOS checkpoint API client.
//!
//! Phase 6a of `SPEC.md`: thin wrappers around the four checkpoint actor
//! actions exposed by an `agent-os` actor. The companion server-side actions
//! are part of the same `POST /gateway/{actor_id}/action/{name}` surface used
//! by `createSession`, `sendPrompt`, etc. (see [`crate::client`]); this module
//! just adds typed wrappers and JSON deserialization helpers so the rest of
//! the workspace can talk to them without touching `serde_json::Value`.
//!
//! Action names follow the rivetkit naming convention used elsewhere in the
//! agent-os actor (`createSession` / `listSessions` / `destroySession` / …):
//!
//! | Method                          | Action               | Args                                      |
//! |---------------------------------|----------------------|-------------------------------------------|
//! | [`RivetClient::create_checkpoint`]  | `createCheckpoint`   | `(sessionId, options?)`                  |
//! | [`RivetClient::list_checkpoints`]   | `listCheckpoints`    | `(sessionId)`                            |
//! | [`RivetClient::restore_checkpoint`] | `restoreCheckpoint`  | `(checkpointId, options?)`               |
//! | [`RivetClient::delete_checkpoint`]  | `deleteCheckpoint`   | `(checkpointId)`                         |
//!
//! The server-side action handlers live in the rivetkit agent-os actor and
//! are tracked separately. This client is intentionally written ahead of the
//! server work (Phase 6a is in parallel group A with no dependencies) so that
//! 6b/6c/6d can build against a real Rust API surface today.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::client::RivetClient;

/// Snapshot of a checkpoint as returned by `createCheckpoint` / `listCheckpoints`
/// / `restoreCheckpoint`.
///
/// Field naming mirrors the camelCase wire format used by the rest of the
/// agent-os actions. Optional fields default to `None` so the struct stays
/// forward-compatible with future server additions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CheckpointRecord {
    #[serde(rename = "checkpointId")]
    pub checkpoint_id: String,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(rename = "createdAt", default)]
    pub created_at: Option<i64>,
    #[serde(rename = "parentCheckpointId", default)]
    pub parent_checkpoint_id: Option<String>,
    #[serde(default)]
    pub metadata: Value,
}

/// Options accepted by `createCheckpoint`. All fields are optional so callers
/// can pass `CreateCheckpointOptions::default()` for an unlabeled snapshot.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateCheckpointOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

impl CreateCheckpointOptions {
    /// Convenience constructor: a checkpoint with just a human-readable label.
    pub fn with_label(label: impl Into<String>) -> Self {
        Self {
            label: Some(label.into()),
            metadata: None,
        }
    }

    /// `true` when no fields would be serialized — used to decide whether to
    /// pass the options arg at all.
    fn is_empty(&self) -> bool {
        self.label.is_none() && self.metadata.is_none()
    }
}

/// Options accepted by `restoreCheckpoint`. The flag mirrors the agent-os
/// `restoreCheckpoint` semantics: by default the active session is rewound
/// in-place; setting `fork = true` asks the server to spawn a new session
/// branch from the checkpoint instead (used by Phase 6d).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RestoreCheckpointOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fork: Option<bool>,
}

impl RestoreCheckpointOptions {
    fn is_empty(&self) -> bool {
        self.fork.is_none()
    }
}

impl RivetClient {
    /// Create a checkpoint of the current session state.
    ///
    /// Maps to the `createCheckpoint(sessionId, options?)` action. The options
    /// envelope is omitted from the wire payload when no fields are set, so the
    /// server sees a plain `[sessionId]` arg list — consistent with how
    /// `createSession` handles its optional `options` parameter.
    pub async fn create_checkpoint(
        &self,
        actor_id: &str,
        session_id: &str,
        options: CreateCheckpointOptions,
    ) -> Result<CheckpointRecord> {
        let args = build_create_args(session_id, &options);
        self.call_action(actor_id, "createCheckpoint", args).await
    }

    /// List all checkpoints belonging to a session, ordered by `createdAt`
    /// ascending (oldest first). The server defines the ordering; the client
    /// just forwards whatever it receives.
    ///
    /// Maps to the `listCheckpoints(sessionId)` action.
    pub async fn list_checkpoints(
        &self,
        actor_id: &str,
        session_id: &str,
    ) -> Result<Vec<CheckpointRecord>> {
        let args = vec![Value::String(session_id.to_string())];
        self.call_action(actor_id, "listCheckpoints", args).await
    }

    /// Restore a checkpoint, either rewinding the session in place or forking
    /// it into a new session branch (when `options.fork == Some(true)`).
    ///
    /// Maps to the `restoreCheckpoint(checkpointId, options?)` action. The
    /// returned record reflects the post-restore session pointer: for an
    /// in-place restore that's the original session at the checkpoint state;
    /// for a fork it points at the freshly-created session.
    pub async fn restore_checkpoint(
        &self,
        actor_id: &str,
        checkpoint_id: &str,
        options: RestoreCheckpointOptions,
    ) -> Result<CheckpointRecord> {
        let args = build_restore_args(checkpoint_id, &options);
        self.call_action(actor_id, "restoreCheckpoint", args).await
    }

    /// Delete a checkpoint by id. The server may refuse to delete a checkpoint
    /// that has dependent forks — that surfaces as an HTTP error from
    /// [`crate::client::RivetClient::call_action`], which the caller can match
    /// on if it needs to distinguish "missing" from "in use".
    ///
    /// Maps to the `deleteCheckpoint(checkpointId)` action. The action returns
    /// no useful payload, so this method discards the body and returns `Ok(())`
    /// on success.
    pub async fn delete_checkpoint(
        &self,
        actor_id: &str,
        checkpoint_id: &str,
    ) -> Result<()> {
        let args = vec![Value::String(checkpoint_id.to_string())];
        let _: Option<Value> = self
            .call_action(actor_id, "deleteCheckpoint", args)
            .await?;
        Ok(())
    }
}

// --- Pure helpers (easy to unit test) ------------------------------------

fn build_create_args(session_id: &str, options: &CreateCheckpointOptions) -> Vec<Value> {
    let mut args = vec![Value::String(session_id.to_string())];
    if !options.is_empty() {
        args.push(serde_json::to_value(options).expect("CreateCheckpointOptions is serializable"));
    }
    args
}

fn build_restore_args(checkpoint_id: &str, options: &RestoreCheckpointOptions) -> Vec<Value> {
    let mut args = vec![Value::String(checkpoint_id.to_string())];
    if !options.is_empty() {
        args.push(serde_json::to_value(options).expect("RestoreCheckpointOptions is serializable"));
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn create_args_omit_options_when_empty() {
        let args = build_create_args("sess_42", &CreateCheckpointOptions::default());
        assert_eq!(args, vec![Value::String("sess_42".into())]);
    }

    #[test]
    fn create_args_include_label_only() {
        let args = build_create_args("sess_42", &CreateCheckpointOptions::with_label("before fix"));
        assert_eq!(
            args,
            vec![
                Value::String("sess_42".into()),
                json!({ "label": "before fix" }),
            ]
        );
    }

    #[test]
    fn create_args_include_metadata() {
        let opts = CreateCheckpointOptions {
            label: Some("turn 3".into()),
            metadata: Some(json!({ "tokens": 1234 })),
        };
        let args = build_create_args("s", &opts);
        assert_eq!(
            args,
            vec![
                Value::String("s".into()),
                json!({ "label": "turn 3", "metadata": { "tokens": 1234 } }),
            ]
        );
    }

    #[test]
    fn restore_args_omit_options_when_empty() {
        let args = build_restore_args("cp_1", &RestoreCheckpointOptions::default());
        assert_eq!(args, vec![Value::String("cp_1".into())]);
    }

    #[test]
    fn restore_args_include_fork_flag() {
        let args = build_restore_args(
            "cp_1",
            &RestoreCheckpointOptions { fork: Some(true) },
        );
        assert_eq!(
            args,
            vec![Value::String("cp_1".into()), json!({ "fork": true })],
        );
    }

    #[test]
    fn checkpoint_record_deserializes_minimal_payload() {
        let rec: CheckpointRecord = serde_json::from_value(json!({
            "checkpointId": "cp_abc",
            "sessionId": "sess_xyz"
        }))
        .unwrap();
        assert_eq!(rec.checkpoint_id, "cp_abc");
        assert_eq!(rec.session_id, "sess_xyz");
        assert!(rec.label.is_none());
        assert!(rec.created_at.is_none());
        assert!(rec.parent_checkpoint_id.is_none());
        assert_eq!(rec.metadata, Value::Null);
    }

    #[test]
    fn checkpoint_record_deserializes_full_payload() {
        let rec: CheckpointRecord = serde_json::from_value(json!({
            "checkpointId": "cp_abc",
            "sessionId": "sess_xyz",
            "label": "before refactor",
            "createdAt": 1_700_000_000,
            "parentCheckpointId": "cp_root",
            "metadata": { "turn": 7, "agent": "pi" }
        }))
        .unwrap();
        assert_eq!(rec.checkpoint_id, "cp_abc");
        assert_eq!(rec.label.as_deref(), Some("before refactor"));
        assert_eq!(rec.created_at, Some(1_700_000_000));
        assert_eq!(rec.parent_checkpoint_id.as_deref(), Some("cp_root"));
        assert_eq!(rec.metadata, json!({ "turn": 7, "agent": "pi" }));
    }

    #[test]
    fn checkpoint_record_round_trips_through_json() {
        let original = CheckpointRecord {
            checkpoint_id: "cp_1".into(),
            session_id: "s_1".into(),
            label: Some("turn 1".into()),
            created_at: Some(1_700_000_000),
            parent_checkpoint_id: None,
            metadata: json!({ "k": "v" }),
        };
        let json = serde_json::to_value(&original).unwrap();
        let decoded: CheckpointRecord = serde_json::from_value(json).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn create_checkpoint_options_with_label_helper() {
        let opts = CreateCheckpointOptions::with_label("hello");
        assert_eq!(opts.label.as_deref(), Some("hello"));
        assert!(opts.metadata.is_none());
        assert!(!opts.is_empty());
    }

    #[test]
    fn empty_options_are_empty() {
        assert!(CreateCheckpointOptions::default().is_empty());
        assert!(RestoreCheckpointOptions::default().is_empty());
    }
}
