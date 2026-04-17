//! Audit-event persister (phase 5d).
//!
//! The gateway emits `AuditEvent`s into a `tokio::sync::broadcast` channel;
//! this module owns a subscriber that drains the channel and writes each
//! event to SQLite via a pluggable `AuditSink`.
//!
//! Design notes:
//!
//! * The SQLite insert happens inside `spawn_blocking` so we never hold the
//!   tokio worker during a lock+write. At human-driven request rates this
//!   is massively over-provisioned, but it keeps the persister honest if
//!   the table ever gets hot (e.g. when Phase 5f's firewall produces
//!   Blocked events on every denied connection).
//! * Broadcast receivers surface `Lagged(n)` when the subscriber falls
//!   behind the channel capacity. We log a warning with the dropped count
//!   and keep going — audit is diagnostic, so a hole in the record is
//!   preferable to either back-pressuring the gateway or crashing.
//! * `AuditSink` is a trait so tests can exercise the drain loop without a
//!   real DB, and so future sinks (e.g. an off-app log file) can slot in
//!   without touching the gateway plumbing.

use std::sync::{Arc, Mutex as StdMutex};
use std::time::SystemTime;

use anyhow::Result;
use chrono::{DateTime, SecondsFormat, Utc};
use tokio::runtime::Handle;
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::task::JoinHandle;

use cuartel_db::audit::{AuditEventInput, AuditRepo};
use cuartel_db::Database;

use super::audit::AuditEvent;

/// Anything that can persist an `AuditEvent`. A blocking method because the
/// real impl hits SQLite; the persister wraps each call in `spawn_blocking`.
pub trait AuditSink: Send + Sync + 'static {
    fn persist(&self, event: &AuditEvent) -> Result<()>;
}

/// `AuditSink` backed by the shared app `Database`.
///
/// The `Database` handle is wrapped in a `StdMutex` by the app (rusqlite's
/// `Connection` is `Send` but not `Sync`), so we take that same guard shape
/// here. Each call grabs the lock, constructs an `AuditRepo`, and inserts.
pub struct DatabaseAuditSink {
    db: Arc<StdMutex<Database>>,
}

impl DatabaseAuditSink {
    pub fn new(db: Arc<StdMutex<Database>>) -> Self {
        Self { db }
    }
}

impl AuditSink for DatabaseAuditSink {
    fn persist(&self, event: &AuditEvent) -> Result<()> {
        // Own every string the input borrows so the slices stay valid for
        // the scope of `insert`. Only timestamp and client_ip require
        // formatting — the rest already live as `String` on the event and
        // can be borrowed as `&str`.
        let timestamp = format_system_time(event_timestamp(event));
        let client_ip_owned = event_client_ip(event).map(|ip| ip.to_string());
        let input = build_input(event, &timestamp, client_ip_owned.as_deref());

        let guard = self
            .db
            .lock()
            .map_err(|_| anyhow::anyhow!("audit db mutex poisoned"))?;
        let repo = AuditRepo::new(&guard);
        repo.insert(&input)?;
        Ok(())
    }
}

/// Spawn the drain loop onto `rt` and return its `JoinHandle`.
///
/// The loop terminates when the broadcast sender half is dropped (i.e. the
/// `GatewayHost` is gone). Dropping the returned handle does NOT cancel the
/// loop — if the app wants an explicit shutdown, it must `abort()` the
/// handle or drop the `GatewayHost`.
pub fn spawn_audit_persister(
    rt: &Handle,
    mut rx: broadcast::Receiver<AuditEvent>,
    sink: Arc<dyn AuditSink>,
) -> JoinHandle<()> {
    rt.spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let sink = Arc::clone(&sink);
                    let event_for_task = event.clone();
                    let res =
                        tokio::task::spawn_blocking(move || sink.persist(&event_for_task)).await;
                    match res {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => {
                            log::warn!(
                                "audit persister: failed to write {} event: {e:#}",
                                event.kind()
                            );
                        }
                        Err(join_err) => {
                            log::warn!("audit persister: blocking task panicked: {join_err}");
                        }
                    }
                }
                Err(RecvError::Lagged(n)) => {
                    log::warn!("audit persister lagged — dropped {n} event(s) before catching up");
                }
                Err(RecvError::Closed) => {
                    log::debug!("audit persister: channel closed, exiting");
                    return;
                }
            }
        }
    })
}

