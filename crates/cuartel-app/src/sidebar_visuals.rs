//! Pure presentation helpers for [`crate::sidebar`].
//!
//! These functions take the inputs the sidebar needs to paint a row and
//! return `(color, text)` tuples — no gpui types, no rendering. Keeping
//! them in their own module lets us unit-test the tri-state reachability
//! logic without dragging the huge `Render` impl through the test binary.

use cuartel_core::session::SessionState;
use gpui::SharedString;

use crate::sidebar::ServerItem;
use crate::sidecar_host::SidecarStatus;
use crate::theme::Theme;

/// Map a `SessionState` onto a (dot color, short status label).
pub fn status_visuals(state: &SessionState, theme: &Theme) -> (u32, SharedString) {
    match state {
        SessionState::Created => (theme.text_muted, "new".into()),
        SessionState::Booting => (theme.warning, "booting".into()),
        SessionState::Ready => (theme.success, "ready".into()),
        SessionState::Running => (theme.accent, "running".into()),
        SessionState::Paused => (theme.warning, "paused".into()),
        SessionState::Checkpointed => (theme.text_muted, "checkpointed".into()),
        SessionState::Forked => (theme.accent, "forked".into()),
        SessionState::Reviewing => (theme.warning, "review".into()),
        SessionState::Error(msg) => (
            theme.error,
            SharedString::from(format!("error: {}", truncate(msg, 24))),
        ),
        SessionState::Destroyed => (theme.text_muted, "destroyed".into()),
    }
}

/// Decide the dot color + subtitle for a server row. Local rows reflect the
/// sidecar status; remote rows reflect the last Tailscale reachability probe.
pub fn server_visuals(
    item: &ServerItem,
    sidecar_status: &SidecarStatus,
    theme: &Theme,
) -> (u32, SharedString) {
    if item.is_local {
        let (color, _, sub) = describe_sidecar(sidecar_status, theme);
        (color, sub)
    } else {
        let sub: SharedString = match &item.tailscale_ip {
            Some(ip) => SharedString::from(format!("{ip} • {}", reachable_label(item.reachable))),
            None => SharedString::from(format!(
                "{} • {}",
                item.address,
                reachable_label(item.reachable),
            )),
        };
        let color = match item.reachable {
            Some(true) => theme.success,
            Some(false) => theme.error,
            None => theme.text_muted,
        };
        (color, sub)
    }
}

pub fn reachable_label(reachable: Option<bool>) -> &'static str {
    match reachable {
        Some(true) => "reachable",
        Some(false) => "unreachable",
        None => "checking…",
    }
}

pub fn describe_sidecar(
    status: &SidecarStatus,
    theme: &Theme,
) -> (u32, SharedString, SharedString) {
    match status {
        SidecarStatus::Idle => (
            theme.text_muted,
            "This Mac (local)".into(),
            "sidecar idle".into(),
        ),
        SidecarStatus::Installing => (
            theme.warning,
            "This Mac (local)".into(),
            "installing deps…".into(),
        ),
        SidecarStatus::Starting => (
            theme.warning,
            "This Mac (local)".into(),
            "starting rivet…".into(),
        ),
        SidecarStatus::Ready => (
            theme.success,
            "This Mac (local)".into(),
            "rivet ready on :6420".into(),
        ),
        SidecarStatus::Failed(msg) => (
            theme.error,
            "This Mac (local)".into(),
            SharedString::from(format!("sidecar failed: {msg}")),
        ),
    }
}

pub fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

