use anyhow::{anyhow, Result};
use cuartel_db::checkpoints::{CheckpointRepo, CheckpointRow};
use cuartel_db::Database;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub id: String,
    pub session_id: String,
    pub rivet_checkpoint_id: Option<String>,
    pub parent_checkpoint_id: Option<String>,
    pub label: Option<String>,
    pub metadata: serde_json::Value,
    pub created_at: String,
    pub updated_at: String,
}

impl From<CheckpointRow> for Checkpoint {
    fn from(row: CheckpointRow) -> Self {
        let metadata = serde_json::from_str(&row.metadata).unwrap_or(serde_json::Value::Object(
            serde_json::Map::new(),
        ));
        Self {
            id: row.id,
            session_id: row.session_id,
            rivet_checkpoint_id: row.rivet_checkpoint_id,
            parent_checkpoint_id: row.parent_checkpoint_id,
            label: row.label,
            metadata,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

pub struct CreateCheckpoint {
    pub session_id: String,
    pub rivet_checkpoint_id: Option<String>,
    pub parent_checkpoint_id: Option<String>,
    pub label: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

pub struct CheckpointService<'a> {
    repo: CheckpointRepo<'a>,
}

impl<'a> CheckpointService<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self {
            repo: CheckpointRepo::new(db),
        }
    }

    pub fn create(&self, params: CreateCheckpoint) -> Result<Checkpoint> {
        let id = uuid::Uuid::new_v4().to_string();
        let metadata_str = match &params.metadata {
            Some(v) => serde_json::to_string(v)?,
            None => "{}".to_string(),
        };

        if let Some(parent_id) = &params.parent_checkpoint_id {
            if self.repo.get(parent_id)?.is_none() {
                return Err(anyhow!("parent checkpoint {parent_id} does not exist"));
            }
        }

        let row = self.repo.insert(
            &id,
            &params.session_id,
            params.rivet_checkpoint_id.as_deref(),
            params.parent_checkpoint_id.as_deref(),
            params.label.as_deref(),
            &metadata_str,
        )?;
        Ok(row.into())
    }

    pub fn get(&self, id: &str) -> Result<Option<Checkpoint>> {
        Ok(self.repo.get(id)?.map(Into::into))
    }

    pub fn require(&self, id: &str) -> Result<Checkpoint> {
        self.get(id)?
            .ok_or_else(|| anyhow!("checkpoint {id} not found"))
    }

    pub fn find_by_rivet_id(&self, rivet_checkpoint_id: &str) -> Result<Option<Checkpoint>> {
        Ok(self.repo.find_by_rivet_id(rivet_checkpoint_id)?.map(Into::into))
    }