/// Build the flat DB input from an event, given pre-owned strings for the
/// two fields that need formatting. `kind()` on `AuditEvent` returns the
/// exact discriminant strings the schema uses, so we lean on it instead of
/// duplicating literals here.
fn build_input<'a>(
    event: &'a AuditEvent,
    timestamp: &'a str,
    client_ip: Option<&'a str>,
) -> AuditEventInput<'a> {
    let kind = event.kind();
    match event {
        AuditEvent::Injected {
            hostname,
            provider_id,
            env_key,
            method,
            path,
            status,
            ..
        } => AuditEventInput {
            kind,
            timestamp,
            hostname,
            provider_id: Some(provider_id),
            env_key: Some(env_key),
            method: Some(method),
            path: Some(path),
            status: Some(*status),
            client_ip,
            reason: None,
            error: None,
        },
        AuditEvent::Blocked {
            hostname,
            method,
            path,
            reason,
            ..
        } => AuditEventInput {
            kind,
            timestamp,
            hostname,
            provider_id: None,
            env_key: None,
            method: Some(method),
            path: Some(path),
            status: None,
            client_ip,
            reason: Some(reason),
            error: None,
        },
        AuditEvent::CredentialMissing {
            hostname,
            provider_id,
            env_key,
            ..
        } => AuditEventInput {
            kind,
            timestamp,
            hostname,
            provider_id: Some(provider_id),
            env_key: Some(env_key),
            method: None,
            path: None,
            status: None,
            client_ip: None,
            reason: None,
            error: None,
        },
        AuditEvent::UpstreamError {
            hostname,
            provider_id,
            error,
            ..
        } => AuditEventInput {
            kind,
            timestamp,
            hostname,
            provider_id: Some(provider_id),
            env_key: None,
            method: None,
            path: None,
            status: None,
            client_ip: None,
            reason: None,
            error: Some(error),
        },
    }
}

fn event_timestamp(event: &AuditEvent) -> SystemTime {
    match event {
        AuditEvent::Injected { timestamp, .. }
        | AuditEvent::Blocked { timestamp, .. }
        | AuditEvent::CredentialMissing { timestamp, .. }
        | AuditEvent::UpstreamError { timestamp, .. } => *timestamp,
    }
}

fn event_client_ip(event: &AuditEvent) -> Option<std::net::IpAddr> {
    match event {
        AuditEvent::Injected { client_ip, .. } | AuditEvent::Blocked { client_ip, .. } => {
            *client_ip
        }
        _ => None,
    }
}

