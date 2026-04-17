//! Persistence for the server registry (phase 7b).
//!
//! Backs a single logical concept — "a rivet server cuartel can talk to" —
//! local or remote. The primary "local" row represents the sidecar managed by
//! this Mac; remote rows are Tailscale peers the user has registered.

use anyhow::{anyhow, Result};
use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use crate::Database;

pub const LOCAL_SERVER_ID: &str = "local";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerRow {
    pub id: String,
    pub name: String,
    pub address: String,
    pub tailscale_ip: Option<String>,
    pub is_local: bool,
    pub created_at: String,
}

impl ServerRow {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        let is_local: i64 = row.get("is_local")?;
        Ok(Self {
            id: row.get("id")?,
            name: row.get("name")?,
            address: row.get("address")?,
            tailscale_ip: row.get("tailscale_ip")?,
            is_local: is_local != 0,
            created_at: row.get("created_at")?,
        })
    }
}

pub struct ServerRepo<'a> {
    db: &'a Database,
}

impl<'a> ServerRepo<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub fn insert(
        &self,
        id: &str,
        name: &str,
        address: &str,
        tailscale_ip: Option<&str>,
        is_local: bool,
    ) -> Result<ServerRow> {
        self.db.conn().execute(
            "INSERT INTO servers (id, name, address, tailscale_ip, is_local)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, name, address, tailscale_ip, is_local as i64],
        )?;
        self.get(id)?
            .ok_or_else(|| anyhow!("server {id} missing after insert"))
    }

    pub fn get(&self, id: &str) -> Result<Option<ServerRow>> {
        let row = self
            .db
            .conn()
            .query_row(
                "SELECT id, name, address, tailscale_ip, is_local, created_at
                 FROM servers WHERE id = ?1",
                params![id],
                ServerRow::from_row,
            )
            .optional()?;
        Ok(row)
    }

    pub fn find_by_tailscale_ip(&self, ip: &str) -> Result<Option<ServerRow>> {
        let row = self
            .db
            .conn()
            .query_row(
                "SELECT id, name, address, tailscale_ip, is_local, created_at
                 FROM servers WHERE tailscale_ip = ?1",
                params![ip],
                ServerRow::from_row,
            )
            .optional()?;
        Ok(row)
    }

    pub fn list(&self) -> Result<Vec<ServerRow>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, name, address, tailscale_ip, is_local, created_at
             FROM servers ORDER BY is_local DESC, created_at ASC",
        )?;
        let rows = stmt
            .query_map([], ServerRow::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn update(
        &self,
        id: &str,
        name: &str,
        address: &str,
        tailscale_ip: Option<&str>,
    ) -> Result<ServerRow> {
        let changed = self.db.conn().execute(
            "UPDATE servers SET name = ?2, address = ?3, tailscale_ip = ?4
             WHERE id = ?1",
            params![id, name, address, tailscale_ip],
        )?;
        if changed == 0 {
            return Err(anyhow!("server {id} not found"));
        }
        self.get(id)?
            .ok_or_else(|| anyhow!("server {id} missing after update"))
    }

    pub fn delete(&self, id: &str) -> Result<bool> {
        if id == LOCAL_SERVER_ID {
            return Err(anyhow!("cannot delete built-in local server"));
        }
        let changed = self
            .db
            .conn()
            .execute("DELETE FROM servers WHERE id = ?1", params![id])?;
        Ok(changed > 0)
    }

    /// Ensure the built-in "This Mac" row exists. Idempotent — safe to call
    /// on every app startup.
    pub fn ensure_local(&self, address: &str) -> Result<ServerRow> {
        if let Some(existing) = self.get(LOCAL_SERVER_ID)? {
            // Keep address in sync with the currently-configured sidecar port
            // in case the user changed it across restarts.
            if existing.address != address {
                return self.update(LOCAL_SERVER_ID, &existing.name, address, None);
            }
            return Ok(existing);
        }
        self.insert(LOCAL_SERVER_ID, "This Mac", address, None, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Database {
        Database::open_in_memory().unwrap()
    }

    #[test]
    fn ensure_local_is_idempotent() {
        let db = db();
        let repo = ServerRepo::new(&db);
        let first = repo.ensure_local("http://localhost:6420").unwrap();
        let second = repo.ensure_local("http://localhost:6420").unwrap();
        assert_eq!(first.id, LOCAL_SERVER_ID);
        assert_eq!(first, second);
        assert_eq!(repo.list().unwrap().len(), 1);
    }

    #[test]
    fn ensure_local_updates_address() {
        let db = db();
        let repo = ServerRepo::new(&db);
        repo.ensure_local("http://localhost:6420").unwrap();
        let updated = repo.ensure_local("http://localhost:9999").unwrap();
        assert_eq!(updated.address, "http://localhost:9999");
    }

    #[test]
    fn insert_and_list_orders_local_first() {
        let db = db();
        let repo = ServerRepo::new(&db);
        repo.insert("hetzner-1", "Hetzner", "http://100.67.106.62:6420", Some("100.67.106.62"), false)
            .unwrap();
        repo.ensure_local("http://localhost:6420").unwrap();
        let list = repo.list().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, LOCAL_SERVER_ID);
        assert_eq!(list[1].id, "hetzner-1");
    }

    #[test]
    fn get_returns_none_for_unknown() {
        let db = db();
        let repo = ServerRepo::new(&db);
        assert!(repo.get("nope").unwrap().is_none());
    }

    #[test]
    fn find_by_tailscale_ip() {
        let db = db();
        let repo = ServerRepo::new(&db);
        repo.insert("h1", "H1", "http://100.67.106.62:6420", Some("100.67.106.62"), false)
            .unwrap();
        let found = repo.find_by_tailscale_ip("100.67.106.62").unwrap().unwrap();
        assert_eq!(found.id, "h1");
        assert!(repo.find_by_tailscale_ip("100.0.0.0").unwrap().is_none());
    }

    #[test]
    fn update_changes_fields() {
        let db = db();
        let repo = ServerRepo::new(&db);
        repo.insert("h1", "H1", "http://old:6420", Some("100.0.0.1"), false)
            .unwrap();
        let updated = repo
            .update("h1", "H1 renamed", "http://new:6420", Some("100.0.0.2"))
            .unwrap();
        assert_eq!(updated.name, "H1 renamed");
        assert_eq!(updated.address, "http://new:6420");
        assert_eq!(updated.tailscale_ip.as_deref(), Some("100.0.0.2"));
    }

    #[test]
    fn update_missing_errors() {
        let db = db();
        let repo = ServerRepo::new(&db);
        assert!(repo
            .update("missing", "x", "http://x:6420", None)
            .is_err());
    }

    #[test]
    fn delete_remote_succeeds() {
        let db = db();
        let repo = ServerRepo::new(&db);
        repo.insert("h1", "H1", "http://h1:6420", None, false).unwrap();
        assert!(repo.delete("h1").unwrap());
        assert!(repo.get("h1").unwrap().is_none());
    }

    #[test]
    fn delete_local_rejected() {
        let db = db();
        let repo = ServerRepo::new(&db);
        repo.ensure_local("http://localhost:6420").unwrap();
        assert!(repo.delete(LOCAL_SERVER_ID).is_err());
        assert!(repo.get(LOCAL_SERVER_ID).unwrap().is_some());
    }

    #[test]
    fn is_local_round_trips() {
        let db = db();
        let repo = ServerRepo::new(&db);
        let local = repo.ensure_local("http://localhost:6420").unwrap();
        let remote = repo
            .insert("h1", "H1", "http://h1:6420", None, false)
            .unwrap();
        assert!(local.is_local);
        assert!(!remote.is_local);
    }

    #[test]
    fn duplicate_id_rejected() {
        let db = db();
        let repo = ServerRepo::new(&db);
        repo.insert("h1", "H1", "http://h1:6420", None, false).unwrap();
        assert!(repo.insert("h1", "H1b", "http://h1b:6420", None, false).is_err());
    }
}
