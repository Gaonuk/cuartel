use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TailscaleDevice {
    pub hostname: String,
    pub addresses: Vec<String>,
    pub os: String,
    pub online: bool,
}

pub struct TailscaleClient {
    api_key: Option<String>,
}

impl TailscaleClient {
    pub fn new(api_key: Option<String>) -> Self {
        Self { api_key }
    }

    pub async fn list_devices(&self) -> Result<Vec<TailscaleDevice>> {
        // TODO: implement via tailscale API or local CLI
        Ok(vec![])
    }

    pub async fn check_connectivity(&self, ip: &str) -> Result<bool> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()?;
        match client.get(format!("http://{}:6420", ip)).send().await {
            Ok(_) => Ok(true),
            Err(_) => Ok(false),
        }
    }
}
