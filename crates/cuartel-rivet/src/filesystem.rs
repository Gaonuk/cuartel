use crate::client::RivetClient;
use anyhow::Result;

pub struct VmFilesystem {
    client: RivetClient,
    vm_id: String,
}

impl VmFilesystem {
    pub fn new(client: RivetClient, vm_id: &str) -> Self {
        Self {
            client,
            vm_id: vm_id.to_string(),
        }
    }

    pub async fn read_file(&self, path: &str) -> Result<Vec<u8>> {
        self.client.read_file(&self.vm_id, path).await
    }

    pub async fn write_file(&self, path: &str, content: &[u8]) -> Result<()> {
        self.client.write_file(&self.vm_id, path, content).await
    }
}
