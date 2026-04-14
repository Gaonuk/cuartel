use anyhow::{Result, anyhow};
use log::{error, info};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};

async fn pipe_lines<R: AsyncRead + Unpin + Send + 'static>(tag: &'static str, reader: R) {
    let mut lines = BufReader::new(reader).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => info!("[{tag}] {line}"),
            Ok(None) => break,
            Err(e) => {
                error!("[{tag}] pipe read error: {e}");
                break;
            }
        }
    }
}

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
        if self.rivet_dir.join("node_modules").exists() {
            return Ok(());
        }
        info!(
            "installing rivet sidecar dependencies in {}...",
            self.rivet_dir.display()
        );
        // Use `.output()` so both pipes are drained — otherwise a chatty npm
        // can fill the stderr buffer and deadlock `.status()`.
        let output = Command::new("npm")
            .arg("install")
            .current_dir(&self.rivet_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            error!(
                "npm install failed (exit {:?})\n--- stderr ---\n{}\n--- stdout ---\n{}",
                output.status.code(),
                stderr.trim(),
                stdout.trim(),
            );
            let snippet: String = stderr
                .lines()
                .rev()
                .take(3)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join(" | ");
            return Err(anyhow!(
                "npm install failed (exit {:?}): {}",
                output.status.code(),
                snippet
            ));
        }
        info!("npm install completed");
        Ok(())
    }

    pub async fn start(&mut self) -> Result<()> {
        if self.process.is_some() {
            return Ok(());
        }
        self.ensure_deps_installed().await?;

        info!("starting rivet sidecar on port {}...", self.port);
        let mut child = Command::new("npx")
            .args(["tsx", "server.ts"])
            .current_dir(&self.rivet_dir)
            .env("PORT", self.port.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        if let Some(stdout) = child.stdout.take() {
            tokio::spawn(pipe_lines("rivet", stdout));
        }
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(pipe_lines("rivet!", stderr));
        }

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
