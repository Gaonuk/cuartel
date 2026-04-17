//! Audit-event persistence for the auth gateway (phase 5d).
//!
//! Stores one row per gateway outcome (`injected`, `blocked`,
//! `credential_missing`, `upstream_error`). The schema keeps the per-variant
//! fields as nullable columns rather than normalising into sub-tables because
//! reads are almost always "give me the last N rows, newest first" — a single
//! indexed table makes that a single `SELECT` without joins.
//!
//! This module speaks in flat column values; the `AuditEvent` enum from
//! `cuartel-core` is translated at the caller (the persister in
//! `cuartel-core::auth_gateway::persister`). Keeping the translation out of
//! here means `cuartel-db` does not need to depend on `cuartel-core`.

use anyhow::{anyhow, Result};
use rusqlite::{params, Row};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Database;

/// Flat representation of a single audit row. Mirrors the column layout of
/// `audit_events` 1:1 so callers can build inputs without caring about the
/// `AuditEvent` enum shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEventInput<'a> {
    pub kind: &'a str,
    /// RFC3339 timestamp of when the gateway emitted the event.
    pub timestamp: &'a str,
    pub hostname: &'a str,
    pub provider_id: Option<&'a str>,
    pub env_key: Option<&'a str>,
    pub method: Option<&'a str>,
    pub path: Option<&'a str>,
    pub status: Option<u16>,
    pub client_ip: Option<&'a str>,
    pub reason: Option<&'a str>,
    pub error: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEventRow {
    pub id: String,
    pub kind: String,
    pub timestamp: String,
    pub hostname: String,
    pub provider_id: Option<String>,
    pub env_key: Option<String>,
    pub method: Option<String>,
    pub path: Option<String>,
    pub status: Option<u16>,
    pub client_ip: Option<String>,
    pub reason: Option<String>,
    pub error: Option<String>,
    pub created_at: String,
}

impl AuditEventRow {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        let status: Option<i64> = row.get("status")?;
        Ok(Self {
            id: row.get("id")?,
            kind: row.get("kind")?,
            timestamp: row.get("timestamp")?,
            hostname: row.get("hostname")?,
            provider_id: row.get("provider_id")?,
            env_key: row.get("env_key")?,
            method: row.get("method")?,
            path: row.get("path")?,
            status: status.map(|s| s as u16),
            client_ip: row.get("client_ip")?,
            reason: row.get("reason")?,
            error: row.get("error")?,
            created_at: row.get("created_at")?,
        })
    }
}

pub struct AuditRepo<'a> {
    db: &'a Database,
}

