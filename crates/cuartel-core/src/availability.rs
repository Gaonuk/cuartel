//! Harness availability detection (spec task 3i).
//!
//! For each registered harness, probe the host to figure out whether the
//! user could actually run it today: is the CLI installed, which version,
//! and which required env vars are still missing from the credential store.
//!
//! The probe is intentionally a pure input/output function: it takes a
//! `HarnessRegistry` and an `EnvSource` (anything that can answer "do you
//! have a value for this env key?") and returns a `Vec<HarnessAvailability>`.
//! The onboarding UI (3j) renders it; the session host (3l) reads it to
//! decide whether a default harness is ready to launch.
//!
//! Probing `which` is done via `tokio::process::Command` so we stay on the
//! same async runtime as the rest of the app. A `ProgramProbe` trait is
//! provided so tests can swap in a deterministic probe without needing
//! real binaries on PATH.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::agent::{AgentHarness, AgentType, HarnessRegistry};

/// Lookup API for "do we have a credential for this provider?". Kept
/// abstract so both the keychain store and in-memory test doubles can
/// implement it without coupling `availability` to either.
pub trait EnvSource {
    fn has(&self, provider_id: &str, env_key: &str) -> bool;
}

/// No-op env source used when the UI wants a pure "installed?" check that
/// ignores credential state.
pub struct NoEnv;
impl EnvSource for NoEnv {
    fn has(&self, _provider_id: &str, _env_key: &str) -> bool {
        false
    }
}

/// Snapshot describing what the onboarding UI should show for a harness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessAvailability {
    pub agent: AgentType,
    pub display_name: String,
    pub provider_id: String,
    /// Whether `probe_program` is visible on the host. `None` means the
    /// harness opted out of probing (e.g. it's a managed npm package), in
    /// which case we treat it as installed for status-matrix purposes.
    pub installed: bool,
    pub version: Option<String>,
    pub install_hint: Option<String>,
    /// Every required env var, annotated with whether the credential store
    /// already has a value for it. `required_env.iter().all(|e| e.present)`
    /// is the fast path the onboarding UI uses to decide `ready` vs
    /// `needs credentials`.
    pub required_env: Vec<RequiredEnv>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequiredEnv {
    pub key: String,
    pub present: bool,
}

impl HarnessAvailability {
    /// Highest-level status badge for the onboarding matrix. Order matters:
    /// `Unsupported` takes precedence over `NotInstalled`, which takes
    /// precedence over `NeedsCredentials`, which in turn outranks `Ready`.
    pub fn status(&self) -> AvailabilityStatus {
        if !self.installed {
            return AvailabilityStatus::NotInstalled;
        }
        if self.required_env.iter().any(|e| !e.present) {
            return AvailabilityStatus::NeedsCredentials;
        }
        AvailabilityStatus::Ready
    }

    /// Env keys from `required_env` that are still missing from the
    /// credential store. Used by 3l to know which keys to fetch and inject
    /// into the sidecar process.
    pub fn missing_env_keys(&self) -> Vec<&str> {
        self.required_env
            .iter()
            .filter(|e| !e.present)
            .map(|e| e.key.as_str())
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvailabilityStatus {
    Ready,
    NeedsCredentials,
    NotInstalled,
}

impl AvailabilityStatus {
    pub fn label(self) -> &'static str {
        match self {
            AvailabilityStatus::Ready => "ready",
            AvailabilityStatus::NeedsCredentials => "needs credentials",
            AvailabilityStatus::NotInstalled => "not installed",
        }
    }
}

/// Result of probing a single binary: `None` if `which` returned non-zero
/// (not on PATH), else the absolute path plus an optional version string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeResult {
    pub path: String,
    pub version: Option<String>,
}

#[async_trait]
pub trait ProgramProbe: Send + Sync {
    async fn probe(&self, program: &str) -> Option<ProbeResult>;
}

/// Default probe: shells out to `which` and, if it succeeds, runs
/// `<program> --version` to capture the version line.
///
/// Uses `std::process::Command` (not `tokio::process::Command`) so the
/// probe works in a plain sync context — the app runs it once at startup
/// before any runtime is fully up. Each call is <100ms in practice, so
/// blocking an async worker thread here is acceptable.
pub struct WhichProbe;

#[async_trait]
impl ProgramProbe for WhichProbe {
    async fn probe(&self, program: &str) -> Option<ProbeResult> {
        use std::process::Command;

        let which = Command::new("which").arg(program).output().ok()?;
        if !which.status.success() {
            return None;
        }
        let path = String::from_utf8_lossy(&which.stdout).trim().to_string();
        if path.is_empty() {
            return None;
        }

        // Version probe is best-effort — some CLIs don't support --version,
        // some print to stderr. Swallow failures and return None so the
        // row still renders with `installed: true`.
        let version_out = Command::new(program).arg("--version").output().ok();

        let version = version_out.and_then(|out| {
            if !out.status.success() {
                return None;
            }
            let combined = [out.stdout, out.stderr].concat();
            let line = String::from_utf8_lossy(&combined)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            if line.is_empty() {
                None
            } else {
                Some(line)
            }
        });

        Some(ProbeResult { path, version })
    }
}

