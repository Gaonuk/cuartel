//! Onboarding configuration (the non-secret half of task 3k/3j).
//!
//! API keys live in the credential store (macOS Keychain today, encrypted
//! SQLite once 5a lands). The *choice* of default harness, and a bit
//! indicating that the user has already dismissed the first-run modal,
//! live alongside the data directory as a small JSON file — not sensitive,
//! not worth a keychain round-trip on every launch.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::agent::AgentType;

const ONBOARDING_FILENAME: &str = "onboarding.json";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnboardingConfig {
    /// AgentType the user picked as "run this by default when creating a
    /// session". `None` while the user is still mid-onboarding.
    #[serde(default)]
    pub default_harness: Option<AgentType>,
    /// Set once the onboarding modal has been dismissed. We still show it
    /// from the settings menu afterwards, but the first-run gate only fires
    /// when this is `false`.
    #[serde(default)]
    pub completed: bool,
}

impl OnboardingConfig {
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join(ONBOARDING_FILENAME)
    }

    pub fn load(data_dir: &Path) -> Result<Self> {
        let path = Self::path(data_dir);
        if !path.exists() {
            return Ok(Self::default());
        }
        let contents = fs::read_to_string(&path)
            .with_context(|| format!("reading onboarding config at {}", path.display()))?;
        let cfg: OnboardingConfig = serde_json::from_str(&contents)
            .with_context(|| format!("parsing onboarding config at {}", path.display()))?;
        Ok(cfg)
    }

    pub fn save(&self, data_dir: &Path) -> Result<()> {
        fs::create_dir_all(data_dir)
            .with_context(|| format!("creating data dir {}", data_dir.display()))?;
        let path = Self::path(data_dir);
        let contents = serde_json::to_string_pretty(self)?;
        fs::write(&path, contents)
            .with_context(|| format!("writing onboarding config to {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn load_returns_default_when_file_missing() {
        let dir = tempdir().unwrap();
        let cfg = OnboardingConfig::load(dir.path()).unwrap();
        assert_eq!(cfg, OnboardingConfig::default());
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempdir().unwrap();
        let cfg = OnboardingConfig {
            default_harness: Some(AgentType::Pi),
            completed: true,
        };
        cfg.save(dir.path()).unwrap();
        let reloaded = OnboardingConfig::load(dir.path()).unwrap();
        assert_eq!(reloaded, cfg);
    }
}
