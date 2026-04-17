use crate::sidebar_visuals::{
    describe_sidecar, relative_time, server_visuals, status_visuals,
};
use crate::sidecar_host::SidecarStatus;
use crate::theme::Theme;
use chrono::{DateTime, Utc};
use cuartel_core::agent::AgentType;
use cuartel_core::session::SessionState;
use gpui::*;
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Duration;

/// Event fired when the user picks a session in the sidebar. The parent
/// (`CuartelApp`) subscribes and forwards it to the workspace view.
#[derive(Clone, Debug)]
#[allow(dead_code)] // `id` wired for 3f when clicks drive real Rivet sessions.
pub struct SessionSelected {
    pub id: String,
    pub label: SharedString,
    pub agent: SharedString,
}

#[derive(Clone, Debug)]
pub struct SettingsRequested;

/// User clicked a server entry in the sidebar. The parent uses this to
/// mark the server as "active" (phase 7e will route new sessions to it).
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct ServerSelected {
    pub id: String,
}

pub struct Sidebar {
    sessions: Vec<SessionItem>,
    selected_index: Option<usize>,
    sidecar_status: Arc<Mutex<SidecarStatus>>,
    last_seen_status: SidecarStatus,
    servers: Vec<ServerItem>,
    active_server_id: Option<String>,
    _poll_task: Task<()>,
}

impl EventEmitter<SessionSelected> for Sidebar {}
impl EventEmitter<SettingsRequested> for Sidebar {}
impl EventEmitter<ServerSelected> for Sidebar {}

#[derive(Clone)]
pub struct SessionItem {
    pub id: SharedString,
    pub label: SharedString,
    pub agent: AgentType,
    pub state: SessionState,
    pub created_at: DateTime<Utc>,
}

impl SessionItem {
    pub fn new(
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        agent: AgentType,
        state: SessionState,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            agent,
            state,
            created_at: Utc::now(),
        }
    }

    pub fn with_created_at(mut self, at: DateTime<Utc>) -> Self {
        self.created_at = at;
        self
    }
}

/// One row in the SERVERS section. Covers both the local sidecar
/// (status streamed from `SidecarStatus`) and remote registered peers
/// (reachability polled from cuartel-remote).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerItem {
    pub id: SharedString,
    pub name: SharedString,
    pub address: SharedString,
    pub tailscale_ip: Option<SharedString>,
    pub is_local: bool,
    pub reachable: Option<bool>,
}

impl Sidebar {
    pub fn new(
        sidecar_status: Arc<Mutex<SidecarStatus>>,
        server_state: Option<Arc<Mutex<Vec<ServerItem>>>>,
        cx: &mut Context<Self>,
    ) -> Self {
        let status_poll = sidecar_status.clone();
        let server_poll = server_state.clone();
        let poll_task = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                let current = status_poll.lock().clone();
                let latest_servers = server_poll.as_ref().map(|s| s.lock().clone());
                if this
                    .update(cx, |sidebar, cx| {
                        if sidebar.last_seen_status != current {
                            sidebar.last_seen_status = current;
                            cx.notify();
                        }
                        if let Some(servers) = latest_servers {
                            sidebar.set_servers(servers, cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        });

        let initial_status = sidecar_status.lock().clone();
        let initial_servers = server_state
            .as_ref()
            .map(|s| s.lock().clone())
            .unwrap_or_default();
        Self {
            sessions: vec![],
            selected_index: None,
            last_seen_status: initial_status,
            sidecar_status,
            servers: initial_servers,
            active_server_id: None,
            _poll_task: poll_task,
        }
    }

    pub fn set_sessions(&mut self, items: Vec<SessionItem>, cx: &mut Context<Self>) {
        self.sessions = items;
        if self.selected_index.is_none() && !self.sessions.is_empty() {
            self.selected_index = Some(0);
        }
        cx.notify();
    }

    pub fn set_servers(&mut self, items: Vec<ServerItem>, cx: &mut Context<Self>) {
        if self.servers != items {
            self.servers = items;
            cx.notify();
        }
    }

    #[allow(dead_code)]
    pub fn set_active_server(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        if self.active_server_id != id {
            self.active_server_id = id;
            cx.notify();
        }
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.sessions.len() {
            return;
        }
        if self.selected_index == Some(index) {
            return;
        }
        self.selected_index = Some(index);
        let item = &self.sessions[index];
        cx.emit(SessionSelected {
            id: item.id.to_string(),
            label: item.label.clone(),
            agent: SharedString::from(item.agent.display_name().to_string()),
        });
        cx.notify();
    }

    fn select_server(&mut self, id: SharedString, cx: &mut Context<Self>) {
        let id_str = id.to_string();
        if self.active_server_id.as_deref() == Some(&id_str) {
            return;
        }
        self.active_server_id = Some(id_str.clone());
        cx.emit(ServerSelected { id: id_str });
        cx.notify();
    }
}

impl Render for Sidebar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::dark();
        let now = Utc::now();

        div()
            .id("sidebar")
            .flex()
            .flex_col()
            .w(px(256.0))
            .h_full()
            .bg(rgb(theme.bg_sidebar))
            .border_r_1()
            .border_color(rgb(theme.border))
            .font_family("IBM Plex Sans")
            .child(
                // Header
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .py_2()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(theme.text_primary))
                            .child("Sessions"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(theme.text_muted))
                            .child(format!("{}", self.sessions.len())),
                    ),
            )
            .child({
                // Session list. Row rendering has to happen inside a closure
                // that owns `&mut Context<Sidebar>` so we can build per-row
                // `cx.listener` click handlers.
                let selected_index = self.selected_index;
                let rows: Vec<AnyElement> = self
                    .sessions
                    .iter()
                    .enumerate()
                    .map(|(idx, item)| {
                        self.render_session_row(
                            idx,
                            item,
                            selected_index == Some(idx),
                            now,
                            &theme,
                            cx,
                        )
                    })
                    .collect();
                div()
                    .id("session-list")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .overflow_y_scroll()
                    .py_1()
                    .children(rows)
            })
            .child(self.render_servers_section(&theme, cx))
            .child(
                // Settings button
                div()
                    .id("settings-btn")
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_t_1()
                    .border_color(rgb(theme.border))
                    .px_3()
                    .py_2()
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(theme.bg_hover)))
                    .on_click(cx.listener(|this, _evt, _win, cx| {
                        let _ = this;
                        cx.emit(SettingsRequested);
                    }))
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(theme.text_muted))
                            .child("\u{2699}"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(theme.text_secondary))
                            .child("Settings"),
                    ),
            )
    }
}

