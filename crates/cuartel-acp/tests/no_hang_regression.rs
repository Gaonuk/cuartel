//! sendPrompt-hang regression test (Phase B2 DoD bullet, v2 doc).
//!
//! The bug that triggered the v2 architecture refactor: `sendPrompt`
//! would silently hang inside Rivet's secure-exec V8 sandbox because
//! the Claude CLI was a V8 grandchild fighting polyfilled syscalls.
//! Removing V8 nesting (running claude-code-acp as a plain Node OS
//! process via [`LocalSandbox`]) is supposed to fix it permanently.
//!
//! This test asserts the fix sticks: 5 consecutive `prompt → result`
//! cycles via `LocalSandbox` complete without hanging. Earlier spike
//! work showed one run is sufficient evidence on its own (KB §22), but
//! the regression test runs 5 to catch any flake or slow degradation
//! that single-run validation might miss.
//!
//! Costs ~$0.05 in API spend per full run. Skipped automatically
//! unless `CUARTEL_ACP_LIVE_TEST=1` is set.
//!
//! Run with:
//!   CUARTEL_ACP_LIVE_TEST=1 cargo test -p cuartel-acp \
//!       --test no_hang_regression -- --nocapture

use std::time::Duration;

use cuartel_acp::{spawn_local_with_default_handler, SessionEvent};

const ENABLE_ENV: &str = "CUARTEL_ACP_LIVE_TEST";
const ITERATIONS: usize = 5;
/// Hard upper bound per iteration. A successful "Reply with OK" turn
/// completes in 5–20s on a typical network. Anything past 60s is a hang.
const HANG_TIMEOUT: Duration = Duration::from_secs(60);

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
async fn five_consecutive_turns_no_hang() {
    if !live_test_enabled() {
        eprintln!("skipping live regression: set {ENABLE_ENV}=1 to enable");
        return;
    }
    if !npx_available() {
        eprintln!("skipping live regression: npx not on PATH");
        return;
    }

    let cwd = std::env::current_dir().expect("cwd");
    let mut hangs = 0usize;
    let mut errors = 0usize;
    let mut durations = Vec::with_capacity(ITERATIONS);

    for i in 1..=ITERATIONS {
        eprintln!("--- iteration {i}/{ITERATIONS} ---");
        let started = std::time::Instant::now();

        let outcome = tokio::time::timeout(HANG_TIMEOUT, async {
            let client = spawn_local_with_default_handler(cwd.clone()).await?;
            let session = client.new_session(cwd.clone()).await?;
            let mut events = client
                .prompt(&session, "Reply with only the word OK.".into())
                .await?;

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
                        return Err(cuartel_acp::AcpError::Protocol {
                            reason: message,
                            raw: None,
                        });
                    }
                    _ => {}
                }
            }

            client.dispose().await;
            Ok::<_, cuartel_acp::AcpError>((got_message, stop_reason))
        })
        .await;

        let elapsed = started.elapsed();
        match outcome {
            Ok(Ok((message, stop_reason))) => {
                durations.push(elapsed);
                eprintln!(
                    "  ok in {:.2}s; stop_reason={:?}; message={:?}",
                    elapsed.as_secs_f32(),
                    stop_reason,
                    message,
                );
                assert_eq!(
                    stop_reason.as_deref(),
                    Some("end_turn"),
                    "iteration {i}: expected end_turn, got {stop_reason:?}"
                );
            }
            Ok(Err(e)) => {
                errors += 1;
                eprintln!("  ERROR in {:.2}s: {e}", elapsed.as_secs_f32());
            }
            Err(_) => {
                hangs += 1;
                eprintln!(
                    "  HANG: exceeded {}s without completing",
                    HANG_TIMEOUT.as_secs()
                );
            }
        }
    }

    let p_max = durations.iter().max().copied().unwrap_or_default();
    let p_min = durations.iter().min().copied().unwrap_or_default();
    eprintln!(
        "\nsummary: {}/{} ok, {} hangs, {} errors; min={:.2}s max={:.2}s",
        durations.len(),
        ITERATIONS,
        hangs,
        errors,
        p_min.as_secs_f32(),
        p_max.as_secs_f32(),
    );

    assert_eq!(
        hangs, 0,
        "{hangs}/{ITERATIONS} iterations hung — V8-nesting hang regression?"
    );
    assert_eq!(
        errors, 0,
        "{errors}/{ITERATIONS} iterations errored"
    );
}
