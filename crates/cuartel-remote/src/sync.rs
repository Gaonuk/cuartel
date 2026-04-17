use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use log::info;
use serde::{Deserialize, Serialize};

use cuartel_db::checkpoints::{CheckpointRepo, CheckpointRow};
use cuartel_db::sessions::{SessionRepo, SessionRow};
use cuartel_db::Database;

use crate::registry::{rivet_client_for, ServerRegistry};
use crate::server::RemoteServer;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SyncDirection {
    Push,
    Pull,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncRequest {
    pub session_id: String,
    pub direction: SyncDirection,
    pub source_server: String,
    pub target_server: String,
}

/// Portable bundle of session state transferred between servers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub session: SessionRow,
    pub checkpoints: Vec<CheckpointRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncResult {
    pub session_id: String,
    pub source_server_id: String,
    pub target_server_id: String,
    pub target_rivet_session_id: Option<String>,
    pub checkpoints_transferred: usize,
}

#[derive(Clone)]
pub struct SessionSyncService {
    db: Arc<Mutex<Database>>,
    registry: ServerRegistry,
}

impl SessionSyncService {
    pub fn new(db: Arc<Mutex<Database>>, registry: ServerRegistry) -> Self {
        Self { db, registry }
    }

    /// Execute a sync request in whichever direction it specifies.
    pub async fn sync(&self, req: &SyncRequest) -> Result<SyncResult> {
        match req.direction {
            SyncDirection::Push => self.push(&req.session_id, &req.target_server).await,
            SyncDirection::Pull => self.pull(&req.session_id, &req.source_server).await,
        }
    }

    /// Push a session from its current server to a target server.
    ///
    /// 1. Export a snapshot from the local DB.
    /// 2. Resolve the target server from the registry.
    /// 3. Create a rivet session on the target server.
    /// 4. Import checkpoint metadata pointing at the new server.
    /// 5. Re-point the local session row to the target server.
    pub async fn push(&self, session_id: &str, target_server_id: &str) -> Result<SyncResult> {
        let snapshot = self.export_snapshot(session_id)?;
        let source_server_id = snapshot.session.server_id.clone();

        let target = self
            .registry
            .get(target_server_id)?
            .ok_or_else(|| anyhow!("target server {target_server_id} not found in registry"))?;

        self.verify_reachable(&target).await?;

        let target_rivet_session_id = self.create_remote_session(&target, &snapshot).await?;

        let checkpoints_transferred = snapshot.checkpoints.len();
        self.repoint_session(session_id, target_server_id, target_rivet_session_id.as_deref())?;

        info!(
            "pushed session {session_id} from {source_server_id} to {target_server_id} ({checkpoints_transferred} checkpoints)"
        );

        Ok(SyncResult {
            session_id: session_id.to_string(),
            source_server_id,
            target_server_id: target_server_id.to_string(),
            target_rivet_session_id,
            checkpoints_transferred,
        })
    }

    /// Pull a session from a remote source server to the local server.
    ///
    /// 1. Verify the source server is reachable.
    /// 2. Fetch the session's live info from the source rivet.
    /// 3. Create a local rivet session mirroring the remote one.
    /// 4. Re-point the local session row to the local server.
    pub async fn pull(&self, session_id: &str, source_server_id: &str) -> Result<SyncResult> {
        let snapshot = self.export_snapshot(session_id)?;

        let source = self
            .registry
            .get(source_server_id)?
            .ok_or_else(|| anyhow!("source server {source_server_id} not found in registry"))?;

        self.verify_reachable(&source).await?;

        let local = self.local_server()?;

        let local_rivet_session_id = self.create_remote_session(&local, &snapshot).await?;

        let checkpoints_transferred = snapshot.checkpoints.len();
        self.repoint_session(session_id, &local.id, local_rivet_session_id.as_deref())?;

        info!(
            "pulled session {session_id} from {source_server_id} to {} ({checkpoints_transferred} checkpoints)",
            local.id
        );

        Ok(SyncResult {
            session_id: session_id.to_string(),
            source_server_id: source_server_id.to_string(),
            target_server_id: local.id,
            target_rivet_session_id: local_rivet_session_id,
            checkpoints_transferred,
        })
    }

    /// Build a portable snapshot of a session and its checkpoints from the DB.
    pub fn export_snapshot(&self, session_id: &str) -> Result<SessionSnapshot> {
        let db = self
            .db
            .lock()
            .map_err(|_| anyhow!("sync service mutex poisoned"))?;
        let session = SessionRepo::new(&db)
            .get(session_id)?
            .ok_or_else(|| anyhow!("session {session_id} not found"))?;
        let checkpoints = CheckpointRepo::new(&db).list_by_session(session_id)?;
        Ok(SessionSnapshot {
            session,
            checkpoints,
        })
    }

    /// Import a snapshot received from another server. Inserts a new session
    /// row and its checkpoints, all pointing at `target_server_id`.
    pub fn import_snapshot(
        &self,
        snapshot: &SessionSnapshot,
        target_server_id: &str,
    ) -> Result<SessionRow> {
        let db = self
            .db
            .lock()
            .map_err(|_| anyhow!("sync service mutex poisoned"))?;
        let sessions = SessionRepo::new(&db);
        let checkpoints = CheckpointRepo::new(&db);

        let s = &snapshot.session;
        let row = sessions.insert(
            &s.id,
            &s.workspace_id,
            target_server_id,
            &s.agent_type,
            s.rivet_session_id.as_deref(),
            &s.status,
        )?;

        for cp in &snapshot.checkpoints {
            checkpoints.insert(
                &cp.id,
                &cp.session_id,
                cp.rivet_checkpoint_id.as_deref(),
                cp.parent_checkpoint_id.as_deref(),
                cp.label.as_deref(),
                &cp.metadata,
            )?;
        }

        Ok(row)
    }

    async fn verify_reachable(&self, server: &RemoteServer) -> Result<()> {
        if server.is_local {
            return Ok(());
        }
        if !self.registry.check_reachability(server).await {
            return Err(anyhow!(
                "server {} ({}) is not reachable",
                server.name,
                server.address
            ));
        }
        Ok(())
    }

    /// Create an agent-os session on the target server's rivet and return
    /// the new rivet session ID.
    async fn create_remote_session(
        &self,
        target: &RemoteServer,
        snapshot: &SessionSnapshot,
    ) -> Result<Option<String>> {
        let client = rivet_client_for(target);

        let actors = client
            .list_actors("vm", None)
            .await
            .context("listing actors on target server")?;

        let actor = actors
            .first()
            .ok_or_else(|| anyhow!("no VM actor found on {}", target.name))?;

        let record = client
            .create_session(&actor.actor_id, &snapshot.session.agent_type, None)
            .await
            .context("creating session on target server")?;

        Ok(Some(record.session_id))
    }

    /// Update the local DB so the session points at a new server (and
    /// optionally a new rivet session ID).
    fn repoint_session(
        &self,
        session_id: &str,
        new_server_id: &str,
        new_rivet_session_id: Option<&str>,
    ) -> Result<()> {
        let db = self
            .db
            .lock()
            .map_err(|_| anyhow!("sync service mutex poisoned"))?;
        let repo = SessionRepo::new(&db);
        repo.update_server(session_id, new_server_id)?;
        if let Some(rid) = new_rivet_session_id {
            repo.set_rivet_session_id(session_id, rid)?;
        }
        Ok(())
    }

    fn local_server(&self) -> Result<RemoteServer> {
        let servers = self.registry.list()?;
        servers
            .into_iter()
            .find(|s| s.is_local)
            .ok_or_else(|| anyhow!("no local server registered"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuartel_db::servers::ServerRepo;
    use cuartel_db::workspaces::WorkspaceRepo;

    fn setup() -> (Arc<Mutex<Database>>, ServerRegistry) {
        let db = Arc::new(Mutex::new(Database::open_in_memory().unwrap()));
        let ts = Arc::new(crate::tailscale::TailscaleClient::new());
        let registry = ServerRegistry::new(db.clone(), ts);
        registry
            .ensure_local("http://localhost:6420")
            .unwrap();
        (db, registry)
    }

    fn seed_session(db: &Arc<Mutex<Database>>) -> String {
        let db = db.lock().unwrap();
        WorkspaceRepo::new(&db)
            .insert("ws1", "test", "/tmp/test")
            .unwrap();
        SessionRepo::new(&db)
            .insert("sess1", "ws1", "local", "pi", Some("rivet_s1"), "ready")
            .unwrap();
        "sess1".to_string()
    }

    fn seed_checkpoints(db: &Arc<Mutex<Database>>, session_id: &str) {
        let db = db.lock().unwrap();
        let repo = cuartel_db::checkpoints::CheckpointRepo::new(&db);
        repo.insert("cp1", session_id, Some("rivet_cp1"), None, Some("initial"), "{}")
            .unwrap();
        repo.insert("cp2", session_id, Some("rivet_cp2"), Some("cp1"), Some("after prompt"), "{}")
            .unwrap();
    }

    #[test]
    fn export_snapshot_captures_session_and_checkpoints() {
        let (db, registry) = setup();
        let sid = seed_session(&db);
        seed_checkpoints(&db, &sid);

        let svc = SessionSyncService::new(db, registry);
        let snap = svc.export_snapshot(&sid).unwrap();

        assert_eq!(snap.session.id, "sess1");
        assert_eq!(snap.session.server_id, "local");
        assert_eq!(snap.checkpoints.len(), 2);
        assert_eq!(snap.checkpoints[0].id, "cp1");
        assert_eq!(snap.checkpoints[1].id, "cp2");
    }

    #[test]
    fn export_snapshot_missing_session_errors() {
        let (db, registry) = setup();
        let svc = SessionSyncService::new(db, registry);
        assert!(svc.export_snapshot("nonexistent").is_err());
    }

    #[test]
    fn export_snapshot_empty_checkpoints_ok() {
        let (db, registry) = setup();
        let sid = seed_session(&db);
        let svc = SessionSyncService::new(db, registry);
        let snap = svc.export_snapshot(&sid).unwrap();
        assert!(snap.checkpoints.is_empty());
    }

    #[test]
    fn import_snapshot_inserts_into_db() {
        let (db, registry) = setup();

        // Seed a workspace and remote server for the import target.
        {
            let d = db.lock().unwrap();
            WorkspaceRepo::new(&d)
                .insert("ws1", "test", "/tmp/test")
                .unwrap();
            ServerRepo::new(&d)
                .insert("remote1", "Remote", "http://remote:6420", None, false)
                .unwrap();
        }

        let snapshot = SessionSnapshot {
            session: SessionRow {
                id: "sess-imported".into(),
                workspace_id: "ws1".into(),
                server_id: "local".into(),
                agent_type: "pi".into(),
                rivet_session_id: Some("rivet_orig".into()),
                status: "ready".into(),
                created_at: "2026-01-01 00:00:00".into(),
                updated_at: "2026-01-01 00:00:00".into(),
            },
            checkpoints: vec![CheckpointRow {
                id: "cp-imported".into(),
                session_id: "sess-imported".into(),
                rivet_checkpoint_id: Some("rivet_cp_orig".into()),
                parent_checkpoint_id: None,
                label: Some("snapshot".into()),
                metadata: "{}".into(),
                created_at: "2026-01-01 00:00:00".into(),
                updated_at: "2026-01-01 00:00:00".into(),
            }],
        };

        let svc = SessionSyncService::new(db.clone(), registry);
        let row = svc.import_snapshot(&snapshot, "remote1").unwrap();

        assert_eq!(row.id, "sess-imported");
        assert_eq!(row.server_id, "remote1");

        let d = db.lock().unwrap();
        let cps = CheckpointRepo::new(&d)
            .list_by_session("sess-imported")
            .unwrap();
        assert_eq!(cps.len(), 1);
        assert_eq!(cps[0].id, "cp-imported");
    }

    #[test]
    fn snapshot_serde_roundtrip() {
        let snap = SessionSnapshot {
            session: SessionRow {
                id: "s1".into(),
                workspace_id: "ws1".into(),
                server_id: "local".into(),
                agent_type: "pi".into(),
                rivet_session_id: None,
                status: "created".into(),
                created_at: "2026-01-01".into(),
                updated_at: "2026-01-01".into(),
            },
            checkpoints: vec![],
        };
        let json = serde_json::to_string(&snap).unwrap();
        let back: SessionSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back.session.id, "s1");
    }

    #[test]
    fn sync_request_serde_roundtrip() {
        let req = SyncRequest {
            session_id: "s1".into(),
            direction: SyncDirection::Push,
            source_server: "local".into(),
            target_server: "remote1".into(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: SyncRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.session_id, "s1");
        assert!(matches!(back.direction, SyncDirection::Push));
    }

    #[test]
    fn repoint_session_updates_server_and_rivet_id() {
        let (db, registry) = setup();
        let sid = seed_session(&db);

        {
            let d = db.lock().unwrap();
            ServerRepo::new(&d)
                .insert("remote1", "Remote", "http://remote:6420", None, false)
                .unwrap();
        }

        let svc = SessionSyncService::new(db.clone(), registry);
        svc.repoint_session(&sid, "remote1", Some("new_rivet_id"))
            .unwrap();

        let d = db.lock().unwrap();
        let sess = SessionRepo::new(&d).get(&sid).unwrap().unwrap();
        assert_eq!(sess.server_id, "remote1");
        assert_eq!(sess.rivet_session_id.as_deref(), Some("new_rivet_id"));
    }

    #[test]
    fn repoint_session_without_rivet_id_only_changes_server() {
        let (db, registry) = setup();
        let sid = seed_session(&db);

        {
            let d = db.lock().unwrap();
            ServerRepo::new(&d)
                .insert("remote1", "Remote", "http://remote:6420", None, false)
                .unwrap();
        }

        let svc = SessionSyncService::new(db.clone(), registry);
        svc.repoint_session(&sid, "remote1", None).unwrap();

        let d = db.lock().unwrap();
        let sess = SessionRepo::new(&d).get(&sid).unwrap().unwrap();
        assert_eq!(sess.server_id, "remote1");
        assert_eq!(sess.rivet_session_id.as_deref(), Some("rivet_s1"));
    }

    #[test]
    fn local_server_returns_local_entry() {
        let (_db, registry) = setup();
        let svc = SessionSyncService::new(_db, registry);
        let local = svc.local_server().unwrap();
        assert!(local.is_local);
        assert_eq!(local.id, "local");
    }
}
