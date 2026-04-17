//! Fork-from-checkpoint flow (spec task 6d).
//!
//! Provides the pure data types and validation logic for forking a session
//! from a checkpoint. The actual Rivet API call and UI wiring live in the
//! app crate — this module only defines the request/result types and the
//! local DB bookkeeping so the core crate stays UI-free.

use anyhow::{anyhow, Result};

use crate::checkpoint::{Checkpoint, CheckpointService, CreateCheckpoint};

/// Everything the caller needs to kick off a fork.
#[derive(Debug, Clone)]
pub struct ForkRequest {
    /// The checkpoint to fork from.
    pub checkpoint_id: String,
    /// Session that owns the checkpoint.
    pub session_id: String,
    /// New session id for the forked branch (caller generates this).
    pub new_session_id: String,
}

/// Result of a successful fork, before any Rivet call.
#[derive(Debug, Clone)]
pub struct ForkResult {
    /// The newly-created checkpoint record that represents the fork point.
    pub checkpoint: Checkpoint,
    /// The source checkpoint that was forked from.
    pub source_checkpoint: Checkpoint,
}

/// Validate and record a fork in the local checkpoint store.
///
/// This does NOT call Rivet — the caller is responsible for calling
/// `rivet_client.restore_checkpoint(checkpoint_id, { fork: true })` and
/// then linking the returned rivet checkpoint id via
/// `checkpoint_service.link_rivet_checkpoint`.
///
/// Steps:
/// 1. Verify the source checkpoint exists and belongs to `session_id`.
/// 2. Verify the source checkpoint has a rivet_checkpoint_id (can't fork
///    an unlinked checkpoint).
/// 3. Create a new checkpoint in the DB for the forked session, with
///    `parent_checkpoint_id` pointing to the source.
pub fn prepare_fork(service: &CheckpointService<'_>, req: &ForkRequest) -> Result<ForkResult> {
    let source = service
        .require(&req.checkpoint_id)
        .map_err(|_| anyhow!("source checkpoint {} not found", req.checkpoint_id))?;

    if source.session_id != req.session_id {
        return Err(anyhow!(
            "checkpoint {} belongs to session {}, not {}",
            req.checkpoint_id,
            source.session_id,
            req.session_id,
        ));
    }

    if source.rivet_checkpoint_id.is_none() {
        return Err(anyhow!(
            "checkpoint {} has no rivet checkpoint id — cannot fork an unlinked checkpoint",
            req.checkpoint_id,
        ));
    }

    let fork_checkpoint = service.create(CreateCheckpoint {
        session_id: req.new_session_id.clone(),
        rivet_checkpoint_id: None, // linked after the Rivet call succeeds
        parent_checkpoint_id: Some(req.checkpoint_id.clone()),
        label: Some(format!(
            "fork from {}",
            source.label.as_deref().unwrap_or(&req.checkpoint_id[..8.min(req.checkpoint_id.len())])
        )),
        metadata: Some(serde_json::json!({
            "forked_from_checkpoint": req.checkpoint_id,
            "forked_from_session": req.session_id,
        })),
    })?;

    Ok(ForkResult {
        checkpoint: fork_checkpoint,
        source_checkpoint: source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkpoint::CreateCheckpoint;
    use cuartel_db::workspaces::WorkspaceRepo;
    use cuartel_db::Database;

    fn db() -> Database {
        Database::open_in_memory().unwrap()
    }

    fn seed_session(db: &Database, id: &str) {
        // Use INSERT OR IGNORE for workspace and server since multiple
        // sessions share them in these tests.
        let _ = WorkspaceRepo::new(db).insert("ws1", "test", "/tmp/test");
        db.conn()
            .execute(
                "INSERT OR IGNORE INTO servers (id, name, address, is_local) VALUES ('srv1', 'local', 'localhost', 1)",
                [],
            )
            .unwrap();
        db.conn()
            .execute(
                &format!(
                    "INSERT INTO sessions (id, workspace_id, server_id, agent_type) VALUES ('{id}', 'ws1', 'srv1', 'pi')"
                ),
                [],
            )
            .unwrap();
    }

    #[test]
    fn prepare_fork_success() {
        let db = db();
        seed_session(&db, "sess1");
        seed_session(&db, "sess-fork");
        let svc = CheckpointService::new(&db);

        let cp = svc
            .create(CreateCheckpoint {
                session_id: "sess1".into(),
                rivet_checkpoint_id: Some("rivet_abc".into()),
                parent_checkpoint_id: None,
                label: Some("turn 3".into()),
                metadata: None,
            })
            .unwrap();

        let req = ForkRequest {
            checkpoint_id: cp.id.clone(),
            session_id: "sess1".into(),
            new_session_id: "sess-fork".into(),
        };

        let result = prepare_fork(&svc, &req).unwrap();
        assert_eq!(result.source_checkpoint.id, cp.id);
        assert_eq!(result.checkpoint.session_id, "sess-fork");
        assert_eq!(
            result.checkpoint.parent_checkpoint_id.as_deref(),
            Some(cp.id.as_str())
        );
        assert!(result
            .checkpoint
            .label
            .as_deref()
            .unwrap()
            .contains("fork from turn 3"));
    }

    #[test]
    fn prepare_fork_wrong_session() {
        let db = db();
        seed_session(&db, "sess1");
        seed_session(&db, "sess2");
        let svc = CheckpointService::new(&db);

        let cp = svc
            .create(CreateCheckpoint {
                session_id: "sess1".into(),
                rivet_checkpoint_id: Some("rivet_abc".into()),
                parent_checkpoint_id: None,
                label: None,
                metadata: None,
            })
            .unwrap();

        let req = ForkRequest {
            checkpoint_id: cp.id,
            session_id: "sess2".into(),
            new_session_id: "sess-fork".into(),
        };

        let err = prepare_fork(&svc, &req).unwrap_err();
        assert!(err.to_string().contains("belongs to session sess1"));
    }

    #[test]
    fn prepare_fork_unlinked_checkpoint() {
        let db = db();
        seed_session(&db, "sess1");
        let svc = CheckpointService::new(&db);

        let cp = svc
            .create(CreateCheckpoint {
                session_id: "sess1".into(),
                rivet_checkpoint_id: None,
                parent_checkpoint_id: None,
                label: None,
                metadata: None,
            })
            .unwrap();

        let req = ForkRequest {
            checkpoint_id: cp.id,
            session_id: "sess1".into(),
            new_session_id: "sess-fork".into(),
        };

        let err = prepare_fork(&svc, &req).unwrap_err();
        assert!(err.to_string().contains("unlinked checkpoint"));
    }

    #[test]
    fn prepare_fork_missing_checkpoint() {
        let db = db();
        seed_session(&db, "sess1");
        let svc = CheckpointService::new(&db);

        let req = ForkRequest {
            checkpoint_id: "nonexistent".into(),
            session_id: "sess1".into(),
            new_session_id: "sess-fork".into(),
        };

        let err = prepare_fork(&svc, &req).unwrap_err();
        assert!(err.to_string().contains("not found"));
    }
}
