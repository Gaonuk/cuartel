use anyhow::{Result, anyhow};
use log::{error, info};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::{Child, Command};

pub struct Sidecar {
    process: Option<Child>,
    rivet_dir: PathBuf,
    port: u16,
}

impl Sidecar {
    pub fn new(rivet_dir: PathBuf, port: u16) -> Self {
        Self {
            process: None,
            rivet_dir,
            port,
        }
    }

    pub async fn ensure_deps_installed(&self) -> Result<()> {
        if !self.rivet_dir.join("node_modules").exists() {
            info!("installing rivet sidecar dependencies...");
            let status = Command::new("npm")
                .arg("install")
                .current_dir(&self.rivet_dir)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .status()
                .await?;
            if !status.success() {
                return Err(anyhow!("npm install failed in rivet sidecar directory"));
            }
        }
        Ok(())
    }

    pub async fn start(&mut self) -> Result<()> {
        if self.process.is_some() {
            return Ok(());
        }
        self.ensure_deps_installed().await?;

        info!("starting rivet sidecar on port {}...", self.port);
        let child = Command::new("npx")
            .args(["tsx", "server.ts"])
            .current_dir(&self.rivet_dir)
            .env("PORT", self.port.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        self.process = Some(child);

        // Wait briefly for the server to start
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

        // Health check
        let client = reqwest::Client::new();
        for attempt in 0..10 {
            match client
                .get(format!("http://localhost:{}", self.port))
                .send()
                .await
            {
                Ok(_) => {
                    info!("rivet sidecar is ready on port {}", self.port);
                    return Ok(());
                }
                Err(_) if attempt < 9 => {
                    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                }
                Err(e) => {
                    error!("rivet sidecar failed to start: {}", e);
                    self.stop().await;
                    return Err(anyhow!("rivet sidecar failed to start: {}", e));
                }
            }
        }
        Ok(())
    }

    pub async fn stop(&mut self) {
        if let Some(mut child) = self.process.take() {
            info!("stopping rivet sidecar...");
            let _ = child.kill().await;
        }
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn is_running(&self) -> bool {
        self.process.is_some()
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        if let Some(mut child) = self.process.take() {
            let _ = child.start_kill();
        }
    }
}
