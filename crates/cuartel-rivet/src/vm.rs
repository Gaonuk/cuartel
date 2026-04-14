use crate::client::{RivetClient, VmInstance};
use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VmStatus {
    Creating,
    Running,
    Sleeping,
    Stopped,
    Error(String),
}

pub struct VmManager {
    client: RivetClient,
}

impl VmManager {
    pub fn new(client: RivetClient) -> Self {
        Self { client }
    }

    pub async fn get_or_create(&self, tags: &[&str]) -> Result<VmInstance> {
        self.client.get_or_create_vm(tags).await
    }

    pub async fn exec(&self, vm_id: &str, command: &str) -> Result<crate::client::ExecResult> {
        self.client.exec(vm_id, command).await
    }
}
