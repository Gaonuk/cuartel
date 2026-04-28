//! Live integration test against `claude-code-acp` running as a host
//! subprocess. Validates the v2-doc Phase B1 DoD bullet:
//!
//! > cuartel-acp connects to claude-code-acp (subprocess, stdio
//! > transport) and completes one full turn.
//!
//! Costs ~$0.01-0.02 in API spend per run. Skipped automatically when:
//!   - `npx` isn't on PATH, or
//!   - the env var `CUARTEL_ACP_LIVE_TEST=1` is not set.
//!
//! Run with:
//!   CUARTEL_ACP_LIVE_TEST=1 cargo test -p cuartel-acp --test live_claude_code_acp -- --nocapture

use std::sync::Arc;

use cuartel_acp::{AcpClient, AcpClientOptions, NoOpClientHandler, SessionEvent};
use cuartel_acp::transport::SpawnOptions;

const ENABLE_ENV: &str = "CUARTEL_ACP_LIVE_TEST";

fn live_test_enabled() -> bool {
    std::env::var(ENABLE_ENV).is_ok_and(|v| !v.is_empty() && v != "0")
}

fn npx_available() -> bool {
    std::process::Command::new("which")
        .arg("npx")
        .output()
        .ok()
        .is_some_and(|o| o.status.success())
}

#[tokio::test]
async fn one_full_turn_against_claude_code_acp() {
    if !live_test_enabled() {
        eprintln!("skipping live test: set {ENABLE_ENV}=1 to enable");
        return;
    }
    if !npx_available() {
        eprintln!("skipping live test: npx not on PATH");
        return;
    }

    let cwd = std::env::current_dir().expect("cwd");
    let opts = AcpClientOptions {
        spawn: SpawnOptions::claude_code_acp(cwd.clone()),
        handler: Arc::new(NoOpClientHandler),
    };

    let started_at = std::time::Instant::now();
    let client = AcpClient::connect(opts)
        .await
        .expect("AcpClient::connect should succeed");

    eprintln!(
        "connected in {:.2}s; capabilities = loadSession={}",
        started_at.elapsed().as_secs_f32(),
        client.capabilities().load_session,
    );

    // The v2-doc Phase A1 spike confirmed loadSession is true; sanity-check
    // it stayed true.
    assert!(
        client.capabilities().load_session,
        "expected loadSession capability"
    );

    let session = client
        .new_session(cwd.clone())
        .await
        .expect("new_session should succeed");
    eprintln!("session {} created", session.id);

    // Cheap prompt: zero tool calls, deterministic answer, fast.
    let mut events = client
        .prompt(&session, "Reply with only the word OK.".into())
        .await
        .expect("prompt should succeed");

    let mut got_message = String::new();
    let mut stop_reason = None;
    while let Some(ev) = events.recv().await {
        match ev {
            SessionEvent::AgentMessageChunk { text } => got_message.push_str(&text),
            SessionEvent::TurnComplete { stop_reason: r } => {
                stop_reason = Some(r);
                break;
            }
            SessionEvent::Error { message } => {
                panic!("agent returned error: {message}");
            }
            _ => {}
        }
    }

    eprintln!(
        "turn finished in {:.2}s; stop_reason={:?}, message={:?}",
        started_at.elapsed().as_secs_f32(),
        stop_reason,
        got_message,
    );

    assert_eq!(
        stop_reason.as_deref(),
        Some("end_turn"),
        "expected end_turn stop reason"
    );
    assert!(
        got_message.to_lowercase().contains("ok"),
        "expected response to contain 'OK', got: {got_message:?}"
    );

    client.dispose().await;
    eprintln!("disposed cleanly");
}
