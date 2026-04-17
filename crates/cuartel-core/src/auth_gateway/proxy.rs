//! Reverse-proxy server for the auth gateway.
//!
//! Binds a loopback TCP listener, accepts HTTP/1.1 connections, and for each
//! request looks up a rule by `Host` header, swaps the dummy credential for
//! the real one from the credential store, and streams the request upstream
//! over TLS. Response bodies stream back unbuffered (SSE-safe).

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{anyhow, Context, Result};
use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt, Empty, Full};
use hyper::body::{Body, Incoming};
use hyper::header::{HeaderName, HeaderValue, HOST};
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode, Uri};
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::net::TcpListener;

use crate::credential_store::CredentialStore;

use super::audit::{AuditEvent, AuditSender};
use super::rules::{AuthGatewayConfig, AuthRule, MissPolicy};

/// Unified body type we forward upstream and stream back downstream.
///
/// Error type is a boxed `std::error::Error` so we can freely mix body
/// sources (hyper `Incoming` on the wire, `Full<Bytes>` for synthesized
/// error responses, arbitrary streams in tests) without fighting hyper's
/// unconstructible `hyper::Error`.
pub type ProxyBodyError = Box<dyn std::error::Error + Send + Sync>;
pub type ProxyBody = BoxBody<Bytes, ProxyBodyError>;

type UpstreamClient = Client<hyper_rustls::HttpsConnector<HttpConnector>, ProxyBody>;

/// Start serving the gateway on `config.bind`. Returns the actual bound
/// address (important when `bind.port() == 0`) and a future that drives the
/// accept loop. Caller owns the future and decides when to drop it.
///
/// `audit` is optional: when `Some`, the proxy emits an `AuditEvent` on
/// every request outcome. Dropped events (no subscribers, or full buffer)
/// are silently ignored — audit is a diagnostic aid, not a load-bearing
/// control.
pub async fn bind(
    config: AuthGatewayConfig,
    creds: Arc<dyn CredentialStore>,
    audit: Option<AuditSender>,
) -> Result<(SocketAddr, impl std::future::Future<Output = ()>)> {
    let listener = TcpListener::bind(config.bind)
        .await
        .with_context(|| format!("bind auth gateway to {}", config.bind))?;
    let local = listener.local_addr()?;

    let upstream: UpstreamClient = Client::builder(TokioExecutor::new()).build(
        hyper_rustls::HttpsConnectorBuilder::new()
            .with_native_roots()
            .context("load native root certificates")?
            .https_or_http()
            .enable_http1()
            .build(),
    );

    let state = Arc::new(ProxyState {
        config,
        creds,
        upstream,
        audit,
    });

    let future = async move {
        loop {
            let (stream, peer) = match listener.accept().await {
                Ok(v) => v,
                Err(e) => {
                    log::warn!("auth gateway accept error: {e}");
                    continue;
                }
            };
            let io = TokioIo::new(stream);
            let state = Arc::clone(&state);
            tokio::spawn(async move {
                let service = service_fn(move |req| {
                    let state = Arc::clone(&state);
                    let peer_ip = peer.ip();
                    async move { Ok::<_, Infallible>(handle(state, req, peer_ip).await) }
                });
                if let Err(e) = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, service)
                    .await
                {
                    log::debug!("auth gateway connection closed: {e}");
                }
            });
        }
    };

    Ok((local, future))
}

struct ProxyState {
    config: AuthGatewayConfig,
    creds: Arc<dyn CredentialStore>,
    upstream: UpstreamClient,
    audit: Option<AuditSender>,
}

impl ProxyState {
    fn emit(&self, event: AuditEvent) {
        if let Some(tx) = &self.audit {
            let _ = tx.send(event);
        }
    }
}

async fn handle(
    state: Arc<ProxyState>,
    req: Request<Incoming>,
    peer_ip: std::net::IpAddr,
) -> Response<ProxyBody> {
    match handle_inner(state, req, peer_ip).await {
        Ok(resp) => resp,
        Err(e) => {
            log::warn!("auth gateway request failed: {e:#}");
            error_response(StatusCode::BAD_GATEWAY, &format!("gateway error: {e}"))
        }
    }
}

