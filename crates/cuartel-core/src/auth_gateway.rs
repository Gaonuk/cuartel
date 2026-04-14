use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthGatewayConfig {
    pub rules: Vec<AuthRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthRule {
    pub pattern: String,
    pub headers: HashMap<String, String>,
}

impl Default for AuthGatewayConfig {
    fn default() -> Self {
        Self { rules: vec![] }
    }
}