impl<'a> AuditRepo<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub fn insert(&self, input: &AuditEventInput<'_>) -> Result<AuditEventRow> {
        let id = Uuid::new_v4().to_string();
        let status = input.status.map(|s| s as i64);
        self.db.conn().execute(
            "INSERT INTO audit_events
                (id, kind, timestamp, hostname, provider_id, env_key,
                 method, path, status, client_ip, reason, error)
             VALUES
                (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                id,
                input.kind,
                input.timestamp,
                input.hostname,
                input.provider_id,
                input.env_key,
                input.method,
                input.path,
                status,
                input.client_ip,
                input.reason,
                input.error,
            ],
        )?;
        self.get(&id)?
            .ok_or_else(|| anyhow!("audit event {id} missing after insert"))
    }

    pub fn get(&self, id: &str) -> Result<Option<AuditEventRow>> {
        let row = self
            .db
            .conn()
            .query_row(
                "SELECT id, kind, timestamp, hostname, provider_id, env_key,
                        method, path, status, client_ip, reason, error, created_at
                 FROM audit_events WHERE id = ?1",
                params![id],
                AuditEventRow::from_row,
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        Ok(row)
    }

    /// Newest-first page of events, capped at `limit`. A UI that wants to
    /// paginate should filter by `timestamp < cursor` in the caller —
    /// keeping an offset-based API off the table avoids the full-scan tail
    /// as the table grows.
    pub fn list_recent(&self, limit: usize) -> Result<Vec<AuditEventRow>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, kind, timestamp, hostname, provider_id, env_key,
                    method, path, status, client_ip, reason, error, created_at
             FROM audit_events
             ORDER BY timestamp DESC, created_at DESC
             LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit as i64], AuditEventRow::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn list_by_kind(&self, kind: &str, limit: usize) -> Result<Vec<AuditEventRow>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, kind, timestamp, hostname, provider_id, env_key,
                    method, path, status, client_ip, reason, error, created_at
             FROM audit_events
             WHERE kind = ?1
             ORDER BY timestamp DESC, created_at DESC
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![kind, limit as i64], AuditEventRow::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn count(&self) -> Result<u64> {
        let n: i64 = self
            .db
            .conn()
            .query_row("SELECT COUNT(*) FROM audit_events", [], |r| r.get(0))?;
        Ok(n as u64)
    }

    /// Delete every row with a timestamp strictly older than `cutoff` (RFC3339
    /// string comparison — our timestamps are always the same width so
    /// lexicographic compares sort chronologically). Returns the number of
    /// rows removed. Intended for a future retention job; not wired into the
    /// app lifecycle yet.
    pub fn purge_before(&self, cutoff: &str) -> Result<usize> {
        let n = self.db.conn().execute(
            "DELETE FROM audit_events WHERE timestamp < ?1",
            params![cutoff],
        )?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Database {
        Database::open_in_memory().unwrap()
    }

    fn injected(ts: &'static str, host: &'static str, status: u16) -> AuditEventInput<'static> {
        AuditEventInput {
            kind: "injected",
            timestamp: ts,
            hostname: host,
            provider_id: Some("anthropic"),
            env_key: Some("ANTHROPIC_API_KEY"),
            method: Some("POST"),
            path: Some("/v1/messages"),
            status: Some(status),
            client_ip: Some("127.0.0.1"),
            reason: None,
            error: None,
        }
    }

    #[test]
    fn insert_round_trips_injected() {
        let db = db();
        let repo = AuditRepo::new(&db);
        let row = repo
            .insert(&injected("2026-04-16T12:00:00Z", "api.anthropic.com", 200))
            .unwrap();
        assert_eq!(row.kind, "injected");
        assert_eq!(row.hostname, "api.anthropic.com");
        assert_eq!(row.provider_id.as_deref(), Some("anthropic"));
        assert_eq!(row.env_key.as_deref(), Some("ANTHROPIC_API_KEY"));
        assert_eq!(row.method.as_deref(), Some("POST"));
        assert_eq!(row.path.as_deref(), Some("/v1/messages"));
        assert_eq!(row.status, Some(200));
        assert_eq!(row.client_ip.as_deref(), Some("127.0.0.1"));
        assert!(row.reason.is_none());
        assert!(row.error.is_none());
        assert!(!row.created_at.is_empty());
    }

    #[test]
    fn insert_handles_blocked_with_nullable_provider() {
        let db = db();
        let repo = AuditRepo::new(&db);
        let row = repo
            .insert(&AuditEventInput {
                kind: "blocked",
                timestamp: "2026-04-16T12:00:00Z",
                hostname: "evil.example.com",
                provider_id: None,
                env_key: None,
                method: Some("GET"),
                path: Some("/"),
                status: None,
                client_ip: Some("127.0.0.1"),
                reason: Some("no rule for host"),
                error: None,
            })
            .unwrap();
        assert_eq!(row.kind, "blocked");
        assert!(row.provider_id.is_none());
        assert!(row.env_key.is_none());
        assert!(row.status.is_none());
        assert_eq!(row.reason.as_deref(), Some("no rule for host"));
    }

    #[test]
    fn list_recent_orders_newest_first_and_caps() {
        let db = db();
        let repo = AuditRepo::new(&db);
        repo.insert(&injected("2026-04-16T12:00:00Z", "a", 200))
            .unwrap();
        repo.insert(&injected("2026-04-16T12:00:02Z", "b", 200))
            .unwrap();
        repo.insert(&injected("2026-04-16T12:00:01Z", "c", 200))
            .unwrap();

        let rows = repo.list_recent(10).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].hostname, "b");
        assert_eq!(rows[1].hostname, "c");
        assert_eq!(rows[2].hostname, "a");

        let limited = repo.list_recent(2).unwrap();
        assert_eq!(limited.len(), 2);
        assert_eq!(limited[0].hostname, "b");
        assert_eq!(limited[1].hostname, "c");
    }

    #[test]
    fn list_by_kind_filters() {
        let db = db();
        let repo = AuditRepo::new(&db);
        repo.insert(&injected("2026-04-16T12:00:00Z", "api.anthropic.com", 200))
            .unwrap();
        repo.insert(&AuditEventInput {
            kind: "blocked",
            timestamp: "2026-04-16T12:00:01Z",
            hostname: "evil.example.com",
            provider_id: None,
            env_key: None,
            method: Some("GET"),
            path: Some("/"),
            status: None,
            client_ip: None,
            reason: Some("no rule"),
            error: None,
        })
        .unwrap();
        repo.insert(&AuditEventInput {
            kind: "credential_missing",
            timestamp: "2026-04-16T12:00:02Z",
            hostname: "api.anthropic.com",
            provider_id: Some("anthropic"),
            env_key: Some("ANTHROPIC_API_KEY"),
            method: None,
            path: None,
            status: None,
            client_ip: None,
            reason: None,
            error: None,
        })
        .unwrap();

        let blocked = repo.list_by_kind("blocked", 10).unwrap();
        assert_eq!(blocked.len(), 1);
        assert_eq!(blocked[0].hostname, "evil.example.com");

        let injected_only = repo.list_by_kind("injected", 10).unwrap();
        assert_eq!(injected_only.len(), 1);
        assert_eq!(injected_only[0].hostname, "api.anthropic.com");
    }

    #[test]
    fn count_reports_rows() {
        let db = db();
        let repo = AuditRepo::new(&db);
        assert_eq!(repo.count().unwrap(), 0);
        repo.insert(&injected("2026-04-16T12:00:00Z", "a", 200))
            .unwrap();
        repo.insert(&injected("2026-04-16T12:00:01Z", "b", 200))
            .unwrap();
        assert_eq!(repo.count().unwrap(), 2);
    }

    #[test]
    fn purge_before_removes_older_rows() {
        let db = db();
        let repo = AuditRepo::new(&db);
        repo.insert(&injected("2026-04-16T10:00:00Z", "old", 200))
            .unwrap();
        repo.insert(&injected("2026-04-16T12:00:00Z", "mid", 200))
            .unwrap();
        repo.insert(&injected("2026-04-16T14:00:00Z", "new", 200))
            .unwrap();

        let removed = repo.purge_before("2026-04-16T12:00:00Z").unwrap();
        assert_eq!(removed, 1);
        let remaining = repo.list_recent(10).unwrap();
        assert_eq!(remaining.len(), 2);
        assert!(remaining.iter().all(|r| r.hostname != "old"));
    }

    #[test]
    fn get_returns_none_for_unknown_id() {
        let db = db();
        let repo = AuditRepo::new(&db);
        assert!(repo.get("no-such-id").unwrap().is_none());
    }
}