async fn handle_inner(
    state: Arc<ProxyState>,
    req: Request<Incoming>,
    peer_ip: std::net::IpAddr,
) -> Result<Response<ProxyBody>> {
    let method = req.method().as_str().to_string();
    let path = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "/".into());
    let hostname = request_hostname(&req).ok_or_else(|| anyhow!("missing Host header"))?;

    let (rule_clone, policy) = match state.config.match_host(&hostname) {
        Some(rule) => (Some(rule.clone()), state.config.on_miss),
        None => (None, state.config.on_miss),
    };

    let Some(rule) = rule_clone else {
        return match policy {
            MissPolicy::Reject => {
                state.emit(AuditEvent::Blocked {
                    timestamp: SystemTime::now(),
                    client_ip: Some(peer_ip),
                    hostname: hostname.clone(),
                    method: method.clone(),
                    path: path.clone(),
                    reason: "no rule for host".into(),
                });
                Ok(error_response(
                    StatusCode::BAD_GATEWAY,
                    &format!("no auth gateway rule for host {hostname}"),
                ))
            }
            MissPolicy::Passthrough => passthrough(&state.upstream, req, &hostname).await,
        };
    };

    let credential = match state.creds.get(&rule.provider_id, &rule.env_key) {
        Ok(Some(v)) if !v.is_empty() => v,
        Ok(_) => {
            state.emit(AuditEvent::CredentialMissing {
                timestamp: SystemTime::now(),
                hostname: hostname.clone(),
                provider_id: rule.provider_id.clone(),
                env_key: rule.env_key.clone(),
            });
            return Ok(error_response(
                StatusCode::BAD_GATEWAY,
                &format!(
                    "credential {}:{} not configured",
                    rule.provider_id, rule.env_key
                ),
            ));
        }
        Err(e) => {
            state.emit(AuditEvent::CredentialMissing {
                timestamp: SystemTime::now(),
                hostname: hostname.clone(),
                provider_id: rule.provider_id.clone(),
                env_key: rule.env_key.clone(),
            });
            return Ok(error_response(
                StatusCode::BAD_GATEWAY,
                &format!("credential lookup failed: {e}"),
            ));
        }
    };

    let upstream_req = build_upstream_request(req, &rule, &credential, &hostname)?;
    let resp = match state.upstream.request(upstream_req).await {
        Ok(r) => r,
        Err(e) => {
            state.emit(AuditEvent::UpstreamError {
                timestamp: SystemTime::now(),
                hostname: hostname.clone(),
                provider_id: rule.provider_id.clone(),
                error: e.to_string(),
            });
            return Err(anyhow!("upstream request to {hostname}: {e}"));
        }
    };

    let status = resp.status().as_u16();
    state.emit(AuditEvent::Injected {
        timestamp: SystemTime::now(),
        client_ip: Some(peer_ip),
        hostname: hostname.clone(),
        provider_id: rule.provider_id.clone(),
        env_key: rule.env_key.clone(),
        method,
        path,
        status,
    });
    Ok(box_response(resp))
}

async fn passthrough(
    client: &UpstreamClient,
    incoming: Request<Incoming>,
    hostname: &str,
) -> Result<Response<ProxyBody>> {
    let (mut parts, body) = incoming.into_parts();
    let path_and_query = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/");
    parts.uri = format!("https://{hostname}{path_and_query}")
        .parse()
        .with_context(|| format!("build passthrough uri for {hostname}"))?;
    parts
        .headers
        .insert(HOST, HeaderValue::from_str(hostname).context("set Host")?);
    let req = Request::from_parts(parts, box_incoming(body));
    let resp = client.request(req).await.context("passthrough request")?;
    Ok(box_response(resp))
}

