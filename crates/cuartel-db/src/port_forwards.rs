//! Port forward persistence (spec task 5e).
//!
//! Stores opt-in port forwarding rules per session. Each rule records the
//! direction (sandbox→host or host→sandbox), port pair, and enabled flag.
//! The actual forwarding activation happens through the rivet client — this
//! module only owns the persistence layer.

use anyhow::Result;
use rusqlite::{params, Connection};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortForwardRow {
    pub id: String,
    pub session_id: String,
    pub direction: String,
    pub sandbox_port: u16,
    pub host_port: u16,
    pub enabled: bool,
    pub created_at: String,
}

pub fn list_for_session(conn: &Connection, session_id: &str) -> Result<Vec<PortForwardRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, session_id, direction, sandbox_port, host_port, enabled, created_at
         FROM port_forwards
         WHERE session_id = ?1
         ORDER BY created_at ASC",
    )?;
    let rows = stmt
        .query_map(params![session_id], |row| {
            Ok(PortForwardRow {
                id: row.get(0)?,
                session_id: row.get(1)?,
                direction: row.get(2)?,
                sandbox_port: row.get::<_, u32>(3)? as u16,
                host_port: row.get::<_, u32>(4)? as u16,
                enabled: row.get::<_, i32>(5)? != 0,
                created_at: row.get(6)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn insert(
    conn: &Connection,
    id: &str,
    session_id: &str,
    direction: &str,
    sandbox_port: u16,
    host_port: u16,
) -> Result<()> {
    conn.execute(
        "INSERT INTO port_forwards (id, session_id, direction, sandbox_port, host_port)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, session_id, direction, sandbox_port as u32, host_port as u32],
    )?;
    Ok(())
}

pub fn set_enabled(conn: &Connection, id: &str, enabled: bool) -> Result<()> {
    conn.execute(
        "UPDATE port_forwards SET enabled = ?2 WHERE id = ?1",
        params![id, enabled as i32],
    )?;
    Ok(())
}

pub fn delete(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM port_forwards WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn delete_for_session(conn: &Connection, session_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM port_forwards WHERE session_id = ?1",
        params![session_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;

    fn test_db() -> Database {
        Database::open_in_memory().unwrap()
    }

    fn seed_session(conn: &Connection) {
        conn.execute_batch(
            "INSERT INTO workspaces (id, name, path) VALUES ('ws1', 'test', '/tmp/ws');
             INSERT INTO servers (id, name, address, is_local) VALUES ('local', 'local', 'localhost', 1);
             INSERT INTO sessions (id, workspace_id, server_id, agent_type) VALUES ('sess1', 'ws1', 'local', 'pi');",
        ).unwrap();
    }

    #[test]
    fn insert_and_list() {
        let db = test_db();
        seed_session(db.conn());
        insert(db.conn(), "pf1", "sess1", "host_to_sandbox", 3000, 8080).unwrap();
        insert(db.conn(), "pf2", "sess1", "sandbox_to_host", 5432, 5432).unwrap();
        let rows = list_for_session(db.conn(), "sess1").unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "pf1");
        assert_eq!(rows[0].direction, "host_to_sandbox");
        assert_eq!(rows[0].sandbox_port, 3000);
        assert_eq!(rows[0].host_port, 8080);
        assert!(rows[0].enabled);
        assert_eq!(rows[1].id, "pf2");
    }

    #[test]
    fn toggle_enabled() {
        let db = test_db();
        seed_session(db.conn());
        insert(db.conn(), "pf1", "sess1", "host_to_sandbox", 3000, 8080).unwrap();
        set_enabled(db.conn(), "pf1", false).unwrap();
        let rows = list_for_session(db.conn(), "sess1").unwrap();
        assert!(!rows[0].enabled);
        set_enabled(db.conn(), "pf1", true).unwrap();
        let rows = list_for_session(db.conn(), "sess1").unwrap();
        assert!(rows[0].enabled);
    }

    #[test]
    fn delete_single() {
        let db = test_db();
        seed_session(db.conn());
        insert(db.conn(), "pf1", "sess1", "host_to_sandbox", 3000, 8080).unwrap();
        insert(db.conn(), "pf2", "sess1", "sandbox_to_host", 5432, 5432).unwrap();
        delete(db.conn(), "pf1").unwrap();
        let rows = list_for_session(db.conn(), "sess1").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "pf2");
    }

    #[test]
    fn delete_for_session_removes_all() {
        let db = test_db();
        seed_session(db.conn());
        insert(db.conn(), "pf1", "sess1", "host_to_sandbox", 3000, 8080).unwrap();
        insert(db.conn(), "pf2", "sess1", "sandbox_to_host", 5432, 5432).unwrap();
        delete_for_session(db.conn(), "sess1").unwrap();
        let rows = list_for_session(db.conn(), "sess1").unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn empty_session_returns_empty() {
        let db = test_db();
        seed_session(db.conn());
        let rows = list_for_session(db.conn(), "sess1").unwrap();
        assert!(rows.is_empty());
    }
}
