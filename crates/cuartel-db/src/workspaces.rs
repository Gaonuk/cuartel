use anyhow::{anyhow, Result};
use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use crate::Database;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRow {
    pub id: String,
    pub name: String,
    pub path: String,
    pub created_at: String,
    pub updated_at: String,
}

impl WorkspaceRow {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            name: row.get("name")?,
            path: row.get("path")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

pub struct WorkspaceRepo<'a> {
    db: &'a Database,
}

impl<'a> WorkspaceRepo<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub fn insert(&self, id: &str, name: &str, path: &str) -> Result<WorkspaceRow> {
        self.db.conn().execute(
            "INSERT INTO workspaces (id, name, path) VALUES (?1, ?2, ?3)",
            params![id, name, path],
        )?;
        self.get(id)?
            .ok_or_else(|| anyhow!("workspace {id} missing after insert"))
    }

    pub fn get(&self, id: &str) -> Result<Option<WorkspaceRow>> {
        let row = self
            .db
            .conn()
            .query_row(
                "SELECT id, name, path, created_at, updated_at FROM workspaces WHERE id = ?1",
                params![id],
                WorkspaceRow::from_row,
            )
            .optional()?;
        Ok(row)
    }

    pub fn find_by_path(&self, path: &str) -> Result<Option<WorkspaceRow>> {
        let row = self
            .db
            .conn()
            .query_row(
                "SELECT id, name, path, created_at, updated_at FROM workspaces WHERE path = ?1",
                params![path],
                WorkspaceRow::from_row,
            )
            .optional()?;
        Ok(row)
    }

    pub fn list(&self) -> Result<Vec<WorkspaceRow>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, name, path, created_at, updated_at FROM workspaces ORDER BY created_at ASC",
        )?;
        let rows = stmt
            .query_map([], WorkspaceRow::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn update(&self, id: &str, name: &str, path: &str) -> Result<WorkspaceRow> {
        let changed = self.db.conn().execute(
            "UPDATE workspaces SET name = ?2, path = ?3, updated_at = datetime('now') WHERE id = ?1",
            params![id, name, path],
        )?;
        if changed == 0 {
            return Err(anyhow!("workspace {id} not found"));
        }
        self.get(id)?
            .ok_or_else(|| anyhow!("workspace {id} missing after update"))
    }

    pub fn delete(&self, id: &str) -> Result<bool> {
        let changed = self
            .db
            .conn()
            .execute("DELETE FROM workspaces WHERE id = ?1", params![id])?;
        Ok(changed > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Database {
        Database::open_in_memory().unwrap()
    }

    #[test]
    fn insert_and_get_round_trip() {
        let db = db();
        let repo = WorkspaceRepo::new(&db);
        let inserted = repo.insert("w1", "proj", "/tmp/proj").unwrap();
        assert_eq!(inserted.id, "w1");
        let got = repo.get("w1").unwrap().unwrap();
        assert_eq!(got, inserted);
    }

    #[test]
    fn find_by_path_returns_match() {
        let db = db();
        let repo = WorkspaceRepo::new(&db);
        repo.insert("w1", "proj", "/tmp/proj").unwrap();
        let found = repo.find_by_path("/tmp/proj").unwrap().unwrap();
        assert_eq!(found.id, "w1");
        assert!(repo.find_by_path("/nope").unwrap().is_none());
    }

    #[test]
    fn list_orders_by_created_at() {
        let db = db();
        let repo = WorkspaceRepo::new(&db);
        repo.insert("w1", "a", "/a").unwrap();
        repo.insert("w2", "b", "/b").unwrap();
        let rows = repo.list().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "w1");
        assert_eq!(rows[1].id, "w2");
    }

    #[test]
    fn update_changes_fields() {
        let db = db();
        let repo = WorkspaceRepo::new(&db);
        repo.insert("w1", "proj", "/tmp/proj").unwrap();
        let updated = repo.update("w1", "renamed", "/tmp/renamed").unwrap();
        assert_eq!(updated.name, "renamed");
        assert_eq!(updated.path, "/tmp/renamed");
    }

    #[test]
    fn update_missing_errors() {
        let db = db();
        let repo = WorkspaceRepo::new(&db);
        assert!(repo.update("missing", "x", "/x").is_err());
    }

    #[test]
    fn delete_removes_row() {
        let db = db();
        let repo = WorkspaceRepo::new(&db);
        repo.insert("w1", "proj", "/tmp/proj").unwrap();
        assert!(repo.delete("w1").unwrap());
        assert!(repo.get("w1").unwrap().is_none());
        assert!(!repo.delete("w1").unwrap());
    }

    #[test]
    fn duplicate_id_rejected() {
        let db = db();
        let repo = WorkspaceRepo::new(&db);
        repo.insert("w1", "a", "/a").unwrap();
        assert!(repo.insert("w1", "b", "/b").is_err());
    }
}
