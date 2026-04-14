use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteServer {
    pub id: String,
    pub name: String,
    pub address: String,
    pub tailscale_ip: Option<String>,
    pub is_local: bool,
}

impl RemoteServer {
    pub fn local() -> Self {
        Self {
            id: "local".to_string(),
            name: "This Mac".to_string(),
            address: "http://localhost:6420".to_string(),
            tailscale_ip: None,
            is_local: true,
        }
    }

    pub fn rivet_url(&self) -> &str {
        &self.address
    }
}