fn format_system_time(ts: SystemTime) -> String {
    let dt: DateTime<Utc> = ts.into();
    dt.to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::net::Ipv4Addr;
    use std::sync::Mutex;
    use std::time::Duration;
    use std::time::UNIX_EPOCH;

    #[derive(Default)]
    struct RecordingSink {
        events: Mutex<Vec<AuditEvent>>,
    }

    impl AuditSink for RecordingSink {
        fn persist(&self, event: &AuditEvent) -> Result<()> {
            self.events.lock().unwrap().push(event.clone());
            Ok(())
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn drains_all_events_in_order() {
        let (tx, rx) = broadcast::channel::<AuditEvent>(16);
        let sink = Arc::new(RecordingSink::default());
        let _handle =
            spawn_audit_persister(&Handle::current(), rx, sink.clone() as Arc<dyn AuditSink>);

        for i in 0..3 {
            tx.send(AuditEvent::Injected {
                timestamp: UNIX_EPOCH + Duration::from_secs(1_700_000_000 + i),
                client_ip: Some(Ipv4Addr::LOCALHOST.into()),
                hostname: format!("host-{i}"),
                provider_id: "anthropic".into(),
                env_key: "ANTHROPIC_API_KEY".into(),
                method: "POST".into(),
                path: "/v1/messages".into(),
                status: 200,
            })
            .unwrap();
        }

        // Poll with a deadline — the drain loop runs concurrently so we
        // can't synchronously observe writes.
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        loop {
            if sink.events.lock().unwrap().len() == 3 {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!(
                    "persister did not drain 3 events in time; got {}",
                    sink.events.lock().unwrap().len()
                );
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let events = sink.events.lock().unwrap().clone();
        match &events[0] {
            AuditEvent::Injected { hostname, .. } => assert_eq!(hostname, "host-0"),
            other => panic!("unexpected event 0: {other:?}"),
        }
        match &events[2] {
            AuditEvent::Injected { hostname, .. } => assert_eq!(hostname, "host-2"),
            other => panic!("unexpected event 2: {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn exits_when_sender_dropped() {
        let (tx, rx) = broadcast::channel::<AuditEvent>(4);
        let sink = Arc::new(RecordingSink::default());
        let handle =
            spawn_audit_persister(&Handle::current(), rx, sink.clone() as Arc<dyn AuditSink>);

        drop(tx);

        // Should complete quickly since the channel is closed.
        tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("persister task did not exit after sender drop")
            .expect("persister task panicked");
    }

    #[test]
    fn database_sink_writes_every_variant() {
        let db = Database::open_in_memory().unwrap();
        let sink = DatabaseAuditSink::new(Arc::new(StdMutex::new(db)));

        sink.persist(&AuditEvent::Injected {
            timestamp: UNIX_EPOCH + Duration::from_secs(1_700_000_000),
            client_ip: Some(Ipv4Addr::new(192, 168, 1, 1).into()),
            hostname: "api.anthropic.com".into(),
            provider_id: "anthropic".into(),
            env_key: "ANTHROPIC_API_KEY".into(),
            method: "POST".into(),
            path: "/v1/messages".into(),
            status: 200,
        })
        .unwrap();
        sink.persist(&AuditEvent::Blocked {
            timestamp: UNIX_EPOCH + Duration::from_secs(1_700_000_001),
            client_ip: None,
            hostname: "evil.example.com".into(),
            method: "GET".into(),
            path: "/".into(),
            reason: "no rule for host".into(),
        })
        .unwrap();
        sink.persist(&AuditEvent::CredentialMissing {
            timestamp: UNIX_EPOCH + Duration::from_secs(1_700_000_002),
            hostname: "api.anthropic.com".into(),
            provider_id: "anthropic".into(),
            env_key: "ANTHROPIC_API_KEY".into(),
        })
        .unwrap();
        sink.persist(&AuditEvent::UpstreamError {
            timestamp: UNIX_EPOCH + Duration::from_secs(1_700_000_003),
            hostname: "api.anthropic.com".into(),
            provider_id: "anthropic".into(),
            error: "connection reset".into(),
        })
        .unwrap();

        let guard = sink.db.lock().unwrap();
        let repo = AuditRepo::new(&guard);
        assert_eq!(repo.count().unwrap(), 4);

        let injected = repo.list_by_kind("injected", 10).unwrap();
        assert_eq!(injected.len(), 1);
        assert_eq!(injected[0].status, Some(200));
        assert_eq!(injected[0].client_ip.as_deref(), Some("192.168.1.1"));

        let blocked = repo.list_by_kind("blocked", 10).unwrap();
        assert_eq!(blocked.len(), 1);
        assert_eq!(blocked[0].reason.as_deref(), Some("no rule for host"));
        assert!(blocked[0].provider_id.is_none());

        let missing = repo.list_by_kind("credential_missing", 10).unwrap();
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].provider_id.as_deref(), Some("anthropic"));
        assert!(missing[0].method.is_none());

        let err = repo.list_by_kind("upstream_error", 10).unwrap();
        assert_eq!(err.len(), 1);
        assert_eq!(err[0].error.as_deref(), Some("connection reset"));
    }

    #[test]
    fn timestamp_is_rfc3339_millis_utc() {
        let ts = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        assert_eq!(format_system_time(ts), "2023-11-14T22:13:20.000Z");
    }
}
