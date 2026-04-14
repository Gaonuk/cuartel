use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub rivet_port: u16,
    pub data_dir: PathBuf,
    pub theme: ThemeConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeConfig {
    pub mode: ThemeMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ThemeMode {
    Dark,
    Light,
}

impl Default for AppConfig {
    fn default() -> Self {
        let data_dir = dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("cuartel");
        Self {
            rivet_port: 6420,
            data_dir,
            theme: ThemeConfig {
                mode: ThemeMode::Dark,
            },
        }
    }
}
