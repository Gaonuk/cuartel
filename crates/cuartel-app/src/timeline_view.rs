//! Checkpoint timeline panel (spec task 6c).
//!
//! Vertical timeline that renders the checkpoint history for the active
//! session. Each checkpoint is a row with a timestamp, optional label,
//! metadata summary, and fork indicator. The selected checkpoint exposes
//! Restore / Fork / Delete action buttons.
//!
//! The view is data-driven: callers push checkpoints in via
//! [`TimelineView::set_checkpoints`] and subscribe to the events emitted
//! when the user triggers an action.

use crate::theme::Theme;
use cuartel_core::checkpoint::Checkpoint;
use gpui::prelude::FluentBuilder;
use gpui::*;

// --- Events ---------------------------------------------------------------

/// User wants to restore the session to a checkpoint (in-place rewind).
#[derive(Clone, Debug)]
pub struct CheckpointRestore {
    pub checkpoint_id: String,
}

/// User wants to fork a new session from a checkpoint.
#[derive(Clone, Debug)]
pub struct CheckpointFork {
    pub checkpoint_id: String,
    pub session_id: String,
}

/// User wants to delete a checkpoint.
#[derive(Clone, Debug)]
pub struct CheckpointDelete {
    pub checkpoint_id: String,
}

impl EventEmitter<CheckpointRestore> for TimelineView {}
impl EventEmitter<CheckpointFork> for TimelineView {}
impl EventEmitter<CheckpointDelete> for TimelineView {}

// --- TimelineView ---------------------------------------------------------

pub struct TimelineView {
    checkpoints: Vec<Checkpoint>,
    selected_index: Option<usize>,
    session_id: String,
}

impl TimelineView {
    pub fn new(session_id: String, _cx: &mut Context<Self>) -> Self {
        Self {
            checkpoints: Vec::new(),
            selected_index: None,
            session_id,
        }
    }

    pub fn set_session_id(&mut self, session_id: String, cx: &mut Context<Self>) {
        if self.session_id != session_id {
            self.session_id = session_id;
            self.checkpoints.clear();
            self.selected_index = None;
            cx.notify();
        }
    }

    pub fn set_checkpoints(&mut self, checkpoints: Vec<Checkpoint>, cx: &mut Context<Self>) {
        self.checkpoints = checkpoints;
        // Keep selection if still valid, otherwise clear.
        if let Some(idx) = self.selected_index {
            if idx >= self.checkpoints.len() {
                self.selected_index = if self.checkpoints.is_empty() {
                    None
                } else {
                    Some(self.checkpoints.len() - 1)
                };
            }
        }
        cx.notify();
    }

    pub fn len(&self) -> usize {
        self.checkpoints.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.checkpoints.is_empty()
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.checkpoints.len() || self.selected_index == Some(index) {
            return;
        }
        self.selected_index = Some(index);
        cx.notify();
    }

    fn restore(&mut self, cx: &mut Context<Self>) {
        if let Some(idx) = self.selected_index {
            if let Some(cp) = self.checkpoints.get(idx) {
                cx.emit(CheckpointRestore {
                    checkpoint_id: cp.id.clone(),
                });
            }
        }
    }

    fn fork(&mut self, cx: &mut Context<Self>) {
        if let Some(idx) = self.selected_index {
            if let Some(cp) = self.checkpoints.get(idx) {
                cx.emit(CheckpointFork {
                    checkpoint_id: cp.id.clone(),
                    session_id: self.session_id.clone(),
                });
            }
        }
    }

    fn delete(&mut self, cx: &mut Context<Self>) {
        if let Some(idx) = self.selected_index {
            if let Some(cp) = self.checkpoints.get(idx) {
                cx.emit(CheckpointDelete {
                    checkpoint_id: cp.id.clone(),
                });
            }
        }
    }

    /// Whether the selected checkpoint can be forked (needs a rivet id).
    fn can_fork_selected(&self) -> bool {
        self.selected_index
            .and_then(|i| self.checkpoints.get(i))
            .map(|cp| cp.rivet_checkpoint_id.is_some())
            .unwrap_or(false)
    }