    pub fn list_by_session(&self, session_id: &str) -> Result<Vec<Checkpoint>> {
        Ok(self
            .repo
            .list_by_session(session_id)?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    pub fn list_children(&self, checkpoint_id: &str) -> Result<Vec<Checkpoint>> {
        Ok(self
            .repo
            .list_children(checkpoint_id)?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    pub fn update_label(&self, id: &str, label: Option<&str>) -> Result<Checkpoint> {
        Ok(self.repo.update_label(id, label)?.into())
    }

    pub fn update_metadata(&self, id: &str, metadata: serde_json::Value) -> Result<Checkpoint> {
        let metadata_str = serde_json::to_string(&metadata)?;
        Ok(self.repo.update_metadata(id, &metadata_str)?.into())
    }

    pub fn link_rivet_checkpoint(&self, id: &str, rivet_checkpoint_id: &str) -> Result<Checkpoint> {
        Ok(self.repo.set_rivet_checkpoint_id(id, rivet_checkpoint_id)?.into())
    }

    pub fn delete(&self, id: &str) -> Result<bool> {
        let children = self.repo.list_children(id)?;
        if !children.is_empty() {
            return Err(anyhow!(
                "checkpoint {id} has {} dependent fork(s); delete them first",
                children.len()
            ));
        }
        self.repo.delete(id)
    }

    pub fn count_by_session(&self, session_id: &str) -> Result<i64> {
        self.repo.count_by_session(session_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuartel_db::workspaces::WorkspaceRepo;
    use serde_json::json;

    fn db() -> Database {
        Database::open_in_memory().unwrap()
    }

    fn seed_session(db: &Database) -> String {
        WorkspaceRepo::new(db)
            .insert("ws1", "test", "/tmp/test")
            .unwrap();
        db.conn()
            .execute(
                "INSERT INTO servers (id, name, address, is_local) VALUES ('srv1', 'local', 'localhost', 1)",
                [],
            )
            .unwrap();
        db.conn()
            .execute(
                "INSERT INTO sessions (id, workspace_id, server_id, agent_type) VALUES ('sess1', 'ws1', 'srv1', 'pi')",
                [],
            )
            .unwrap();
        "sess1".to_string()
    }

    fn base_params(session_id: &str) -> CreateCheckpoint {
        CreateCheckpoint {
            session_id: session_id.to_string(),
            rivet_checkpoint_id: None,
            parent_checkpoint_id: None,
            label: None,
            metadata: None,
        }
    }

    #[test]
    fn create_and_get() {
        let db = db();
        let sid = seed_session(&db);
        let svc = CheckpointService::new(&db);

        let cp = svc
            .create(CreateCheckpoint {
                label: Some("turn 1".into()),
                metadata: Some(json!({"tokens": 100})),
                ..base_params(&sid)
            })
            .unwrap();

        assert_eq!(cp.session_id, sid);
        assert_eq!(cp.label.as_deref(), Some("turn 1"));
        assert_eq!(cp.metadata, json!({"tokens": 100}));

        let got = svc.require(&cp.id).unwrap();
        assert_eq!(got.id, cp.id);
    }

    #[test]
    fn require_missing_errors() {
        let db = db();
        let svc = CheckpointService::new(&db);
        assert!(svc.require("nonexistent").is_err());
    }

    #[test]
    fn create_with_parent() {
        let db = db();
        let sid = seed_session(&db);
        let svc = CheckpointService::new(&db);

        let parent = svc.create(base_params(&sid)).unwrap();
        let child = svc
            .create(CreateCheckpoint {
                parent_checkpoint_id: Some(parent.id.clone()),
                label: Some("fork".into()),
                ..base_params(&sid)
            })
            .unwrap();

        assert_eq!(child.parent_checkpoint_id.as_deref(), Some(parent.id.as_str()));
    }

    #[test]
    fn create_with_invalid_parent_errors() {
        let db = db();
        let sid = seed_session(&db);
        let svc = CheckpointService::new(&db);

        let result = svc.create(CreateCheckpoint {
            parent_checkpoint_id: Some("nonexistent".into()),
            ..base_params(&sid)
        });
        assert!(result.is_err());
    }

    #[test]
    fn list_by_session() {
        let db = db();
        let sid = seed_session(&db);
        let svc = CheckpointService::new(&db);

        svc.create(CreateCheckpoint {
            label: Some("a".into()),
            ..base_params(&sid)
        })
        .unwrap();
        svc.create(CreateCheckpoint {
            label: Some("b".into()),
            ..base_params(&sid)
        })
        .unwrap();

        let list = svc.list_by_session(&sid).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].label.as_deref(), Some("a"));
        assert_eq!(list[1].label.as_deref(), Some("b"));
    }

    #[test]
    fn list_children_of_parent() {
        let db = db();
        let sid = seed_session(&db);
        let svc = CheckpointService::new(&db);

        let root = svc.create(base_params(&sid)).unwrap();
        svc.create(CreateCheckpoint {
            parent_checkpoint_id: Some(root.id.clone()),
            label: Some("fork-1".into()),
            ..base_params(&sid)
        })
        .unwrap();
        svc.create(CreateCheckpoint {
            parent_checkpoint_id: Some(root.id.clone()),
            label: Some("fork-2".into()),
            ..base_params(&sid)
        })
        .unwrap();

        let children = svc.list_children(&root.id).unwrap();
        assert_eq!(children.len(), 2);
    }

    #[test]
    fn find_by_rivet_id() {
        let db = db();
        let sid = seed_session(&db);
        let svc = CheckpointService::new(&db);

        let cp = svc
            .create(CreateCheckpoint {
                rivet_checkpoint_id: Some("rivet_abc".into()),
                ..base_params(&sid)
            })
            .unwrap();

        let found = svc.find_by_rivet_id("rivet_abc").unwrap().unwrap();
        assert_eq!(found.id, cp.id);
        assert!(svc.find_by_rivet_id("nonexistent").unwrap().is_none());
    }

    #[test]
    fn update_label() {
        let db = db();
        let sid = seed_session(&db);
        let svc = CheckpointService::new(&db);

        let cp = svc.create(base_params(&sid)).unwrap();
        let updated = svc.update_label(&cp.id, Some("renamed")).unwrap();
        assert_eq!(updated.label.as_deref(), Some("renamed"));

        let cleared = svc.update_label(&cp.id, None).unwrap();
        assert!(cleared.label.is_none());
    }

    #[test]
    fn update_metadata() {
        let db = db();
        let sid = seed_session(&db);
        let svc = CheckpointService::new(&db);

        let cp = svc.create(base_params(&sid)).unwrap();
        let updated = svc
            .update_metadata(&cp.id, json!({"turn": 5, "cost_usd": 0.12}))
            .unwrap();
        assert_eq!(updated.metadata, json!({"turn": 5, "cost_usd": 0.12}));
    }

    #[test]
    fn link_rivet_checkpoint() {
        let db = db();
        let sid = seed_session(&db);
        let svc = CheckpointService::new(&db);

        let cp = svc.create(base_params(&sid)).unwrap();
        assert!(cp.rivet_checkpoint_id.is_none());

        let linked = svc.link_rivet_checkpoint(&cp.id, "rivet_xyz").unwrap();
        assert_eq!(linked.rivet_checkpoint_id.as_deref(), Some("rivet_xyz"));
    }

    #[test]
    fn delete_checkpoint() {
        let db = db();
        let sid = seed_session(&db);
        let svc = CheckpointService::new(&db);

        let cp = svc.create(base_params(&sid)).unwrap();
        assert!(svc.delete(&cp.id).unwrap());
        assert!(svc.get(&cp.id).unwrap().is_none());
        assert!(!svc.delete(&cp.id).unwrap());
    }

    #[test]
    fn delete_blocked_by_children() {
        let db = db();
        let sid = seed_session(&db);
        let svc = CheckpointService::new(&db);

        let root = svc.create(base_params(&sid)).unwrap();
        svc.create(CreateCheckpoint {
            parent_checkpoint_id: Some(root.id.clone()),
            ..base_params(&sid)
        })
        .unwrap();

        let err = svc.delete(&root.id).unwrap_err();
        assert!(err.to_string().contains("dependent fork"));
    }

    #[test]
    fn count_by_session() {
        let db = db();
        let sid = seed_session(&db);
        let svc = CheckpointService::new(&db);

        assert_eq!(svc.count_by_session(&sid).unwrap(), 0);
        svc.create(base_params(&sid)).unwrap();
        svc.create(base_params(&sid)).unwrap();
        assert_eq!(svc.count_by_session(&sid).unwrap(), 2);
    }

    #[test]
    fn metadata_defaults_to_empty_object() {
        let db = db();
        let sid = seed_session(&db);
        let svc = CheckpointService::new(&db);

        let cp = svc.create(base_params(&sid)).unwrap();
        assert_eq!(cp.metadata, json!({}));
    }
}
