//! Permission prompt UI.
//!
//! Renders a banner whenever an agent session has a pending tool-use
//! permission request, with Approve / Deny actions. The component owns a
//! FIFO queue so multiple requests pile up behind the currently-displayed
//! one; decisions pop the head and emit a [`PermissionDecision`] event that
//! Phase 3f will forward back to Rivet.
//!
//! For 3g this is wired against fixture data in `CuartelApp::new`.

use crate::theme::Theme;
use cuartel_core::agent::HarnessEvent;
use gpui::*;
use serde_json::Value;
use std::collections::VecDeque;

/// One pending tool-use permission awaiting the user's decision.
#[derive(Clone, Debug)]
pub struct PendingPermission {
    pub id: String,
    pub session_id: String,
    pub session_label: SharedString,
    pub tool_name: String,
    pub summary: SharedString,
    pub input: Value,
}

impl PendingPermission {
    pub fn new(
        id: impl Into<String>,
        session_id: impl Into<String>,
        session_label: impl Into<SharedString>,
        tool_name: impl Into<String>,
        input: Value,
    ) -> Self {
        let tool_name = tool_name.into();
        let summary = summarize_tool_input(&tool_name, &input);
        Self {
            id: id.into(),
            session_id: session_id.into(),
            session_label: session_label.into(),
            tool_name,
            summary,
            input,
        }
    }

    /// Build a PendingPermission from a HarnessEvent::ToolUse. Returns None
    /// for any other variant.
    #[allow(dead_code)] // wired in phase 3f
    pub fn from_harness_event(
        id: impl Into<String>,
        session_id: impl Into<String>,
        session_label: impl Into<SharedString>,
        event: &HarnessEvent,
    ) -> Option<Self> {
        match event {
            HarnessEvent::ToolUse { name, input } => Some(Self::new(
                id,
                session_id,
                session_label,
                name.clone(),
                input.clone(),
            )),
            _ => None,
        }
    }
}

/// Decision fired when the user approves or denies a permission request.
/// Phase 3f subscribes to this and forwards the reply over Rivet.
#[derive(Clone, Debug)]
#[allow(dead_code)] // fields consumed in phase 3f
pub enum PermissionDecision {
    Approve { id: String, session_id: String },
    Deny { id: String, session_id: String },
}

pub struct PermissionPrompt {
    queue: VecDeque<PendingPermission>,
}

impl EventEmitter<PermissionDecision> for PermissionPrompt {}

impl PermissionPrompt {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            queue: VecDeque::new(),
        }
    }

    pub fn enqueue(&mut self, pending: PendingPermission, cx: &mut Context<Self>) {
        self.queue.push_back(pending);
        cx.notify();
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    fn approve_head(&mut self, cx: &mut Context<Self>) {
        if let Some(p) = self.queue.pop_front() {
            log::info!(
                "permission approved: id={} session={} tool={}",
                p.id,
                p.session_id,
                p.tool_name,
            );
            cx.emit(PermissionDecision::Approve {
                id: p.id,
                session_id: p.session_id,
            });
            cx.notify();
        }
    }

    fn deny_head(&mut self, cx: &mut Context<Self>) {
        if let Some(p) = self.queue.pop_front() {
            log::info!(
                "permission denied: id={} session={} tool={}",
                p.id,
                p.session_id,
                p.tool_name,
            );
            cx.emit(PermissionDecision::Deny {
                id: p.id,
                session_id: p.session_id,
            });
            cx.notify();
        }
    }
}

impl Render for PermissionPrompt {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::dark();

        // Render an empty zero-height placeholder when the queue is empty so
        // the parent workspace can unconditionally mount us without a flicker.
        let Some(head) = self.queue.front().cloned() else {
            return div().id("permission-prompt").into_any_element();
        };

        let queue_len = self.queue.len();
        let input_pretty = pretty_json(&head.input);
        let position_label = if queue_len > 1 {
            SharedString::from(format!("1 of {queue_len} pending"))
        } else {
            SharedString::from("")
        };

        div()
            .id("permission-prompt")
            .flex()
            .flex_col()
            .w_full()
            .bg(rgb(theme.bg_secondary))
            .border_b_1()
            .border_color(rgb(theme.warning))
            .px_4()
            .py_3()
            .gap_2()
            // Header row: badge, session label, queue counter.
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .size(px(8.0))
                                    .rounded_full()
                                    .bg(rgb(theme.warning)),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(rgb(theme.warning))
                                    .child("PERMISSION REQUEST"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(theme.text_muted))
                                    .child(SharedString::from(format!(
                                        "• {}",
                                        head.session_label
                                    ))),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(theme.text_muted))
                            .child(position_label),
                    ),
            )
            // Summary line.
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .items_baseline()
                    .child(
                        div()
                            .flex_none()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(theme.text_primary))
                            .child(SharedString::from(head.tool_name.clone())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_color(rgb(theme.text_secondary))
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(head.summary.clone()),
                    ),
            )
            // Pretty-printed JSON input.
            .child(
                div()
                    .id("permission-input")
                    .font_family("Lilex")
                    .text_xs()
                    .text_color(rgb(theme.text_secondary))
                    .bg(rgb(theme.bg_primary))
                    .rounded_md()
                    .p_2()
                    .max_h(px(120.0))
                    .overflow_y_scroll()
                    .child(input_pretty),
            )
            // Action buttons.
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .justify_end()
                    .child(
                        div()
                            .id("deny-btn")
                            .px_4()
                            .py_1p5()
                            .rounded_md()
                            .bg(rgb(theme.bg_primary))
                            .border_1()
                            .border_color(rgb(theme.border))
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(theme.error))
                            .hover(|s| s.bg(rgb(theme.bg_hover)))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _evt, _win, cx| this.deny_head(cx)))
                            .child("Deny"),
                    )
                    .child(
                        div()
                            .id("approve-btn")
                            .px_4()
                            .py_1p5()
                            .rounded_md()
                            .bg(rgb(theme.success))
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(theme.bg_primary))
                            .hover(|s| s.bg(rgb(theme.accent)))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _evt, _win, cx| this.approve_head(cx)))
                            .child("Approve"),
                    ),
            )
            .into_any_element()
    }
}

/// Best-effort one-line summary of a tool-call payload.
fn summarize_tool_input(name: &str, input: &Value) -> SharedString {
    let get_str = |k: &str| input.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let text = match name {
        "bash" | "shell" | "run" => get_str("command"),
        "write_file" | "create_file" | "edit_file" => {
            get_str("path").map(|p| format!("write {p}"))
        }
        "read_file" => get_str("path").map(|p| format!("read {p}")),
        "search" | "grep" => get_str("pattern").map(|p| format!("search {p:?}")),
        "fetch" | "http" => get_str("url"),
        _ => None,
    };
    SharedString::from(text.unwrap_or_else(|| format!("call {name}")))
}

fn pretty_json(v: &Value) -> SharedString {
    serde_json::to_string_pretty(v)
        .map(SharedString::from)
        .unwrap_or_else(|_| SharedString::from("(invalid json)"))
}