pub async fn probe_harness<E>(
    harness: &Arc<dyn AgentHarness>,
    probe: &dyn ProgramProbe,
    env: &E,
) -> HarnessAvailability
where
    E: EnvSource + ?Sized,
{
    let agent = harness.agent_type();
    let display_name = agent.display_name().to_string();
    let provider_id = harness.provider_id().to_string();

    let (installed, version) = match harness.probe_program() {
        None => (true, None),
        Some(program) => match probe.probe(program).await {
            Some(r) => (true, r.version),
            None => (false, None),
        },
    };

    let hint = harness.install_hint();
    let install_hint = if hint.is_empty() {
        None
    } else {
        Some(hint.to_string())
    };

    let required_env = harness
        .required_env_keys()
        .iter()
        .map(|k| RequiredEnv {
            key: (*k).to_string(),
            present: env.has(&provider_id, k),
        })
        .collect();

    HarnessAvailability {
        agent,
        display_name,
        provider_id,
        installed,
        version,
        install_hint,
        required_env,
    }
}

pub async fn probe_registry<E>(
    registry: &HarnessRegistry,
    probe: &dyn ProgramProbe,
    env: &E,
) -> Vec<HarnessAvailability>
where
    E: EnvSource + ?Sized,
{
    let mut out = Vec::new();
    for agent in registry.registered() {
        if let Some(h) = registry.get(&agent) {
            out.push(probe_harness(&h, probe, env).await);
        }
    }
    // Stable order for the UI: sort by display name.
    out.sort_by(|a, b| a.display_name.cmp(&b.display_name));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};
    use std::sync::Mutex;

    struct FakeProbe {
        installed: HashMap<String, ProbeResult>,
    }

    #[async_trait]
    impl ProgramProbe for FakeProbe {
        async fn probe(&self, program: &str) -> Option<ProbeResult> {
            self.installed.get(program).cloned()
        }
    }

    #[derive(Default)]
    struct FakeEnv {
        present: Mutex<HashSet<(String, String)>>,
    }

    impl FakeEnv {
        fn with(pairs: &[(&str, &str)]) -> Self {
            let e = Self::default();
            for (p, k) in pairs {
                e.present
                    .lock()
                    .unwrap()
                    .insert(((*p).to_string(), (*k).to_string()));
            }
            e
        }
    }

    impl EnvSource for FakeEnv {
        fn has(&self, provider_id: &str, env_key: &str) -> bool {
            self.present
                .lock()
                .unwrap()
                .contains(&(provider_id.to_string(), env_key.to_string()))
        }
    }

    #[tokio::test]
    async fn ready_when_installed_and_all_envs_present() {
        let registry = HarnessRegistry::with_builtins();
        let probe = FakeProbe {
            installed: [(
                "pi".to_string(),
                ProbeResult {
                    path: "/usr/local/bin/pi".into(),
                    version: Some("pi 1.0.0".into()),
                },
            )]
            .into_iter()
            .collect(),
        };
        let env = FakeEnv::with(&[("anthropic", "ANTHROPIC_API_KEY")]);

        let h = registry.get(&AgentType::Pi).unwrap();
        let avail = probe_harness(&h, &probe, &env).await;
        assert!(avail.installed);
        assert_eq!(avail.version.as_deref(), Some("pi 1.0.0"));
        assert_eq!(avail.status(), AvailabilityStatus::Ready);
        assert!(avail.missing_env_keys().is_empty());
    }

    #[tokio::test]
    async fn needs_credentials_when_env_missing() {
        let registry = HarnessRegistry::with_builtins();
        let probe = FakeProbe {
            installed: [(
                "claude".to_string(),
                ProbeResult {
                    path: "/usr/local/bin/claude".into(),
                    version: None,
                },
            )]
            .into_iter()
            .collect(),
        };
        let env = FakeEnv::default();

        let h = registry.get(&AgentType::ClaudeCode).unwrap();
        let avail = probe_harness(&h, &probe, &env).await;
        assert!(avail.installed);
        assert_eq!(avail.status(), AvailabilityStatus::NeedsCredentials);
        assert_eq!(avail.missing_env_keys(), vec!["ANTHROPIC_API_KEY"]);
    }

    #[tokio::test]
    async fn not_installed_when_which_returns_none() {
        let registry = HarnessRegistry::with_builtins();
        let probe = FakeProbe {
            installed: HashMap::new(),
        };
        let env = FakeEnv::with(&[("openai", "OPENAI_API_KEY")]);

        let h = registry.get(&AgentType::Codex).unwrap();
        let avail = probe_harness(&h, &probe, &env).await;
        assert!(!avail.installed);
        assert_eq!(avail.status(), AvailabilityStatus::NotInstalled);
        assert_eq!(
            avail.install_hint.as_deref(),
            Some("npm install -g @openai/codex")
        );
    }

    #[tokio::test]
    async fn probe_registry_returns_all_harnesses_sorted() {
        let registry = HarnessRegistry::with_builtins();
        let probe = FakeProbe {
            installed: HashMap::new(),
        };
        let env = FakeEnv::default();
        let out = probe_registry(&registry, &probe, &env).await;
        assert_eq!(out.len(), 4);
        let names: Vec<&str> = out.iter().map(|a| a.display_name.as_str()).collect();
        let mut expected = names.clone();
        expected.sort();
        assert_eq!(names, expected);
    }
}