impl Sidebar {
    fn render_session_row(
        &self,
        index: usize,
        item: &SessionItem,
        selected: bool,
        now: DateTime<Utc>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (dot_color, status_label) = status_visuals(&item.state, theme);
        let bg = if selected { theme.bg_active } else { theme.bg_sidebar };
        let hover_bg = if selected { theme.bg_active } else { theme.bg_hover };
        let age = relative_time(now, item.created_at);
        let subline = SharedString::from(format!(
            "{} • {} • {}",
            item.agent.display_name(),
            status_label,
            age
        ));

        div()
            .id(ElementId::Name(item.id.clone().into()))
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .mx_1()
            .px_2()
            .py_1p5()
            .rounded_md()
            .bg(rgb(bg))
            .hover(move |s| s.bg(rgb(hover_bg)))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _evt, _window, cx| {
                this.select(index, cx);
            }))
            // Fixed-width leading icon column.
            .child(
                div()
                    .flex()
                    .flex_none()
                    .w(px(12.0))
                    .justify_center()
                    .child(
                        div()
                            .size(px(8.0))
                            .rounded_full()
                            .bg(rgb(dot_color)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(theme.text_primary))
                            .font_weight(if selected {
                                FontWeight::SEMIBOLD
                            } else {
                                FontWeight::NORMAL
                            })
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(item.label.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(theme.text_muted))
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(subline),
                    ),
            )
            .into_any_element()
    }

    fn render_servers_section(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let sidecar_status = self.sidecar_status.lock().clone();
        let active_id = self.active_server_id.clone();
        let rows: Vec<AnyElement> = self
            .servers
            .iter()
            .map(|item| {
                self.render_server_row(
                    item,
                    &sidecar_status,
                    active_id.as_deref() == Some(&item.id),
                    theme,
                    cx,
                )
            })
            .collect();

        // Fallback: when the registry hasn't populated yet (very first
        // paint), render the live local sidecar status so the UI is never
        // empty. Once set_servers is called this branch no longer runs.
        let fallback = if self.servers.is_empty() {
            Some(self.render_local_fallback(&sidecar_status, theme))
        } else {
            None
        };

        div()
            .id("servers-section")
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(rgb(theme.border))
            .px_3()
            .py_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(theme.text_muted))
                            .child("SERVERS"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(theme.text_muted))
                            .child(format!("{}", self.servers.len().max(1))),
                    ),
            )
            .children(rows)
            .children(fallback)
            .into_any_element()
    }

    fn render_server_row(
        &self,
        item: &ServerItem,
        sidecar_status: &SidecarStatus,
        is_active: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (dot_color, subline) = server_visuals(item, sidecar_status, theme);
        let bg = if is_active { theme.bg_active } else { theme.bg_sidebar };
        let hover_bg = if is_active { theme.bg_active } else { theme.bg_hover };
        let id = item.id.clone();
        let row_id = ElementId::Name(format!("server-{}", item.id).into());

        div()
            .id(row_id)
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .py_1()
            .px_1()
            .rounded_md()
            .bg(rgb(bg))
            .hover(move |s| s.bg(rgb(hover_bg)))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _evt, _win, cx| {
                this.select_server(id.clone(), cx);
            }))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .w(px(12.0))
                    .justify_center()
                    .child(
                        div()
                            .size(px(6.0))
                            .rounded_full()
                            .bg(rgb(dot_color)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(theme.text_secondary))
                            .font_weight(if is_active {
                                FontWeight::SEMIBOLD
                            } else {
                                FontWeight::NORMAL
                            })
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(item.name.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(theme.text_muted))
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(subline),
                    ),
            )
            .into_any_element()
    }

    fn render_local_fallback(
        &self,
        status: &SidecarStatus,
        theme: &Theme,
    ) -> AnyElement {
        let (dot_color, label, sub) = describe_sidecar(status, theme);
        div()
            .flex()
            .items_center()
            .gap_2()
            .py_1()
            .child(
                div()
                    .size(px(6.0))
                    .rounded_full()
                    .bg(rgb(dot_color)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(theme.text_secondary))
                            .child(label),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(theme.text_muted))
                            .child(sub),
                    ),
            )
            .into_any_element()
    }
}

