use anyhow::Result;
use reqwest::Client;
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct RivetClient {
    base_url: String,
    http: Client,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmInstance {
    pub id: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEvent {
    pub method: String,
    #[serde(default)]
    pub data: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

impl RivetClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http: Client::new(),
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub async fn get_or_create_vm(&self, tags: &[&str]) -> Result<VmInstance> {
        let resp = self
            .http
            .post(format!("{}/vm/getOrCreate", self.base_url))
            .json(&serde_json::json!({ "tags": tags }))
            .send()
            .await?
            .error_for_status()?;
        Ok(resp.json().await?)
    }

    pub async fn create_session(
        &self,
        vm_id: &str,
        agent: &str,
        env: &std::collections::HashMap<String, String>,
    ) -> Result<SessionInfo> {
        let resp = self
            .http
            .post(format!("{}/vm/{}/createSession", self.base_url, vm_id))
            .json(&serde_json::json!({
                "agent": agent,
                "env": env,
            }))
            .send()
            .await?
            .error_for_status()?;
        Ok(resp.json().await?)
    }

    pub async fn send_prompt(
        &self,
        vm_id: &str,
        session_id: &str,
        prompt: &str,
    ) -> Result<serde_json::Value> {
        let resp = self
            .http
            .post(format!("{}/vm/{}/sendPrompt", self.base_url, vm_id))
            .json(&serde_json::json!({
                "sessionId": session_id,
                "prompt": prompt,
            }))
            .send()
            .await?
            .error_for_status()?;
        Ok(resp.json().await?)
    }

    pub async fn read_file(&self, vm_id: &str, path: &str) -> Result<Vec<u8>> {
        let resp = self
            .http
            .get(format!("{}/vm/{}/readFile", self.base_url, vm_id))
            .query(&[("path", path)])
            .send()
            .await?
            .error_for_status()?;
        Ok(resp.bytes().await?.to_vec())
    }

    pub async fn write_file(&self, vm_id: &str, path: &str, content: &[u8]) -> Result<()> {
        self.http
            .post(format!("{}/vm/{}/writeFile", self.base_url, vm_id))
            .json(&serde_json::json!({
                "path": path,
                "content": base64_encode(content),
            }))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    pub async fn exec(&self, vm_id: &str, command: &str) -> Result<ExecResult> {
        let resp = self
            .http
            .post(format!("{}/vm/{}/exec", self.base_url, vm_id))
            .json(&serde_json::json!({ "command": command }))
            .send()
            .await?
            .error_for_status()?;
        Ok(resp.json().await?)
    }
}

fn base64_encode(data: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(data.len() * 4 / 3 + 4);
    for chunk in data.chunks(3) {
        let b = match chunk.len() {
            3 => [chunk[0], chunk[1], chunk[2]],
            2 => [chunk[0], chunk[1], 0],
            _ => [chunk[0], 0, 0],
        };
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32);
        const CHARS: &[u8] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let _ = write!(s, "{}", CHARS[((n >> 18) & 63) as usize] as char);
        let _ = write!(s, "{}", CHARS[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            let _ = write!(s, "{}", CHARS[((n >> 6) & 63) as usize] as char);
        } else {
            s.push('=');
        }
        if chunk.len() > 2 {
            let _ = write!(s, "{}", CHARS[(n & 63) as usize] as char);
        } else {
            s.push('=');
        }
    }
    s
}
