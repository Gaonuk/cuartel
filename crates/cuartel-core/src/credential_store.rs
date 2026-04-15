//! Stopgap credential store (spec task 3k).
//!
//! Backed by the macOS Keychain via the `keyring` crate. Phase 5a will
//! replace the storage with an AES-256-GCM SQLite table; when that lands,
//! only the internals of `KeychainCredentialStore` change — the trait and
//! all of its consumers (onboarding UI, sidecar env injection) stay put.
//!
//! Keys in the keychain are `(service = "dev.cuartel.credentials",
//! account = "{provider_id}:{env_key}")`. Scoping by both provider_id and
//! env_key lets a single provider expose multiple distinct credentials
//! (e.g. `anthropic:ANTHROPIC_API_KEY` vs `anthropic:ANTHROPIC_OAUTH_TOKEN`)
//! without losing which one is which.

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::{anyhow, Result};

use crate::agent::{AgentType, HarnessRegistry};
use crate::availability::EnvSource;

pub const KEYCHAIN_SERVICE: &str = "dev.cuartel.credentials";

/// Minimal CRUD surface used by onboarding (3j) and sidecar wiring (3l).
/// Implementations must be `Send + Sync` so `Arc<dyn CredentialStore>` can
/// travel across the tokio runtime / GPUI boundary.
pub trait CredentialStore: Send + Sync {
    fn get(&self, provider_id: &str, env_key: &str) -> Result<Option<String>>;
    fn set(&self, provider_id: &str, env_key: &str, value: &str) -> Result<()>;
    fn delete(&self, provider_id: &str, env_key: &str) -> Result<()>;
}

/// Default blanket impl: anything that is a `CredentialStore` is also an
/// `EnvSource` for availability probes. Errors from the store are logged
/// and reported as "not present" so a transient keychain failure doesn't
/// flip the onboarding matrix into a weird half-state.
impl<T: CredentialStore + ?Sized> EnvSource for T {
    fn has(&self, provider_id: &str, env_key: &str) -> bool {
        match self.get(provider_id, env_key) {
            Ok(Some(v)) => !v.is_empty(),
            Ok(None) => false,
            Err(e) => {
                log::warn!("credential store lookup failed for {provider_id}:{env_key}: {e}");
                false
            }
        }
    }
}

/// macOS Keychain-backed implementation using the `keyring` crate.
pub struct KeychainCredentialStore {
    service: String,
}

impl KeychainCredentialStore {
    pub fn new() -> Self {
        Self {
            service: KEYCHAIN_SERVICE.to_string(),
        }
    }

    pub fn with_service(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    fn entry(&self, provider_id: &str, env_key: &str) -> Result<keyring::Entry> {
        let account = format!("{provider_id}:{env_key}");
        keyring::Entry::new(&self.service, &account)
            .map_err(|e| anyhow!("keyring entry init failed: {e}"))
    }
}

impl Default for KeychainCredentialStore {
    fn default() -> Self {
        Self::new()
    }
}

impl CredentialStore for KeychainCredentialStore {
    fn get(&self, provider_id: &str, env_key: &str) -> Result<Option<String>> {
        let entry = self.entry(provider_id, env_key)?;
        match entry.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(anyhow!("keyring read failed: {e}")),
        }
    }

    fn set(&self, provider_id: &str, env_key: &str, value: &str) -> Result<()> {
        let entry = self.entry(provider_id, env_key)?;
        entry
            .set_password(value)
            .map_err(|e| anyhow!("keyring write failed: {e}"))
    }

    fn delete(&self, provider_id: &str, env_key: &str) -> Result<()> {
        let entry = self.entry(provider_id, env_key)?;
        match entry.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(anyhow!("keyring delete failed: {e}")),
        }
    }
}

/// Read every required env var for `agent` out of the credential store and
/// return them as a plain `HashMap<String, String>` ready to hand to
/// `Command::env()`. Missing keys are skipped silently — callers that care
/// about completeness should run an availability probe first.
///
/// This is the glue task 3l uses to populate the sidecar's environment
/// before spawning `npx tsx server.ts`.
pub fn env_for_harness(
    registry: &HarnessRegistry,
    store: &dyn CredentialStore,
    agent: &AgentType,
) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Some(harness) = registry.get(agent) else {
        return out;
    };
    let provider = harness.provider_id();
    for key in harness.required_env_keys() {
        match store.get(provider, key) {
            Ok(Some(value)) if !value.is_empty() => {
                out.insert((*key).to_string(), value);
            }
            Ok(_) => {}
            Err(e) => log::warn!("credential store lookup failed for {provider}:{key}: {e}"),
        }
    }
    for (k, v) in harness.extra_env() {
        out.insert(k.to_string(), v);
    }
    out
}

