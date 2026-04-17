use anyhow::{anyhow, Result};
use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use crate::Database;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointRow {
    pub id: String,
    pub session_id: String,
    pub rivet_checkpoint_id: Option<String>,
    pub parent_checkpoint_id: Option<String>,
    pub label: Option<String>,
    pub metadata: String,
    pub created_at: String,
    pub updated_at: String,
}

impl CheckpointRow {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            session_id: row.get("session_id")?,
            rivet_checkpoint_id: row.get("rivet_checkpoint_id")?,
            parent_checkpoint_id: row.get("parent_checkpoint_id")?,
            label: row.get("label")?,
            metadata: row.get("metadata")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

pub struct CheckpointRepo<'a> {
    db: &'a Database,
}

impl<'a> CheckpointRepo<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub fn insert(
        &self,
        id: &str,
        session_id: &str,
        rivet_checkpoint_id: Option<&str>,
        parent_checkpoint_id: Option<&str>,
        label: Option<&str>,
        metadata: &str,
    ) -> Result<CheckpointRow> {
        self.db.conn().execute(
            "INSERT INTO checkpoints (id, session_id, rivet_checkpoint_id, parent_checkpoint_id, label, metadata)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, session_id, rivet_checkpoint_id, parent_checkpoint_id, label, metadata],
        )?;
        self.get(id)?
            .ok_or_else(|| anyhow!("checkpoint {id} missing after insert"))
    }

    pub fn get(&self, id: &str) -> Result<Option<CheckpointRow>> {
        let row = self
            .db
            .conn()
            .query_row(
                "SELECT id, session_id, rivet_checkpoint_id, parent_checkpoint_id, label, metadata, created_at, updated_at
                 FROM checkpoints WHERE id = ?1",
                params![id],
                CheckpointRow::from_row,
            )
            .optional()?;
        Ok(row)
    }

    pub fn find_by_rivet_id(&self, rivet_checkpoint_id: &str) -> Result<Option<CheckpointRow>> {
        let row = self
            .db
            .conn()
            .query_row(
                "SELECT id, session_id, rivet_checkpoint_id, parent_checkpoint_id, label, metadata, created_at, updated_at
                 FROM checkpoints WHERE rivet_checkpoint_id = ?1",
                params![rivet_checkpoint_id],
                CheckpointRow::from_row,
            )
            .optional()?;
        Ok(row)
    }

    pub fn list_by_session(&self, session_id: &str) -> Result<Vec<CheckpointRow>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, session_id, rivet_checkpoint_id, parent_checkpoint_id, label, metadata, created_at, updated_at
             FROM checkpoints WHERE session_id = ?1 ORDER BY created_at ASC",
        )?;
        let rows = stmt
            .query_map(params![session_id], CheckpointRow::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn list_children(&self, parent_checkpoint_id: &str) -> Result<Vec<CheckpointRow>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, session_id, rivet_checkpoint_id, parent_checkpoint_id, label, metadata, created_at, updated_at
             FROM checkpoints WHERE parent_checkpoint_id = ?1 ORDER BY created_at ASC",
        )?;
        let rows = stmt
            .query_map(params![parent_checkpoint_id], CheckpointRow::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn update_label(&self, id: &str, label: Option<&str>) -> Result<CheckpointRow> {
        let changed = self.db.conn().execute(
            "UPDATE checkpoints SET label = ?2, updated_at = datetime('now') WHERE id = ?1",
            params![id, label],
        )?;
        if changed == 0 {
            return Err(anyhow!("checkpoint {id} not found"));
        }
        self.get(id)?
            .ok_or_else(|| anyhow!("checkpoint {id} missing after update"))
    }

    pub fn update_metadata(&self, id: &str, metadata: &str) -> Result<CheckpointRow> {
        let changed = self.db.conn().execute(
            "UPDATE checkpoints SET metadata = ?2, updated_at = datetime('now') WHERE id = ?1",
            params![id, metadata],
        )?;
        if changed == 0 {
            return Err(anyhow!("checkpoint {id} not found"));
        }
        self.get(id)?
            .ok_or_else(|| anyhow!("checkpoint {id} missing after update"))
    }

    pub fn set_rivet_checkpoint_id(
        &self,
        id: &str,
        rivet_checkpoint_id: &str,
    ) -> Result<CheckpointRow> {
        let changed = self.db.conn().execute(
            "UPDATE checkpoints SET rivet_checkpoint_id = ?2, updated_at = datetime('now') WHERE id = ?1",
            params![id, rivet_checkpoint_id],
        )?;
        if changed == 0 {
            return Err(anyhow!("checkpoint {id} not found"));
        }
        self.get(id)?
            .ok_or_else(|| anyhow!("checkpoint {id} missing after update"))
    }

    pub fn delete(&self, id: &str) -> Result<bool> {
        let changed = self
            .db
            .conn()
            .execute("DELETE FROM checkpoints WHERE id = ?1", params![id])?;
        Ok(changed > 0)
    }

    pub fn count_by_session(&self, session_id: &str) -> Result<i64> {
        let count: i64 = self.db.conn().query_row(
            "SELECT COUNT(*) FROM checkpoints WHERE session_id = ?1",
            params![session_id],
            |row| row.get(0),
        )?;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspaces::WorkspaceRepo;

    fn db() -> Database {
        Database::open_in_memory().unwrap()
    }

    fn seed_session(db: &Database) -> String {
        let ws = WorkspaceRepo::new(db);
        ws.insert("ws1", "test", "/tmp/test").unwrap();
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

    #[test]
    fn insert_and_get_round_trip() {
        let db = db();
        let session_id = seed_session(&db);
        let repo = CheckpointRepo::new(&db);

        let cp = repo
            .insert("cp1", &session_id, Some("rivet_cp1"), None, Some("initial"), "{}")
            .unwrap();
        assert_eq!(cp.id, "cp1");
        assert_eq!(cp.session_id, session_id);
        assert_eq!(cp.rivet_checkpoint_id.as_deref(), Some("rivet_cp1"));
        assert!(cp.parent_checkpoint_id.is_none());
        assert_eq!(cp.label.as_deref(), Some("initial"));
        assert_eq!(cp.metadata, "{}");

        let got = repo.get("cp1").unwrap().unwrap();
        assert_eq!(got, cp);
    }

    #[test]
    fn get_missing_returns_none() {
        let db = db();
        let repo = CheckpointRepo::new(&db);
        assert!(repo.get("nonexistent").unwrap().is_none());
    }

    #[test]
    fn insert_without_optional_fields() {
        let db = db();
        let session_id = seed_session(&db);
        let repo = CheckpointRepo::new(&db);

        let cp = repo.insert("cp1", &session_id, None, None, None, "{}").unwrap();
        assert!(cp.rivet_checkpoint_id.is_none());
        assert!(cp.parent_checkpoint_id.is_none());
        assert!(cp.label.is_none());
    }

    #[test]
    fn find_by_rivet_id() {
        let db = db();
        let session_id = seed_session(&db);
        let repo = CheckpointRepo::new(&db);

        repo.insert("cp1", &session_id, Some("rivet_abc"), None, None, "{}").unwrap();
        let found = repo.find_by_rivet_id("rivet_abc").unwrap().unwrap();
        assert_eq!(found.id, "cp1");
        assert!(repo.find_by_rivet_id("nonexistent").unwrap().is_none());
    }

    #[test]
    fn list_by_session_ordered_by_created_at() {
        let db = db();
        let session_id = seed_session(&db);
        let repo = CheckpointRepo::new(&db);

        repo.insert("cp1", &session_id, None, None, Some("first"), "{}").unwrap();
        repo.insert("cp2", &session_id, None, None, Some("second"), "{}").unwrap();
        repo.insert("cp3", &session_id, None, None, Some("third"), "{}").unwrap();

        let list = repo.list_by_session(&session_id).unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].id, "cp1");
        assert_eq!(list[1].id, "cp2");
        assert_eq!(list[2].id, "cp3");
    }

    #[test]
    fn list_by_session_filters_other_sessions() {
        let db = db();
        seed_session(&db);
        db.conn()
            .execute(
                "INSERT INTO sessions (id, workspace_id, server_id, agent_type) VALUES ('sess2', 'ws1', 'srv1', 'pi')",
                [],
            )
            .unwrap();
        let repo = CheckpointRepo::new(&db);

        repo.insert("cp1", "sess1", None, None, None, "{}").unwrap();
        repo.insert("cp2", "sess2", None, None, None, "{}").unwrap();

        assert_eq!(repo.list_by_session("sess1").unwrap().len(), 1);
        assert_eq!(repo.list_by_session("sess2").unwrap().len(), 1);
    }

    #[test]
    fn list_children() {
        let db = db();
        let session_id = seed_session(&db);
        let repo = CheckpointRepo::new(&db);

        repo.insert("cp_root", &session_id, None, None, Some("root"), "{}").unwrap();
        repo.insert("cp_child1", &session_id, None, Some("cp_root"), Some("child1"), "{}").unwrap();
        repo.insert("cp_child2", &session_id, None, Some("cp_root"), Some("child2"), "{}").unwrap();
        repo.insert("cp_other", &session_id, None, None, Some("orphan"), "{}").unwrap();

        let children = repo.list_children("cp_root").unwrap();
        assert_eq!(children.len(), 2);
        assert_eq!(children[0].id, "cp_child1");
        assert_eq!(children[1].id, "cp_child2");
    }

    #[test]
    fn update_label() {
        let db = db();
        let session_id = seed_session(&db);
        let repo = CheckpointRepo::new(&db);

        repo.insert("cp1", &session_id, None, None, Some("old"), "{}").unwrap();
        let updated = repo.update_label("cp1", Some("new")).unwrap();
        assert_eq!(updated.label.as_deref(), Some("new"));

        let cleared = repo.update_label("cp1", None).unwrap();
        assert!(cleared.label.is_none());
    }

    #[test]
    fn update_label_missing_errors() {
        let db = db();
        let repo = CheckpointRepo::new(&db);
        assert!(repo.update_label("nope", Some("x")).is_err());
    }

    #[test]
    fn update_metadata() {
        let db = db();
        let session_id = seed_session(&db);
        let repo = CheckpointRepo::new(&db);

        repo.insert("cp1", &session_id, None, None, None, "{}").unwrap();
        let updated = repo
            .update_metadata("cp1", r#"{"tokens":500}"#)
            .unwrap();
        assert_eq!(updated.metadata, r#"{"tokens":500}"#);
    }

    #[test]
    fn set_rivet_checkpoint_id() {
        let db = db();
        let session_id = seed_session(&db);
        let repo = CheckpointRepo::new(&db);

        repo.insert("cp1", &session_id, None, None, None, "{}").unwrap();
        let updated = repo.set_rivet_checkpoint_id("cp1", "rivet_xyz").unwrap();
        assert_eq!(updated.rivet_checkpoint_id.as_deref(), Some("rivet_xyz"));
    }

    #[test]
    fn delete_removes_row() {
        let db = db();
        let session_id = seed_session(&db);
        let repo = CheckpointRepo::new(&db);

        repo.insert("cp1", &session_id, None, None, None, "{}").unwrap();
        assert!(repo.delete("cp1").unwrap());
        assert!(repo.get("cp1").unwrap().is_none());
        assert!(!repo.delete("cp1").unwrap());
    }

    #[test]
    fn count_by_session() {
        let db = db();
        let session_id = seed_session(&db);
        let repo = CheckpointRepo::new(&db);

        assert_eq!(repo.count_by_session(&session_id).unwrap(), 0);
        repo.insert("cp1", &session_id, None, None, None, "{}").unwrap();
        repo.insert("cp2", &session_id, None, None, None, "{}").unwrap();
        assert_eq!(repo.count_by_session(&session_id).unwrap(), 2);
    }

    #[test]
    fn duplicate_id_rejected() {
        let db = db();
        let session_id = seed_session(&db);
        let repo = CheckpointRepo::new(&db);

        repo.insert("cp1", &session_id, None, None, None, "{}").unwrap();
        assert!(repo.insert("cp1", &session_id, None, None, None, "{}").is_err());
    }

    #[test]
    fn foreign_key_session_enforced() {
        let db = db();
        let repo = CheckpointRepo::new(&db);
        assert!(repo.insert("cp1", "nonexistent_session", None, None, None, "{}").is_err());
    }

    #[test]
    fn foreign_key_parent_enforced() {
        let db = db();
        let session_id = seed_session(&db);
        let repo = CheckpointRepo::new(&db);
        assert!(repo
            .insert("cp1", &session_id, None, Some("nonexistent_parent"), None, "{}")
            .is_err());
    }
}
