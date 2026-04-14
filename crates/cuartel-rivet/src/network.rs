use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortForward {
    pub sandbox_port: u16,
    pub host_port: u16,
    pub protocol: PortProtocol,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PortProtocol {
    Tcp,
    Udp,
}
