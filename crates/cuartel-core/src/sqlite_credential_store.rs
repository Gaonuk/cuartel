//! SQLite-backed [`CredentialStore`] (spec task 5a, API half).
//!
//! Composes [`cuartel_db::credentials::CredentialRepo`] with
//! [`cuartel_db::crypto::Vault`] to implement the same
//! `CredentialStore` trait the stopgap [`KeychainCredentialStore`] did in
//! task 3k. Callers (onboarding UI, sidecar env injection) should not have
//! to change when we migrate from the keychain backend to this one:
//! `Arc<dyn CredentialStore>` still works, and the `(provider_id, env_key)`
//! scoping is identical.
//!
//! # Threading
//!
//! `rusqlite::Connection` is `Send` but not `Sync`, so we wrap the
//! [`Database`] in a `Mutex` and take it as `Arc<Mutex<Database>>` so the
//! same DB handle can be shared with other subsystems (workspaces, sessions,
//! …). The [`Vault`] is cheap to clone conceptually but we share it via
//! `Arc` to keep the AES key allocation pinned.
//!
//! # Encryption envelope
//!
//! Each write goes plaintext → `Vault::encrypt` → `(ciphertext, nonce)` →
//! `CredentialRepo::upsert`. Each read reverses the flow. The repo knows
//! nothing about AES; the store knows nothing about SQL. That split means a
//! unit test can swap in an in-memory DB with a deterministic key without
//! touching either side.
//!
//! [`KeychainCredentialStore`]: crate::credential_store::KeychainCredentialStore

use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use cuartel_db::credentials::CredentialRepo;
use cuartel_db::crypto::Vault;
use cuartel_db::Database;

use crate::credential_store::CredentialStore;

pub struct SqliteCredentialStore {
    db: Arc<Mutex<Database>>,
    vault: Arc<Vault>,
}

impl SqliteCredentialStore {
    pub fn new(db: Arc<Mutex<Database>>, vault: Arc<Vault>) -> Self {
        Self { db, vault }
    }

    /// Test helper: build a store backed by an in-memory SQLite DB and a
    /// deterministic AES key. Never use this in production — a fixed key
    /// defeats the entire point of encrypting the store.
    #[cfg(test)]
    fn in_memory_for_tests() -> Result<Self> {
        let db = Database::open_in_memory()?;
        let vault = Vault::new(&[0u8; 32]);
        Ok(Self::new(Arc::new(Mutex::new(db)), Arc::new(vault)))
    }

    /// List all `(provider, env_key)` pairs currently stored. Used by the
    /// settings UI (5b) to render the credential inventory without ever
    /// decrypting the values.
    pub fn list_entries(&self) -> Result<Vec<(String, String)>> {
        let db = self
            .db
            .lock()
            .map_err(|_| anyhow!("credential store mutex poisoned"))?;
        let repo = CredentialRepo::new(&db);
        Ok(repo
            .list()?
            .into_iter()
            .map(|row| (row.provider, row.env_key))
            .collect())
    }

    /// Remove every credential for `provider` in one shot.
    pub fn delete_provider(&self, provider: &str) -> Result<usize> {
        let db = self
            .db
            .lock()
            .map_err(|_| anyhow!("credential store mutex poisoned"))?;
        CredentialRepo::new(&db).delete_provider(provider)
    }
}

impl CredentialStore for SqliteCredentialStore {
    fn get(&self, provider_id: &str, env_key: &str) -> Result<Option<String>> {
        let db = self
            .db
            .lock()
            .map_err(|_| anyhow!("credential store mutex poisoned"))?;
        let repo = CredentialRepo::new(&db);
        let Some(row) = repo.get(provider_id, env_key)? else {
            return Ok(None);
        };
        let plaintext = self.vault.decrypt(&row.encrypted_value, &row.nonce)?;
        // Credential values are UTF-8 (API keys, OAuth tokens). A non-UTF-8
        // blob means either a corrupted row or a schema violation — both
        // are hard errors, not "key not present".
        let value = String::from_utf8(plaintext)
            .map_err(|e| anyhow!("credential {provider_id}:{env_key} is not valid UTF-8: {e}"))?;
        Ok(Some(value))
    }