    /// Whether the selected checkpoint can be restored.
    fn can_restore_selected(&self) -> bool {
        self.selected_index
            .and_then(|i| self.checkpoints.get(i))
            .map(|cp| cp.rivet_checkpoint_id.is_some())
            .unwrap_or(false)
    }

    /// Whether the selected checkpoint can be deleted (no children).
    fn can_delete_selected(&self) -> bool {
        let Some(idx) = self.selected_index else {
            return false;
        };
        let Some(cp) = self.checkpoints.get(idx) else {
            return false;
        };
        // A checkpoint can be deleted if no other checkpoint in the list
        // has it as a parent.
        !self
            .checkpoints
            .iter()
            .any(|other| other.parent_checkpoint_id.as_deref() == Some(&cp.id))
    }
}

impl Render for TimelineView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::dark();

        if self.checkpoints.is_empty() {
            return div()
                .id("timeline-empty")
                .flex()
                .flex_1()
                .flex_col()
                .items_center()
                .justify_center()
                .bg(rgb(theme.bg_primary))
                .text_color(rgb(theme.text_muted))
                .text_sm()
                .child("No checkpoints yet")
                .child(
                    div()
                        .text_xs()
                        .mt_1()
                        .text_color(rgb(theme.text_muted))
                        .child("Checkpoints will appear here as you work"),
                )
                .into_any_element();
        }

        let selected_index = self.selected_index;
        let can_restore = self.can_restore_selected();
        let can_fork = self.can_fork_selected();
        let can_delete = self.can_delete_selected();
        let count = self.checkpoints.len();

        // Header with count and action buttons.
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .h(px(36.0))
            .px_3()
            .bg(rgb(theme.bg_secondary))
            .border_b_1()
            .border_color(rgb(theme.border))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(theme.text_secondary))
                            .child(SharedString::from(format!(
                                "{count} checkpoint{}",
                                if count == 1 { "" } else { "s" }
                            ))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(timeline_action_button(
                        "restore-btn",
                        "Restore",
                        theme.accent,
                        can_restore,
                        &theme,
                        cx,
                        |this, cx| this.restore(cx),
                    ))
                    .child(timeline_action_button(
                        "fork-btn",
                        "Fork",
                        theme.success,
                        can_fork,
                        &theme,
                        cx,
                        |this, cx| this.fork(cx),
                    ))
                    .child(timeline_action_button(
                        "delete-btn",
                        "Delete",
                        theme.error,
                        can_delete,
                        &theme,
                        cx,
                        |this, cx| this.delete(cx),
                    )),
            );

        // Checkpoint rows rendered as a vertical timeline.
        let total = self.checkpoints.len();
        let rows: Vec<AnyElement> = self
            .checkpoints
            .iter()
            .enumerate()
            .map(|(idx, cp)| {
                render_checkpoint_row(
                    idx,
                    cp,
                    selected_index == Some(idx),
                    idx == total - 1,
                    &self.checkpoints,
                    &theme,
                    cx,
                )
            })
            .collect();

        // Detail panel for selected checkpoint.
        let detail: AnyElement = match selected_index.and_then(|i| self.checkpoints.get(i)) {
            Some(cp) => render_checkpoint_detail(cp, &self.checkpoints, &theme).into_any_element(),
            None => div()
                .id("timeline-detail-empty")
                .flex()
                .flex_1()
                .items_center()
                .justify_center()
                .text_color(rgb(theme.text_muted))
                .text_sm()
                .child("Select a checkpoint")
                .into_any_element(),
        };

        div()
            .id("timeline-view")
            .flex()
            .flex_col()
            .flex_1()
            .bg(rgb(theme.bg_primary))
            .font_family("IBM Plex Sans")
            .child(header)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    // Left: timeline list
                    .child(
                        div()
                            .id("timeline-list")
                            .flex()
                            .flex_col()
                            .w(px(300.0))
                            .h_full()
                            .bg(rgb(theme.bg_sidebar))
                            .border_r_1()
                            .border_color(rgb(theme.border))
                            .overflow_y_scroll()
                            .py_2()
                            .children(rows),
                    )
                    // Right: detail panel
                    .child(
                        div()
                            .id("timeline-detail")
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .overflow_y_scroll()
                            .child(detail),
                    ),
            )
            .into_any_element()
    }
}

