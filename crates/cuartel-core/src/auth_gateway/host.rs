//! Lifecycle wrapper around the auth gateway proxy.
//!
//! Mirrors the shape of `SidecarHost` in `cuartel-app` so the app layer can
//! orchestrate both the same way: spawn on a shared tokio runtime, expose a
//! status `Arc<Mutex<_>>`, and hand back accessors the UI can poll.
//!
//! `GatewayHost` intentionally does **not** own a runtime — it accepts a
//! `tokio::runtime::Handle` from the caller (typically
//! `SidecarHost::runtime_handle()`). Running both the sidecar and the
//! gateway on the same runtime keeps the process to a single worker pool.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tokio::runtime::Handle;
use tokio::sync::broadcast;

use crate::credential_store::CredentialStore;

use super::audit::{AuditEvent, AuditSender, DEFAULT_AUDIT_BUFFER};
use super::proxy;
use super::rules::AuthGatewayConfig;

/// Lifecycle state of the gateway. Mirrors `SidecarStatus`'s shape so the
/// UI can render both with the same template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayStatus {
    Idle,
    Starting,
    Ready(SocketAddr),
    Failed(String),
}

/// Handle the app holds onto for the lifetime of the gateway.
///
/// Drop semantics: dropping `GatewayHost` does **not** stop the accept
/// loop, because the loop runs on the shared tokio runtime. If you need to
/// tear the gateway down, take a `JoinHandle` from `spawn_on` instead.
/// For the app's needs (spawn once at startup, live for the whole
/// process), no explicit shutdown is required — `Box::leak` at the call
/// site is the expected pattern.
pub struct GatewayHost {
    status: Arc<Mutex<GatewayStatus>>,
    bind_addr: Arc<Mutex<Option<SocketAddr>>>,
    audit_tx: AuditSender,
}

impl GatewayHost {
    /// Spawn the gateway onto `rt` and return immediately. The accept loop
    /// runs as a detached task; status transitions reflect its progress.
    ///
    /// The returned host owns the `broadcast::Sender` half of the audit
    /// channel. Subscribers must call `subscribe_audit()` before an event
    /// is sent to receive it — the channel does not replay past events.
    pub fn spawn(
        rt: Handle,
        creds: Arc<dyn CredentialStore>,
        config: AuthGatewayConfig,
    ) -> Self {
        let (audit_tx, _) = broadcast::channel::<AuditEvent>(DEFAULT_AUDIT_BUFFER);
        let status = Arc::new(Mutex::new(GatewayStatus::Idle));
        let bind_addr = Arc::new(Mutex::new(None));

        let status_bg = Arc::clone(&status);
        let bind_bg = Arc::clone(&bind_addr);
        let audit_bg = audit_tx.clone();
        rt.spawn(async move {
            *status_bg.lock().unwrap() = GatewayStatus::Starting;
            match proxy::bind(config, creds, Some(audit_bg)).await {
                Ok((addr, serve)) => {
                    *bind_bg.lock().unwrap() = Some(addr);
                    *status_bg.lock().unwrap() = GatewayStatus::Ready(addr);
                    log::info!("auth gateway listening on {addr}");
                    serve.await;
                    // The accept loop never terminates on its own; landing
                    // here means the runtime is shutting down.
                }
                Err(e) => {
                    log::error!("auth gateway bind failed: {e:#}");
                    *status_bg.lock().unwrap() = GatewayStatus::Failed(format!("bind: {e}"));
                }
            }
        });

        Self {
            status,
            bind_addr,
            audit_tx,
        }
    }

    /// Current gateway status, cloned for the caller. Polled by the UI
    /// (e.g. a status dot in the sidebar).
    pub fn status(&self) -> GatewayStatus {
        self.status.lock().unwrap().clone()
    }

    /// Shared handle so UI views can observe status in place without
    /// copying on every frame.
    pub fn status_handle(&self) -> Arc<Mutex<GatewayStatus>> {
        Arc::clone(&self.status)
    }

    /// The address the gateway accepted on, once it is `Ready`. Returns
    /// `None` while the gateway is still starting up or if it failed.
    ///
    /// Used at app startup to assemble `ANTHROPIC_BASE_URL=http://{addr}`
    /// for the sidecar env before spawning the rivet child process.
    pub fn bind_addr(&self) -> Option<SocketAddr> {
        *self.bind_addr.lock().unwrap()
    }

