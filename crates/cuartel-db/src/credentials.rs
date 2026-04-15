//! Encrypted credential CRUD (spec task 5a, storage half).
//!
//! This module speaks only in ciphertext + nonces — it knows nothing about
//! `Vault` or the AES-256-GCM layer. The encryption wrapper lives in
//! [`crate::crypto::Vault`] and is composed on top of this repo by
//! `cuartel-core`'s `SqliteCredentialStore`. Keeping the split here means:
//!
//! * schema migration tests don't need keys
//! * the repo is trivially reusable if we ever want a different AEAD
//! * the blob/nonce columns are the only place raw bytes live
//!
//! Each row is scoped by `(provider, env_key)` — a single provider can have
//! multiple distinct credentials (`anthropic:ANTHROPIC_API_KEY` vs
//! `anthropic:ANTHROPIC_OAUTH_TOKEN`). The pair is `UNIQUE` in the schema;
//! `upsert` leans on that constraint via `ON CONFLICT`.

use anyhow::{anyhow, Result};
use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Database;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialRow {
    pub id: String,
    pub provider: String,
    pub env_key: String,
    pub encrypted_value: Vec<u8>,
    pub nonce: Vec<u8>,
    pub created_at: String,
    pub updated_at: String,
}

impl CredentialRow {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            provider: row.get("provider")?,
            env_key: row.get("env_key")?,
            encrypted_value: row.get("encrypted_value")?,
            nonce: row.get("nonce")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

pub struct CredentialRepo<'a> {
    db: &'a Database,
}