    fn set(&self, provider_id: &str, env_key: &str, value: &str) -> Result<()> {
        let (ciphertext, nonce) = self.vault.encrypt(value.as_bytes())?;
        let db = self
            .db
            .lock()
            .map_err(|_| anyhow!("credential store mutex poisoned"))?;
        CredentialRepo::new(&db).upsert(provider_id, env_key, &ciphertext, &nonce)?;
        Ok(())
    }

    fn delete(&self, provider_id: &str, env_key: &str) -> Result<()> {
        let db = self
            .db
            .lock()
            .map_err(|_| anyhow!("credential store mutex poisoned"))?;
        CredentialRepo::new(&db).delete(provider_id, env_key)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::availability::EnvSource;

    fn store() -> SqliteCredentialStore {
        SqliteCredentialStore::in_memory_for_tests().unwrap()
    }

    #[test]
    fn set_and_get_round_trip() {
        let store = store();
        assert!(store
            .get("anthropic", "ANTHROPIC_API_KEY")
            .unwrap()
            .is_none());
        store
            .set("anthropic", "ANTHROPIC_API_KEY", "sk-secret")
            .unwrap();
        assert_eq!(
            store
                .get("anthropic", "ANTHROPIC_API_KEY")
                .unwrap()
                .as_deref(),
            Some("sk-secret"),
        );
    }

    #[test]
    fn set_overwrites_previous_value() {
        let store = store();
        store.set("openai", "OPENAI_API_KEY", "v1").unwrap();
        store.set("openai", "OPENAI_API_KEY", "v2").unwrap();
        assert_eq!(
            store.get("openai", "OPENAI_API_KEY").unwrap().as_deref(),
            Some("v2"),
        );
    }

    #[test]
    fn delete_removes_only_targeted_row() {
        let store = store();
        store.set("anthropic", "ANTHROPIC_API_KEY", "a").unwrap();
        store
            .set("anthropic", "ANTHROPIC_OAUTH_TOKEN", "b")
            .unwrap();
        store.delete("anthropic", "ANTHROPIC_API_KEY").unwrap();
        assert!(store
            .get("anthropic", "ANTHROPIC_API_KEY")
            .unwrap()
            .is_none());
        assert_eq!(
            store
                .get("anthropic", "ANTHROPIC_OAUTH_TOKEN")
                .unwrap()
                .as_deref(),
            Some("b"),
        );
    }

    #[test]
    fn delete_missing_is_noop() {
        let store = store();
        // Matches the Keychain store's behaviour: deleting a missing key is
        // not an error. Onboarding "clear key" buttons rely on this.
        store.delete("anthropic", "ANTHROPIC_API_KEY").unwrap();
    }

    #[test]
    fn scopes_by_provider_and_env_key() {
        let store = store();
        store.set("anthropic", "ANTHROPIC_API_KEY", "a").unwrap();
        store
            .set("anthropic", "ANTHROPIC_OAUTH_TOKEN", "b")
            .unwrap();
        store.set("openai", "OPENAI_API_KEY", "c").unwrap();
        assert_eq!(
            store
                .get("anthropic", "ANTHROPIC_API_KEY")
                .unwrap()
                .as_deref(),
            Some("a")
        );
        assert_eq!(
            store
                .get("anthropic", "ANTHROPIC_OAUTH_TOKEN")
                .unwrap()
                .as_deref(),
            Some("b")
        );
        assert_eq!(
            store.get("openai", "OPENAI_API_KEY").unwrap().as_deref(),
            Some("c")
        );
    }

    #[test]
    fn plaintext_is_not_stored_in_the_database() {
        // Sanity check that encryption actually happens: fish the raw blob
        // out of SQLite and make sure it doesn't contain the plaintext.
        let db = Arc::new(Mutex::new(Database::open_in_memory().unwrap()));
        let vault = Arc::new(Vault::new(&[7u8; 32]));
        let store = SqliteCredentialStore::new(db.clone(), vault);
        store
            .set("anthropic", "ANTHROPIC_API_KEY", "sk-plaintext-marker")
            .unwrap();

        let locked = db.lock().unwrap();
        let repo = CredentialRepo::new(&locked);
        let row = repo.get("anthropic", "ANTHROPIC_API_KEY").unwrap().unwrap();
        assert_ne!(row.encrypted_value, b"sk-plaintext-marker");
        let needle = b"sk-plaintext-marker";
        assert!(
            !row.encrypted_value
                .windows(needle.len())
                .any(|w| w == needle),
            "plaintext leaked into encrypted_value blob"
        );
    }

    #[test]
    fn different_writes_use_different_nonces() {
        // AES-GCM with a reused nonce is catastrophically broken — the
        // Vault must produce a fresh nonce per encrypt call. This test
        // pins that guarantee at the store level.
        let db = Arc::new(Mutex::new(Database::open_in_memory().unwrap()));
        let vault = Arc::new(Vault::new(&[0u8; 32]));
        let store = SqliteCredentialStore::new(db.clone(), vault);
        store.set("openai", "OPENAI_API_KEY", "same-value").unwrap();
        let first_nonce = {
            let locked = db.lock().unwrap();
            CredentialRepo::new(&locked)
                .get("openai", "OPENAI_API_KEY")
                .unwrap()
                .unwrap()
                .nonce
        };
        store.set("openai", "OPENAI_API_KEY", "same-value").unwrap();
        let second_nonce = {
            let locked = db.lock().unwrap();
            CredentialRepo::new(&locked)
                .get("openai", "OPENAI_API_KEY")
                .unwrap()
                .unwrap()
                .nonce
        };
        assert_ne!(first_nonce, second_nonce);
    }

    #[test]
    fn works_as_env_source_for_availability_probe() {
        let store = store();
        let env: &dyn EnvSource = &store;
        assert!(!env.has("anthropic", "ANTHROPIC_API_KEY"));
        store.set("anthropic", "ANTHROPIC_API_KEY", "sk-x").unwrap();
        assert!(env.has("anthropic", "ANTHROPIC_API_KEY"));
        store.set("anthropic", "ANTHROPIC_API_KEY", "").unwrap();
        assert!(!env.has("anthropic", "ANTHROPIC_API_KEY"));
    }

    #[test]
    fn list_entries_returns_provider_env_key_pairs_without_decrypting() {
        let store = store();
        store.set("anthropic", "ANTHROPIC_API_KEY", "a").unwrap();
        store.set("openai", "OPENAI_API_KEY", "b").unwrap();
        let mut entries = store.list_entries().unwrap();
        entries.sort();
        assert_eq!(
            entries,
            vec![
                ("anthropic".into(), "ANTHROPIC_API_KEY".into()),
                ("openai".into(), "OPENAI_API_KEY".into()),
            ]
        );
    }

    #[test]
    fn delete_provider_clears_all_entries_for_provider() {
        let store = store();
        store.set("anthropic", "ANTHROPIC_API_KEY", "a").unwrap();
        store
            .set("anthropic", "ANTHROPIC_OAUTH_TOKEN", "b")
            .unwrap();
        store.set("openai", "OPENAI_API_KEY", "c").unwrap();
        assert_eq!(store.delete_provider("anthropic").unwrap(), 2);
        assert!(store
            .get("anthropic", "ANTHROPIC_API_KEY")
            .unwrap()
            .is_none());
        assert!(store
            .get("anthropic", "ANTHROPIC_OAUTH_TOKEN")
            .unwrap()
            .is_none());
        assert_eq!(
            store.get("openai", "OPENAI_API_KEY").unwrap().as_deref(),
            Some("c")
        );
    }

    #[test]
    fn decrypt_with_wrong_key_fails() {
        // Persistence across Vault swap: data written with one key cannot
        // be decrypted with another. This is the spec's threat model for
        // "what if someone copies the SQLite file".
        let db = Arc::new(Mutex::new(Database::open_in_memory().unwrap()));
        let good_vault = Arc::new(Vault::new(&[1u8; 32]));
        let good = SqliteCredentialStore::new(db.clone(), good_vault);
        good.set("anthropic", "ANTHROPIC_API_KEY", "sk-x").unwrap();

        let bad_vault = Arc::new(Vault::new(&[2u8; 32]));
        let bad = SqliteCredentialStore::new(db, bad_vault);
        assert!(bad.get("anthropic", "ANTHROPIC_API_KEY").is_err());
    }
}