    /// Block the current thread (briefly) until the gateway reaches
    /// `Ready` or `Failed`. Called from `cuartel-app/main.rs` at startup
    /// so the sidecar env can be assembled with the real bind port.
    ///
    /// `timeout` caps the wait; on timeout the call returns `None` and
    /// the caller can decide whether to proceed without the gateway
    /// (current path: log and carry on with a `None` base URL, which
    /// means the app falls back to direct credentials in sidecar env).
    pub fn wait_until_ready(&self, timeout: std::time::Duration) -> Option<SocketAddr> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match self.status() {
                GatewayStatus::Ready(addr) => return Some(addr),
                GatewayStatus::Failed(_) => return None,
                _ => {}
            }
            if std::time::Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }

    /// Subscribe to the audit event stream. Each subscriber gets its own
    /// receiver; events sent before `subscribe_audit()` is called are not
    /// replayed (broadcast channels are at-most-once from the
    /// subscriber's perspective).
    ///
    /// 5d attaches a subscriber here that persists events to SQLite.
    pub fn subscribe_audit(&self) -> broadcast::Receiver<AuditEvent> {
        self.audit_tx.subscribe()
    }

    /// Sender clone, for integration scenarios that want to emit events
    /// from outside the proxy itself (e.g. the app layer noting that the
    /// user manually forced a gateway restart).
    pub fn audit_sender(&self) -> AuditSender {
        self.audit_tx.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential_store::MemoryCredentialStore;

    use std::time::Duration;

    use super::super::rules::{AuthRule, MissPolicy};

    #[tokio::test(flavor = "multi_thread")]
    async fn gateway_host_becomes_ready_and_reports_bind_addr() {
        let rt = Handle::current();
        let store = Arc::new(MemoryCredentialStore::new());
        let config = AuthGatewayConfig {
            rules: vec![],
            bind: "127.0.0.1:0".parse().unwrap(),
            on_miss: MissPolicy::Reject,
        };
        let host = GatewayHost::spawn(rt, store, config);

        // Poll until ready — runs in the background on the same runtime.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            if matches!(host.status(), GatewayStatus::Ready(_)) {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("gateway never reached Ready: {:?}", host.status());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let addr = host.bind_addr().expect("bind_addr set at Ready");
        assert_eq!(addr.ip().to_string(), "127.0.0.1");
        assert_ne!(addr.port(), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn gateway_host_emits_blocked_event_on_unknown_host() {
        let rt = Handle::current();
        let store = Arc::new(MemoryCredentialStore::new());
        let config = AuthGatewayConfig {
            rules: vec![],
            bind: "127.0.0.1:0".parse().unwrap(),
            on_miss: MissPolicy::Reject,
        };
        let host = GatewayHost::spawn(rt, store, config);

        // Subscribe BEFORE the request so the event isn't lost.
        let mut rx = host.subscribe_audit();

        let addr = loop {
            match host.status() {
                GatewayStatus::Ready(a) => break a,
                GatewayStatus::Failed(e) => panic!("gateway failed: {e}"),
                _ => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        };

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(
                b"GET /v1/x HTTP/1.1\r\nHost: evil.example.com\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();

        let event = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("audit event not received")
            .expect("broadcast recv ok");
        match event {
            AuditEvent::Blocked { hostname, path, .. } => {
                assert_eq!(hostname, "evil.example.com");
                assert_eq!(path, "/v1/x");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn gateway_host_emits_credential_missing_event() {
        let rt = Handle::current();
        let store = Arc::new(MemoryCredentialStore::new());
        let config = AuthGatewayConfig {
            rules: vec![AuthRule {
                hostname: "api.anthropic.com".into(),
                provider_id: "anthropic".into(),
                env_key: "ANTHROPIC_API_KEY".into(),
                header_name: "x-api-key".into(),
                header_format: "{key}".into(),
                strip_headers: vec![],
                upstream_scheme: "https".into(),
                upstream_authority: None,
            }],
            bind: "127.0.0.1:0".parse().unwrap(),
            on_miss: MissPolicy::Reject,
        };
        let host = GatewayHost::spawn(rt, store, config);
        let mut rx = host.subscribe_audit();

        let addr = loop {
            match host.status() {
                GatewayStatus::Ready(a) => break a,
                GatewayStatus::Failed(e) => panic!("gateway failed: {e}"),
                _ => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        };

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(
                b"GET /v1/x HTTP/1.1\r\nHost: api.anthropic.com\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();

        let event = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("audit event not received")
            .expect("broadcast recv ok");
        match event {
            AuditEvent::CredentialMissing {
                provider_id,
                env_key,
                ..
            } => {
                assert_eq!(provider_id, "anthropic");
                assert_eq!(env_key, "ANTHROPIC_API_KEY");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }
}