impl<'a> CredentialRepo<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Insert a new credential or replace the ciphertext/nonce of an existing
    /// row with the same `(provider, env_key)` pair. Returns the stored row.
    pub fn upsert(
        &self,
        provider: &str,
        env_key: &str,
        encrypted_value: &[u8],
        nonce: &[u8],
    ) -> Result<CredentialRow> {
        let conn = self.db.conn();
        // Generate an id up front; it's only used on the insert path. On
        // conflict the existing row's id is preserved (we touch
        // updated_at only).
        let new_id = Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO credentials (id, provider, env_key, encrypted_value, nonce)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(provider, env_key) DO UPDATE SET
                encrypted_value = excluded.encrypted_value,
                nonce           = excluded.nonce,
                updated_at      = datetime('now')",
            params![new_id, provider, env_key, encrypted_value, nonce],
        )?;
        self.get(provider, env_key)?.ok_or_else(|| {
            anyhow!("credential {provider}:{env_key} missing after upsert")
        })
    }

    pub fn get(&self, provider: &str, env_key: &str) -> Result<Option<CredentialRow>> {
        let row = self
            .db
            .conn()
            .query_row(
                "SELECT id, provider, env_key, encrypted_value, nonce, created_at, updated_at
                 FROM credentials
                 WHERE provider = ?1 AND env_key = ?2",
                params![provider, env_key],
                CredentialRow::from_row,
            )
            .optional()?;
        Ok(row)
    }

    pub fn list(&self) -> Result<Vec<CredentialRow>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, provider, env_key, encrypted_value, nonce, created_at, updated_at
             FROM credentials
             ORDER BY provider ASC, env_key ASC",
        )?;
        let rows = stmt
            .query_map([], CredentialRow::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn list_by_provider(&self, provider: &str) -> Result<Vec<CredentialRow>> {
        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT id, provider, env_key, encrypted_value, nonce, created_at, updated_at
             FROM credentials
             WHERE provider = ?1
             ORDER BY env_key ASC",
        )?;
        let rows = stmt
            .query_map(params![provider], CredentialRow::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Remove the credential for `(provider, env_key)`. Returns `true` if a
    /// row was deleted, `false` if there was nothing to delete.
    pub fn delete(&self, provider: &str, env_key: &str) -> Result<bool> {
        let changed = self.db.conn().execute(
            "DELETE FROM credentials WHERE provider = ?1 AND env_key = ?2",
            params![provider, env_key],
        )?;
        Ok(changed > 0)
    }

    /// Remove every credential for a provider. Used when the user revokes a
    /// provider entirely from settings. Returns the number of rows deleted.
    pub fn delete_provider(&self, provider: &str) -> Result<usize> {
        let changed = self
            .db
            .conn()
            .execute("DELETE FROM credentials WHERE provider = ?1", params![provider])?;
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Database {
        Database::open_in_memory().unwrap()
    }

    #[test]
    fn upsert_inserts_new_row() {
        let db = db();
        let repo = CredentialRepo::new(&db);
        let row = repo
            .upsert("anthropic", "ANTHROPIC_API_KEY", b"ciphertext", b"nonce1234567")
            .unwrap();
        assert_eq!(row.provider, "anthropic");
        assert_eq!(row.env_key, "ANTHROPIC_API_KEY");
        assert_eq!(row.encrypted_value, b"ciphertext");
        assert_eq!(row.nonce, b"nonce1234567");
    }

    #[test]
    fn upsert_replaces_existing_ciphertext() {
        let db = db();
        let repo = CredentialRepo::new(&db);
        let first = repo
            .upsert("anthropic", "ANTHROPIC_API_KEY", b"v1", b"n1")
            .unwrap();
        let second = repo
            .upsert("anthropic", "ANTHROPIC_API_KEY", b"v2", b"n2")
            .unwrap();
        // id is preserved across upserts (ON CONFLICT updates in place)
        assert_eq!(first.id, second.id);
        assert_eq!(second.encrypted_value, b"v2");
        assert_eq!(second.nonce, b"n2");
    }

    #[test]
    fn get_returns_none_for_missing() {
        let db = db();
        let repo = CredentialRepo::new(&db);
        assert!(repo.get("anthropic", "ANTHROPIC_API_KEY").unwrap().is_none());
    }

    #[test]
    fn scopes_by_provider_and_env_key() {
        let db = db();
        let repo = CredentialRepo::new(&db);
        repo.upsert("anthropic", "ANTHROPIC_API_KEY", b"a", b"na")
            .unwrap();
        repo.upsert("anthropic", "ANTHROPIC_OAUTH_TOKEN", b"b", b"nb")
            .unwrap();
        repo.upsert("openai", "OPENAI_API_KEY", b"c", b"nc").unwrap();

        assert_eq!(
            repo.get("anthropic", "ANTHROPIC_API_KEY")
                .unwrap()
                .unwrap()
                .encrypted_value,
            b"a"
        );
        assert_eq!(
            repo.get("anthropic", "ANTHROPIC_OAUTH_TOKEN")
                .unwrap()
                .unwrap()
                .encrypted_value,
            b"b"
        );
        assert_eq!(
            repo.get("openai", "OPENAI_API_KEY")
                .unwrap()
                .unwrap()
                .encrypted_value,
            b"c"
        );
    }

    #[test]
    fn list_orders_by_provider_then_env_key() {
        let db = db();
        let repo = CredentialRepo::new(&db);
        repo.upsert("openai", "OPENAI_API_KEY", b"o", b"no").unwrap();
        repo.upsert("anthropic", "ANTHROPIC_OAUTH_TOKEN", b"b", b"nb")
            .unwrap();
        repo.upsert("anthropic", "ANTHROPIC_API_KEY", b"a", b"na")
            .unwrap();
        let all = repo.list().unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(
            (all[0].provider.as_str(), all[0].env_key.as_str()),
            ("anthropic", "ANTHROPIC_API_KEY")
        );
        assert_eq!(
            (all[1].provider.as_str(), all[1].env_key.as_str()),
            ("anthropic", "ANTHROPIC_OAUTH_TOKEN")
        );
        assert_eq!(
            (all[2].provider.as_str(), all[2].env_key.as_str()),
            ("openai", "OPENAI_API_KEY")
        );
    }

    #[test]
    fn list_by_provider_filters() {
        let db = db();
        let repo = CredentialRepo::new(&db);
        repo.upsert("anthropic", "ANTHROPIC_API_KEY", b"a", b"na")
            .unwrap();
        repo.upsert("anthropic", "ANTHROPIC_OAUTH_TOKEN", b"b", b"nb")
            .unwrap();
        repo.upsert("openai", "OPENAI_API_KEY", b"c", b"nc").unwrap();
        let rows = repo.list_by_provider("anthropic").unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.provider == "anthropic"));
    }

    #[test]
    fn delete_removes_targeted_row() {
        let db = db();
        let repo = CredentialRepo::new(&db);
        repo.upsert("anthropic", "ANTHROPIC_API_KEY", b"a", b"na")
            .unwrap();
        repo.upsert("anthropic", "ANTHROPIC_OAUTH_TOKEN", b"b", b"nb")
            .unwrap();
        assert!(repo.delete("anthropic", "ANTHROPIC_API_KEY").unwrap());
        assert!(repo
            .get("anthropic", "ANTHROPIC_API_KEY")
            .unwrap()
            .is_none());
        // unrelated row survives
        assert!(repo
            .get("anthropic", "ANTHROPIC_OAUTH_TOKEN")
            .unwrap()
            .is_some());
        // delete of missing is a no-op
        assert!(!repo.delete("anthropic", "ANTHROPIC_API_KEY").unwrap());
    }

    #[test]
    fn delete_provider_removes_all_rows_for_provider() {
        let db = db();
        let repo = CredentialRepo::new(&db);
        repo.upsert("anthropic", "ANTHROPIC_API_KEY", b"a", b"na")
            .unwrap();
        repo.upsert("anthropic", "ANTHROPIC_OAUTH_TOKEN", b"b", b"nb")
            .unwrap();
        repo.upsert("openai", "OPENAI_API_KEY", b"c", b"nc").unwrap();
        assert_eq!(repo.delete_provider("anthropic").unwrap(), 2);
        assert!(repo.list_by_provider("anthropic").unwrap().is_empty());
        assert_eq!(repo.list_by_provider("openai").unwrap().len(), 1);
    }

    #[test]
    fn timestamps_are_populated() {
        let db = db();
        let repo = CredentialRepo::new(&db);
        let row = repo
            .upsert("anthropic", "ANTHROPIC_API_KEY", b"a", b"na")
            .unwrap();
        assert!(!row.created_at.is_empty());
        assert!(!row.updated_at.is_empty());
    }
}