// --- Checkpoint row -------------------------------------------------------

fn render_checkpoint_row(
    index: usize,
    cp: &Checkpoint,
    selected: bool,
    is_last: bool,
    all_checkpoints: &[Checkpoint],
    theme: &Theme,
    cx: &mut Context<TimelineView>,
) -> AnyElement {
    let bg = if selected {
        theme.bg_active
    } else {
        theme.bg_sidebar
    };
    let hover_bg = if selected {
        theme.bg_active
    } else {
        theme.bg_hover
    };

    let is_fork = cp.parent_checkpoint_id.is_some();
    let has_children = all_checkpoints
        .iter()
        .any(|other| other.parent_checkpoint_id.as_deref() == Some(&cp.id));

    let label = cp
        .label
        .as_deref()
        .unwrap_or("Unnamed checkpoint");
    let label = SharedString::from(label.to_string());

    let timestamp = format_timestamp(&cp.created_at);
    let linked = cp.rivet_checkpoint_id.is_some();

    let row_id = ElementId::Name(format!("cp-row-{index}").into());

    // Timeline connector: vertical line + dot
    let dot_color = if selected {
        theme.accent
    } else if is_fork {
        theme.success
    } else {
        theme.text_muted
    };

    let connector = div()
        .flex_none()
        .w(px(24.0))
        .flex()
        .flex_col()
        .items_center()
        .child(
            // Dot
            div()
                .size(px(10.0))
                .rounded_full()
                .bg(rgb(dot_color))
                .border_2()
                .border_color(rgb(if selected { theme.accent } else { theme.border })),
        )
        .when(!is_last, |el| {
            // Vertical connector line to next checkpoint
            el.child(
                div()
                    .flex_1()
                    .w(px(2.0))
                    .min_h(px(20.0))
                    .bg(rgb(theme.border)),
            )
        });

    // Content
    let content = div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
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
                        .child(label),
                )
                .when(is_fork, |el| {
                    el.child(
                        div()
                            .px_1()
                            .rounded_sm()
                            .bg(rgb(theme.success))
                            .text_xs()
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(0x1e1e2e))
                            .child("fork"),
                    )
                })
                .when(has_children, |el| {
                    el.child(
                        div()
                            .px_1()
                            .rounded_sm()
                            .bg(rgb(theme.warning))
                            .text_xs()
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(0x1e1e2e))
                            .child("parent"),
                    )
                }),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(theme.text_muted))
                        .child(SharedString::from(timestamp)),
                )
                .when(!linked, |el| {
                    el.child(
                        div()
                            .text_xs()
                            .text_color(rgb(theme.warning))
                            .child("unlinked"),
                    )
                }),
        );

    div()
        .id(row_id)
        .flex()
        .flex_row()
        .items_start()
        .gap_2()
        .mx_2()
        .px_2()
        .py_1p5()
        .rounded_md()
        .bg(rgb(bg))
        .hover(move |s| s.bg(rgb(hover_bg)))
        .cursor_pointer()
        .on_click(cx.listener(move |this, _evt, _win, cx| this.select(index, cx)))
        .child(connector)
        .child(content)
        .into_any_element()
}

// --- Checkpoint detail panel ----------------------------------------------

