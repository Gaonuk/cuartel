use anyhow::{anyhow, Result};
use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use crate::Database;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRow {
    pub id: String,
    pub workspace_id: String,
    pub server_id: String,
    pub agent_type: String,
    pub rivet_session_id: Option<String>,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

impl SessionRow {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            workspace_id: row.get("workspace_id")?,
            server_id: row.get("server_id")?,
            agent_type: row.get("agent_type")?,
            rivet_session_id: row.get("rivet_session_id")?,
            status: row.get("status")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

pub struct SessionRepo<'a> {
    db: &'a Database,
}

impl<'a> SessionRepo<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub fn insert(
        &self,
        id: &str,
        workspace_id: &str,
        server_id: &str,
        agent_type: &str,
        rivet_session_id: Option<&str>,
        status: &str,
    ) -> Result<SessionRow> {
        self.db.conn().execute(
            "INSERT INTO sessions (id, workspace_id, server_id, agent_type, rivet_session_id, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, workspace_id, server_id, agent_type, rivet_session_id, status],
        )?;
        self.get(id)?
            .ok_or_else(|| anyhow!("session {id} missing after insert"))
    }

    pub fn get(&self, id: &str) -> Result<Option<SessionRow>> {
        let row = self
            .db
            .conn()
            .query_row(
                "SELECT id, workspace_id, server_id, agent_type, rivet_session_id, status, created_at, updated_at
                 FROM sessions WHERE id = ?1",
                params![id],
                SessionRow::from_row,
            )
            .optional()?;
        Ok(row)
    }

    pub fn list_by_workspace(&self, workspace_id: &str) -> Result<Vec<SessionRow>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, workspace_id, server_id, agent_type, rivet_session_id, status, created_at, updated_at
             FROM sessions WHERE workspace_id = ?1 ORDER BY created_at ASC",
        )?;
        let rows = stmt
            .query_map(params![workspace_id], SessionRow::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn list_by_server(&self, server_id: &str) -> Result<Vec<SessionRow>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, workspace_id, server_id, agent_type, rivet_session_id, status, created_at, updated_at
             FROM sessions WHERE server_id = ?1 ORDER BY created_at ASC",
        )?;
        let rows = stmt
            .query_map(params![server_id], SessionRow::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn update_status(&self, id: &str, status: &str) -> Result<SessionRow> {
        let changed = self.db.conn().execute(
            "UPDATE sessions SET status = ?2, updated_at = datetime('now') WHERE id = ?1",
            params![id, status],
        )?;
        if changed == 0 {
            return Err(anyhow!("session {id} not found"));
        }
        self.get(id)?
            .ok_or_else(|| anyhow!("session {id} missing after update"))
    }

    pub fn update_server(&self, id: &str, server_id: &str) -> Result<SessionRow> {
        let changed = self.db.conn().execute(
            "UPDATE sessions SET server_id = ?2, updated_at = datetime('now') WHERE id = ?1",
            params![id, server_id],
        )?;
        if changed == 0 {
            return Err(anyhow!("session {id} not found"));
        }
        self.get(id)?
            .ok_or_else(|| anyhow!("session {id} missing after update"))
    }

    pub fn set_rivet_session_id(
        &self,
        id: &str,
        rivet_session_id: &str,
    ) -> Result<SessionRow> {
        let changed = self.db.conn().execute(
            "UPDATE sessions SET rivet_session_id = ?2, updated_at = datetime('now') WHERE id = ?1",
            params![id, rivet_session_id],
        )?;
        if changed == 0 {
            return Err(anyhow!("session {id} not found"));
        }
        self.get(id)?
            .ok_or_else(|| anyhow!("session {id} missing after update"))
    }

    pub fn delete(&self, id: &str) -> Result<bool> {
        let changed = self
            .db
            .conn()
            .execute("DELETE FROM sessions WHERE id = ?1", params![id])?;
        Ok(changed > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::servers::ServerRepo;
    use crate::workspaces::WorkspaceRepo;

    fn db() -> Database {
        Database::open_in_memory().unwrap()
    }

    fn seed(db: &Database) {
        WorkspaceRepo::new(db)
            .insert("ws1", "test", "/tmp/test")
            .unwrap();
        ServerRepo::new(db)
            .ensure_local("http://localhost:6420")
            .unwrap();
    }

    #[test]
    fn insert_and_get_round_trip() {
        let db = db();
        seed(&db);
        let repo = SessionRepo::new(&db);

        let s = repo
            .insert("s1", "ws1", "local", "pi", Some("rivet_s1"), "created")
            .unwrap();
        assert_eq!(s.id, "s1");
        assert_eq!(s.workspace_id, "ws1");
        assert_eq!(s.server_id, "local");
        assert_eq!(s.agent_type, "pi");
        assert_eq!(s.rivet_session_id.as_deref(), Some("rivet_s1"));
        assert_eq!(s.status, "created");

        let got = repo.get("s1").unwrap().unwrap();
        assert_eq!(got, s);
    }

    #[test]
    fn get_missing_returns_none() {
        let db = db();
        let repo = SessionRepo::new(&db);
        assert!(repo.get("nonexistent").unwrap().is_none());
    }

    #[test]
    fn insert_without_rivet_session_id() {
        let db = db();
        seed(&db);
        let repo = SessionRepo::new(&db);

        let s = repo.insert("s1", "ws1", "local", "pi", None, "created").unwrap();
        assert!(s.rivet_session_id.is_none());
    }

    #[test]
    fn list_by_workspace() {
        let db = db();
        seed(&db);
        let repo = SessionRepo::new(&db);

        repo.insert("s1", "ws1", "local", "pi", None, "created").unwrap();
        repo.insert("s2", "ws1", "local", "pi", None, "running").unwrap();

        WorkspaceRepo::new(&db)
            .insert("ws2", "other", "/tmp/other")
            .unwrap();
        repo.insert("s3", "ws2", "local", "pi", None, "created").unwrap();

        let ws1 = repo.list_by_workspace("ws1").unwrap();
        assert_eq!(ws1.len(), 2);
        assert_eq!(ws1[0].id, "s1");
        assert_eq!(ws1[1].id, "s2");

        let ws2 = repo.list_by_workspace("ws2").unwrap();
        assert_eq!(ws2.len(), 1);
    }

    #[test]
    fn list_by_server() {
        let db = db();
        seed(&db);
        ServerRepo::new(&db)
            .insert("remote1", "Remote", "http://remote:6420", None, false)
            .unwrap();
        let repo = SessionRepo::new(&db);

        repo.insert("s1", "ws1", "local", "pi", None, "created").unwrap();
        repo.insert("s2", "ws1", "remote1", "pi", None, "created").unwrap();

        let local = repo.list_by_server("local").unwrap();
        assert_eq!(local.len(), 1);
        assert_eq!(local[0].id, "s1");

        let remote = repo.list_by_server("remote1").unwrap();
        assert_eq!(remote.len(), 1);
        assert_eq!(remote[0].id, "s2");
    }

    #[test]
    fn update_status() {
        let db = db();
        seed(&db);
        let repo = SessionRepo::new(&db);

        repo.insert("s1", "ws1", "local", "pi", None, "created").unwrap();
        let updated = repo.update_status("s1", "running").unwrap();
        assert_eq!(updated.status, "running");
    }

    #[test]
    fn update_status_missing_errors() {
        let db = db();
        let repo = SessionRepo::new(&db);
        assert!(repo.update_status("nope", "running").is_err());
    }

    #[test]
    fn update_server() {
        let db = db();
        seed(&db);
        ServerRepo::new(&db)
            .insert("remote1", "Remote", "http://remote:6420", None, false)
            .unwrap();
        let repo = SessionRepo::new(&db);

        repo.insert("s1", "ws1", "local", "pi", None, "created").unwrap();
        let updated = repo.update_server("s1", "remote1").unwrap();
        assert_eq!(updated.server_id, "remote1");
    }

    #[test]
    fn update_server_missing_errors() {
        let db = db();
        let repo = SessionRepo::new(&db);
        assert!(repo.update_server("nope", "local").is_err());
    }

    #[test]
    fn set_rivet_session_id() {
        let db = db();
        seed(&db);
        let repo = SessionRepo::new(&db);

        repo.insert("s1", "ws1", "local", "pi", None, "created").unwrap();
        let updated = repo.set_rivet_session_id("s1", "rivet_abc").unwrap();
        assert_eq!(updated.rivet_session_id.as_deref(), Some("rivet_abc"));
    }

    #[test]
    fn delete_removes_row() {
        let db = db();
        seed(&db);
        let repo = SessionRepo::new(&db);

        repo.insert("s1", "ws1", "local", "pi", None, "created").unwrap();
        assert!(repo.delete("s1").unwrap());
        assert!(repo.get("s1").unwrap().is_none());
        assert!(!repo.delete("s1").unwrap());
    }

    #[test]
    fn duplicate_id_rejected() {
        let db = db();
        seed(&db);
        let repo = SessionRepo::new(&db);

        repo.insert("s1", "ws1", "local", "pi", None, "created").unwrap();
        assert!(repo
            .insert("s1", "ws1", "local", "pi", None, "created")
            .is_err());
    }

    #[test]
    fn foreign_key_workspace_enforced() {
        let db = db();
        seed(&db);
        let repo = SessionRepo::new(&db);
        assert!(repo
            .insert("s1", "bad_ws", "local", "pi", None, "created")
            .is_err());
    }

    #[test]
    fn foreign_key_server_enforced() {
        let db = db();
        seed(&db);
        let repo = SessionRepo::new(&db);
        assert!(repo
            .insert("s1", "ws1", "bad_server", "pi", None, "created")
            .is_err());
    }
}