/// In-memory store used in tests and as a fallback when the user is in an
/// environment where the system keychain is unavailable (CI, headless).
#[derive(Default)]
pub struct MemoryCredentialStore {
    inner: Mutex<HashMap<(String, String), String>>,
}

impl MemoryCredentialStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl CredentialStore for MemoryCredentialStore {
    fn get(&self, provider_id: &str, env_key: &str) -> Result<Option<String>> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .get(&(provider_id.into(), env_key.into()))
            .cloned())
    }

    fn set(&self, provider_id: &str, env_key: &str, value: &str) -> Result<()> {
        self.inner
            .lock()
            .unwrap()
            .insert((provider_id.into(), env_key.into()), value.into());
        Ok(())
    }

    fn delete(&self, provider_id: &str, env_key: &str) -> Result<()> {
        self.inner
            .lock()
            .unwrap()
            .remove(&(provider_id.into(), env_key.into()));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_round_trip() {
        let store = MemoryCredentialStore::new();
        assert!(store
            .get("anthropic", "ANTHROPIC_API_KEY")
            .unwrap()
            .is_none());
        store
            .set("anthropic", "ANTHROPIC_API_KEY", "sk-test")
            .unwrap();
        assert_eq!(
            store
                .get("anthropic", "ANTHROPIC_API_KEY")
                .unwrap()
                .as_deref(),
            Some("sk-test"),
        );
        store.delete("anthropic", "ANTHROPIC_API_KEY").unwrap();
        assert!(store
            .get("anthropic", "ANTHROPIC_API_KEY")
            .unwrap()
            .is_none());
    }

    #[test]
    fn memory_store_scopes_by_provider_and_env_key() {
        let store = MemoryCredentialStore::new();
        store.set("anthropic", "ANTHROPIC_API_KEY", "sk-a").unwrap();
        store
            .set("anthropic", "ANTHROPIC_OAUTH_TOKEN", "oa-a")
            .unwrap();
        store.set("openai", "OPENAI_API_KEY", "sk-o").unwrap();
        assert_eq!(
            store
                .get("anthropic", "ANTHROPIC_API_KEY")
                .unwrap()
                .unwrap(),
            "sk-a"
        );
        assert_eq!(
            store
                .get("anthropic", "ANTHROPIC_OAUTH_TOKEN")
                .unwrap()
                .unwrap(),
            "oa-a"
        );
        assert_eq!(
            store.get("openai", "OPENAI_API_KEY").unwrap().unwrap(),
            "sk-o"
        );
    }

    #[test]
    fn env_source_blanket_impl_answers_present_when_value_nonempty() {
        let store = MemoryCredentialStore::new();
        let env: &dyn EnvSource = &store;
        assert!(!env.has("anthropic", "ANTHROPIC_API_KEY"));
        store.set("anthropic", "ANTHROPIC_API_KEY", "sk-x").unwrap();
        assert!(env.has("anthropic", "ANTHROPIC_API_KEY"));
    }

    #[test]
    fn env_source_blanket_impl_skips_empty_string_values() {
        let store = MemoryCredentialStore::new();
        store.set("anthropic", "ANTHROPIC_API_KEY", "").unwrap();
        let env: &dyn EnvSource = &store;
        assert!(!env.has("anthropic", "ANTHROPIC_API_KEY"));
    }

    #[test]
    fn env_for_harness_returns_required_keys_when_present() {
        let registry = HarnessRegistry::with_builtins();
        let store = MemoryCredentialStore::new();
        store.set("anthropic", "ANTHROPIC_API_KEY", "sk-a").unwrap();
        let env = env_for_harness(&registry, &store, &AgentType::Pi);
        assert_eq!(
            env.get("ANTHROPIC_API_KEY").map(String::as_str),
            Some("sk-a")
        );
    }

    #[test]
    fn env_for_harness_skips_missing_keys_silently() {
        let registry = HarnessRegistry::with_builtins();
        let store = MemoryCredentialStore::new();
        let env = env_for_harness(&registry, &store, &AgentType::Codex);
        assert!(env.is_empty());
    }

    #[test]
    fn env_for_harness_includes_extra_env_from_harness() {
        let registry = HarnessRegistry::with_builtins();
        let store = MemoryCredentialStore::new();
        store.set("anthropic", "ANTHROPIC_API_KEY", "sk-a").unwrap();
        let env = env_for_harness(&registry, &store, &AgentType::Pi);
        assert_eq!(
            env.get("ANTHROPIC_API_KEY").map(String::as_str),
            Some("sk-a")
        );
        assert_eq!(
            env.get("PI_DEFAULT_PROVIDER").map(String::as_str),
            Some("anthropic")
        );
        assert_eq!(
            env.get("PI_DEFAULT_MODEL").map(String::as_str),
            Some("claude-sonnet-4-20250514")
        );
    }
}