fn render_checkpoint_detail(
    cp: &Checkpoint,
    all_checkpoints: &[Checkpoint],
    theme: &Theme,
) -> Div {
    let is_fork = cp.parent_checkpoint_id.is_some();
    let children_count = all_checkpoints
        .iter()
        .filter(|other| other.parent_checkpoint_id.as_deref() == Some(&cp.id))
        .count();

    let label = cp
        .label
        .as_deref()
        .unwrap_or("Unnamed checkpoint");

    let header = div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .h(px(32.0))
        .px_3()
        .bg(rgb(theme.bg_secondary))
        .border_b_1()
        .border_color(rgb(theme.border))
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(theme.text_primary))
                .child(SharedString::from(label.to_string())),
        );

    let mut rows: Vec<AnyElement> = Vec::new();

    rows.push(detail_row("ID", &cp.id[..8.min(cp.id.len())], theme));
    rows.push(detail_row("Created", &cp.created_at, theme));

    if let Some(rivet_id) = &cp.rivet_checkpoint_id {
        rows.push(detail_row(
            "Rivet ID",
            &rivet_id[..12.min(rivet_id.len())],
            theme,
        ));
    }

    if is_fork {
        if let Some(parent_id) = &cp.parent_checkpoint_id {
            let parent_label = all_checkpoints
                .iter()
                .find(|c| c.id == *parent_id)
                .and_then(|c| c.label.as_deref())
                .unwrap_or(&parent_id[..8.min(parent_id.len())]);
            rows.push(detail_row("Forked from", parent_label, theme));
        }
    }

    if children_count > 0 {
        rows.push(detail_row(
            "Forks",
            &format!("{children_count} fork{}", if children_count == 1 { "" } else { "s" }),
            theme,
        ));
    }

    // Metadata
    if let Some(obj) = cp.metadata.as_object() {
        if !obj.is_empty() {
            rows.push(
                div()
                    .mt_2()
                    .px_3()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(theme.text_muted))
                            .child("METADATA"),
                    )
                    .into_any_element(),
            );
            for (key, value) in obj {
                let val_str = match value {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                rows.push(detail_row(key, &val_str, theme));
            }
        }
    }

    div()
        .flex()
        .flex_col()
        .child(header)
        .child(div().flex().flex_col().py_2().children(rows))
}

fn detail_row(label: &str, value: &str, theme: &Theme) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .px_3()
        .py(px(4.0))
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(theme.text_muted))
                .child(SharedString::from(label.to_string())),
        )
        .child(
            div()
                .text_xs()
                .font_family("Lilex")
                .text_color(rgb(theme.text_secondary))
                .child(SharedString::from(value.to_string())),
        )
        .into_any_element()
}

// --- Action button --------------------------------------------------------

fn timeline_action_button(
    id: &'static str,
    label: &'static str,
    accent: u32,
    enabled: bool,
    theme: &Theme,
    cx: &mut Context<TimelineView>,
    handler: fn(&mut TimelineView, &mut Context<TimelineView>),
) -> Stateful<Div> {
    let fg = if enabled { accent } else { theme.text_muted };
    let border = if enabled { accent } else { theme.border };
    div()
        .id(id)
        .px_2()
        .py(px(2.0))
        .rounded_md()
        .border_1()
        .border_color(rgb(border))
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(fg))
        .when(enabled, |el| {
            el.cursor_pointer()
                .hover(|s| s.bg(rgb(theme.bg_hover)))
                .on_click(cx.listener(move |this, _evt, _win, cx| handler(this, cx)))
        })
        .child(label)
}

// --- Helpers --------------------------------------------------------------

/// Format an ISO-8601 timestamp string into a human-friendly relative time.
fn format_timestamp(ts: &str) -> String {
    // Try to parse as ISO-8601. If it fails, just return the raw string.
    let Ok(dt) = chrono::DateTime::parse_from_rfc3339(ts) else {
        // Try the SQLite datetime format (YYYY-MM-DD HH:MM:SS)
        let Ok(naive) = chrono::NaiveDateTime::parse_from_str(ts, "%Y-%m-%d %H:%M:%S") else {
            return ts.to_string();
        };
        let dt = naive.and_utc();
        return relative_time(dt);
    };
    relative_time(dt.with_timezone(&chrono::Utc))
}

fn relative_time(then: chrono::DateTime<chrono::Utc>) -> String {
    let now = chrono::Utc::now();
    let dur = now.signed_duration_since(then);
    let secs = dur.num_seconds().max(0);
    if secs < 5 {
        "just now".to_string()
    } else if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}