/// Strip forbidden headers, inject the credential, rewrite Host and URI.
///
/// Generic over the body type so unit tests can exercise this against a
/// concrete `Empty<Bytes>` without needing to construct an `Incoming`
/// (which hyper 1 does not expose a constructor for).
pub(crate) fn build_upstream_request<B>(
    incoming: Request<B>,
    rule: &AuthRule,
    credential: &str,
    hostname: &str,
) -> Result<Request<ProxyBody>>
where
    B: Body<Data = Bytes> + Send + Sync + 'static,
    B::Error: Into<ProxyBodyError>,
{
    let (mut parts, body) = incoming.into_parts();

    let path_and_query = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/");
    let authority = rule
        .upstream_authority
        .as_deref()
        .unwrap_or(hostname);
    parts.uri = format!("{}://{authority}{path_and_query}", rule.upstream_scheme)
        .parse::<Uri>()
        .with_context(|| format!("build upstream uri for {hostname}"))?;

    let strip_lc: Vec<String> = rule
        .strip_headers
        .iter()
        .map(|h| h.to_ascii_lowercase())
        .collect();
    let to_remove: Vec<HeaderName> = parts
        .headers
        .keys()
        .filter(|name| strip_lc.iter().any(|s| s == &name.as_str().to_ascii_lowercase()))
        .cloned()
        .collect();
    for name in to_remove {
        parts.headers.remove(&name);
    }

    let header_name: HeaderName = rule
        .header_name
        .parse()
        .with_context(|| format!("invalid header name {}", rule.header_name))?;
    let header_value = HeaderValue::from_str(&rule.render_header_value(credential))
        .context("render credential into header value")?;
    parts.headers.insert(header_name, header_value);
    parts
        .headers
        .insert(HOST, HeaderValue::from_str(hostname).context("set Host")?);

    let boxed = BodyExt::map_err(body, |e| e.into()).boxed();
    Ok(Request::from_parts(parts, boxed))
}

fn box_incoming(body: Incoming) -> ProxyBody {
    BodyExt::map_err(body, |e| -> ProxyBodyError { Box::new(e) }).boxed()
}

fn box_response(resp: Response<Incoming>) -> Response<ProxyBody> {
    let (parts, body) = resp.into_parts();
    Response::from_parts(parts, BodyExt::map_err(body, |e| -> ProxyBodyError { Box::new(e) }).boxed())
}

fn request_hostname<B>(req: &Request<B>) -> Option<String> {
    if let Some(host) = req.headers().get(HOST).and_then(|v| v.to_str().ok()) {
        return Some(strip_port(host).to_string());
    }
    req.uri().host().map(|h| h.to_string())
}

fn strip_port(host: &str) -> &str {
    if host.starts_with('[') {
        return host;
    }
    match host.rfind(':') {
        Some(idx) => &host[..idx],
        None => host,
    }
}

fn error_response(status: StatusCode, msg: &str) -> Response<ProxyBody> {
    let body = format!("{{\"error\":{}}}", serde_json::to_string(msg).unwrap());
    let boxed = Full::<Bytes>::new(Bytes::from(body))
        .map_err(|never| -> ProxyBodyError { match never {} })
        .boxed();
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(boxed)
        .expect("static response always builds")
}