pub fn relative_time(
    now: chrono::DateTime<chrono::Utc>,
    then: chrono::DateTime<chrono::Utc>,
) -> SharedString {
    let dur = now.signed_duration_since(then);
    let secs = dur.num_seconds().max(0);
    let out = if secs < 5 {
        "just now".to_string()
    } else if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    };
    SharedString::from(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration as ChronoDuration, TimeZone, Utc};

    fn theme() -> Theme {
        Theme::dark()
    }

    fn sample_server(reachable: Option<bool>, tailscale_ip: Option<&'static str>) -> ServerItem {
        ServerItem {
            id: "h1".into(),
            name: "Hetzner".into(),
            address: "http://100.67.106.62:6420".into(),
            tailscale_ip: tailscale_ip.map(SharedString::from),
            is_local: false,
            reachable,
        }
    }

    #[test]
    fn reachable_label_renders_tri_state() {
        assert_eq!(reachable_label(Some(true)), "reachable");
        assert_eq!(reachable_label(Some(false)), "unreachable");
        assert_eq!(reachable_label(None), "checking…");
    }

    #[test]
    fn server_visuals_local_uses_sidecar_status() {
        let theme = theme();
        let item = ServerItem {
            id: "local".into(),
            name: "This Mac".into(),
            address: "http://localhost:6420".into(),
            tailscale_ip: None,
            is_local: true,
            reachable: None,
        };

        let (color_ready, sub_ready) =
            server_visuals(&item, &SidecarStatus::Ready, &theme);
        assert_eq!(color_ready, theme.success);
        assert!(sub_ready.to_string().contains(":6420"));

        let (color_fail, _) =
            server_visuals(&item, &SidecarStatus::Failed("boom".into()), &theme);
        assert_eq!(color_fail, theme.error);
    }

    #[test]
    fn server_visuals_remote_checking_state() {
        let theme = theme();
        let item = sample_server(None, Some("100.67.106.62"));
        let (color, sub) = server_visuals(&item, &SidecarStatus::Ready, &theme);
        assert_eq!(color, theme.text_muted);
        assert!(sub.to_string().contains("100.67.106.62"));
        assert!(sub.to_string().contains("checking"));
    }

    #[test]
    fn server_visuals_remote_reachable() {
        let theme = theme();
        let item = sample_server(Some(true), Some("100.67.106.62"));
        let (color, sub) = server_visuals(&item, &SidecarStatus::Ready, &theme);
        assert_eq!(color, theme.success);
        assert!(sub.to_string().contains("reachable"));
    }

    #[test]
    fn server_visuals_remote_unreachable() {
        let theme = theme();
        let item = sample_server(Some(false), Some("100.67.106.62"));
        let (color, sub) = server_visuals(&item, &SidecarStatus::Ready, &theme);
        assert_eq!(color, theme.error);
        assert!(sub.to_string().contains("unreachable"));
    }

    #[test]
    fn server_visuals_remote_without_tailscale_ip_shows_address() {
        let theme = theme();
        let item = sample_server(Some(true), None);
        let (_, sub) = server_visuals(&item, &SidecarStatus::Ready, &theme);
        assert!(sub.to_string().contains("100.67.106.62:6420"));
    }

    #[test]
    fn describe_sidecar_covers_all_states() {
        let theme = theme();
        for status in [
            SidecarStatus::Idle,
            SidecarStatus::Installing,
            SidecarStatus::Starting,
            SidecarStatus::Ready,
            SidecarStatus::Failed("x".into()),
        ] {
            let (_, label, _) = describe_sidecar(&status, &theme);
            assert!(label.to_string().contains("This Mac"));
        }
    }

    #[test]
    fn status_visuals_produces_nonempty_labels() {
        let theme = theme();
        for state in [
            SessionState::Created,
            SessionState::Booting,
            SessionState::Ready,
            SessionState::Running,
            SessionState::Paused,
            SessionState::Checkpointed,
            SessionState::Forked,
            SessionState::Reviewing,
            SessionState::Error("oops because of a very long reason line that we need to clamp to something reasonable".into()),
            SessionState::Destroyed,
        ] {
            let (_, label) = status_visuals(&state, &theme);
            assert!(!label.is_empty());
        }
    }

    #[test]
    fn truncate_handles_ascii_and_unicode() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello", 3), "he…");
        assert_eq!(truncate("rápido", 4), "ráp…");
    }

    #[test]
    fn relative_time_scales_buckets() {
        let now = Utc.with_ymd_and_hms(2026, 4, 16, 12, 0, 0).unwrap();
        assert_eq!(relative_time(now, now).to_string(), "just now");
        assert_eq!(
            relative_time(now, now - ChronoDuration::seconds(30)).to_string(),
            "30s ago"
        );
        assert_eq!(
            relative_time(now, now - ChronoDuration::minutes(5)).to_string(),
            "5m ago"
        );
        assert_eq!(
            relative_time(now, now - ChronoDuration::hours(2)).to_string(),
            "2h ago"
        );
        assert_eq!(
            relative_time(now, now - ChronoDuration::days(3)).to_string(),
            "3d ago"
        );
    }
}