#[allow(dead_code)]
fn boxed_empty() -> ProxyBody {
    Empty::<Bytes>::new()
        .map_err(|never| -> ProxyBodyError { match never {} })
        .boxed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential_store::MemoryCredentialStore;

    use http_body_util::Empty;
    use hyper::Method;

    use super::super::rules::{AuthGatewayConfig, AuthRule, MissPolicy};

    fn empty_body() -> Empty<Bytes> {
        Empty::<Bytes>::new()
    }

    #[test]
    fn build_upstream_request_strips_and_injects() {
        let rule = AuthRule {
            hostname: "api.anthropic.com".into(),
            provider_id: "anthropic".into(),
            env_key: "ANTHROPIC_API_KEY".into(),
            header_name: "x-api-key".into(),
            header_format: "{key}".into(),
            strip_headers: vec!["authorization".into(), "x-api-key".into()],
            upstream_scheme: "https".into(),
            upstream_authority: None,
        };
        let req = Request::builder()
            .method(Method::POST)
            .uri("http://api.anthropic.com/v1/messages?beta=true")
            .header("host", "api.anthropic.com")
            .header("authorization", "Bearer sk-cuartel-gateway")
            .header("x-api-key", "sk-cuartel-gateway")
            .header("content-type", "application/json")
            .body(empty_body())
            .unwrap();

        let upstream =
            build_upstream_request(req, &rule, "sk-real-secret", "api.anthropic.com").unwrap();

        assert_eq!(
            upstream.uri().to_string(),
            "https://api.anthropic.com/v1/messages?beta=true"
        );
        assert_eq!(
            upstream.headers().get("x-api-key").unwrap(),
            "sk-real-secret"
        );
        assert!(upstream.headers().get("authorization").is_none());
        assert_eq!(upstream.headers().get("host").unwrap(), "api.anthropic.com");
        assert_eq!(
            upstream.headers().get("content-type").unwrap(),
            "application/json"
        );
    }

    #[test]
    fn build_upstream_request_formats_bearer_for_openai() {
        let rule = AuthRule {
            hostname: "api.openai.com".into(),
            provider_id: "openai".into(),
            env_key: "OPENAI_API_KEY".into(),
            header_name: "Authorization".into(),
            header_format: "Bearer {key}".into(),
            strip_headers: vec!["authorization".into(), "x-api-key".into()],
            upstream_scheme: "https".into(),
            upstream_authority: None,
        };
        let req = Request::builder()
            .uri("http://api.openai.com/v1/chat/completions")
            .header("host", "api.openai.com")
            .header("authorization", "Bearer sk-cuartel-gateway")
            .body(empty_body())
            .unwrap();

        let upstream = build_upstream_request(req, &rule, "sk-real", "api.openai.com").unwrap();
        assert_eq!(
            upstream.headers().get("authorization").unwrap(),
            "Bearer sk-real"
        );
    }

    #[test]
    fn strip_port_handles_ipv4_ipv6_and_no_port() {
        assert_eq!(strip_port("api.anthropic.com"), "api.anthropic.com");
        assert_eq!(strip_port("api.anthropic.com:443"), "api.anthropic.com");
        assert_eq!(strip_port("[::1]:8080"), "[::1]:8080");
    }

    /// End-to-end accept-loop test: start the proxy, hit it with a
    /// `Host: evil.example.com` request, and assert we get a 502 with the
    /// `no auth gateway rule` message. This covers the server wiring
    /// (TcpListener + hyper::server::conn) without needing TLS.
    #[tokio::test]
    async fn rejects_unknown_host_with_502() {
        let store = Arc::new(MemoryCredentialStore::new());
        let config = AuthGatewayConfig {
            rules: vec![],
            bind: "127.0.0.1:0".parse().unwrap(),
            on_miss: MissPolicy::Reject,
        };
        let (addr, fut) = bind(config, store, None).await.unwrap();
        let handle = tokio::spawn(fut);

        let response = raw_get(addr, "evil.example.com", "/").await.unwrap();
        assert!(response.starts_with("HTTP/1.1 502"), "got: {response}");
        assert!(response.contains("no auth gateway rule"));

        handle.abort();
    }

    #[tokio::test]
    async fn rejects_matched_host_when_credential_missing() {
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
        let (addr, fut) = bind(config, store, None).await.unwrap();
        let handle = tokio::spawn(fut);

        let response = raw_get(addr, "api.anthropic.com", "/v1/x").await.unwrap();
        assert!(response.starts_with("HTTP/1.1 502"), "got: {response}");
        assert!(response.contains("credential anthropic:ANTHROPIC_API_KEY not configured"));

        handle.abort();
    }

    /// Minimal HTTP/1.1 client: opens a TCP socket, writes a request with
    /// the given Host header, reads the full response into a String.
    async fn raw_get(
        addr: std::net::SocketAddr,
        host: &str,
        path: &str,
    ) -> anyhow::Result<String> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let mut stream = tokio::net::TcpStream::connect(addr).await?;
        let req =
            format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
        stream.write_all(req.as_bytes()).await?;
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await?;
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }

    /// End-to-end: agent → proxy → fake upstream, proves the real key
    /// lands upstream and the dummy never does, and that the body streams
    /// back in chunks (SSE-safe).
    #[tokio::test]
    async fn injects_real_key_and_streams_chunked_response() {
        // 1. Fake upstream: records the x-api-key it saw, streams back a
        //    chunked SSE-style response.
        let captured: Arc<tokio::sync::Mutex<Option<String>>> =
            Arc::new(tokio::sync::Mutex::new(None));
        let upstream_addr = spawn_fake_upstream(Arc::clone(&captured)).await;

        // 2. Credential store seeded with the real key.
        let store = Arc::new(MemoryCredentialStore::new());
        store
            .set("anthropic", "ANTHROPIC_API_KEY", "sk-real-secret")
            .unwrap();

        // 3. Rule points `api.anthropic.com` at 127.0.0.1:<fake> over http.
        let rule = AuthRule {
            hostname: "api.anthropic.com".into(),
            provider_id: "anthropic".into(),
            env_key: "ANTHROPIC_API_KEY".into(),
            header_name: "x-api-key".into(),
            header_format: "{key}".into(),
            strip_headers: vec!["authorization".into(), "x-api-key".into()],
            upstream_scheme: "http".into(),
            upstream_authority: Some(upstream_addr.to_string()),
        };
        let config = AuthGatewayConfig {
            rules: vec![rule],
            bind: "127.0.0.1:0".parse().unwrap(),
            on_miss: MissPolicy::Reject,
        };
        let (gateway_addr, fut) = bind(config, store, None).await.unwrap();
        let gateway_handle = tokio::spawn(fut);

        // 4. Client hits the gateway with the dummy key.
        let response = raw_request_with_dummy(
            gateway_addr,
            "api.anthropic.com",
            "/v1/messages",
            "sk-cuartel-gateway",
        )
        .await
        .unwrap();

        // 5. Fake upstream received the real key, not the dummy.
        let saw = captured.lock().await.clone();
        assert_eq!(saw.as_deref(), Some("sk-real-secret"));

        // 6. Client received the chunked body verbatim.
        assert!(response.contains("data: chunk-1"));
        assert!(response.contains("data: chunk-2"));
        assert!(response.contains("data: [DONE]"));

        gateway_handle.abort();
    }

    async fn raw_request_with_dummy(
        addr: std::net::SocketAddr,
        host: &str,
        path: &str,
        dummy_key: &str,
    ) -> anyhow::Result<String> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::TcpStream::connect(addr).await?;
        let req = format!(
            "GET {path} HTTP/1.1\r\n\
             Host: {host}\r\n\
             x-api-key: {dummy_key}\r\n\
             Authorization: Bearer {dummy_key}\r\n\
             Connection: close\r\n\r\n"
        );
        stream.write_all(req.as_bytes()).await?;
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await?;
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }

    /// Fake upstream: captures `x-api-key` into `sink`, streams back three
    /// SSE chunks separated by a small delay so we exercise the chunked
    /// body path rather than a single buffered response.
    async fn spawn_fake_upstream(
        sink: Arc<tokio::sync::Mutex<Option<String>>>,
    ) -> std::net::SocketAddr {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => break,
                };
                let sink = Arc::clone(&sink);
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    for line in req.lines() {
                        if let Some(rest) = line.to_ascii_lowercase().strip_prefix("x-api-key:") {
                            *sink.lock().await = Some(rest.trim().to_string());
                        }
                    }

                    let headers = "HTTP/1.1 200 OK\r\n\
                                   Content-Type: text/event-stream\r\n\
                                   Transfer-Encoding: chunked\r\n\
                                   Connection: close\r\n\r\n";
                    let _ = stream.write_all(headers.as_bytes()).await;
                    for body in ["data: chunk-1\n\n", "data: chunk-2\n\n", "data: [DONE]\n\n"]
                    {
                        let chunk = format!("{:x}\r\n{}\r\n", body.len(), body);
                        let _ = stream.write_all(chunk.as_bytes()).await;
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    }
                    let _ = stream.write_all(b"0\r\n\r\n").await;
                });
            }
        });
        addr
    }
}
