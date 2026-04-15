//! First-run onboarding modal (spec task 3j).
//!
//! Renders a harness matrix driven by `availability::probe_registry` data.
//! Each row shows a status badge (`ready`, `needs credentials`, `not
//! installed`), an install hint for missing binaries, and — for rows that
//! are installed but missing credentials — an "Import from shell env"
//! affordance that copies the relevant env vars out of the host's
//! environment into the credential store.
//!
//! Design compromise: GPUI 0.2 does not ship a turnkey text input widget,
//! and cuartel hasn't grown a custom one yet. Instead of blocking the
//! onboarding on an inline credential form, this view uses the existing
//! shell environment as the source of truth: the user exports
//! `ANTHROPIC_API_KEY` in their shell, launches cuartel, clicks Import,
//! and the value is persisted into the keychain. Once the app grows a
//! real text input we can swap the import button for an inline form
//! without changing the persistence contract.
//!
//! State machine for the modal:
//!   render → user imports → user picks default → user saves → Completed
//! The `Completed` event is emitted once; `CuartelApp` listens for it,
//! marks `OnboardingConfig.completed = true`, writes it to disk, and
//! hides the modal.

use crate::theme::Theme;
use cuartel_core::agent::{AgentType, HarnessRegistry};
use cuartel_core::availability::{
    probe_registry, AvailabilityStatus, HarnessAvailability, WhichProbe,
};
use cuartel_core::credential_store::CredentialStore;
use futures::executor::block_on;
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::sync::Arc;

/// Event emitted when the user dismisses the modal. Carries the default
/// harness choice so the parent can persist it; the parent then knows the
/// app needs to be restarted for the new sidecar env to take effect.
#[derive(Clone, Debug)]
pub struct OnboardingCompleted {
    pub default_harness: AgentType,
}

pub struct OnboardingView {
    registry: Arc<HarnessRegistry>,
    credentials: Arc<dyn CredentialStore>,
    rows: Vec<HarnessAvailability>,
    selected_default: Option<AgentType>,
    /// User-facing log of what happened after the last import click, so
    /// they get feedback ("imported ANTHROPIC_API_KEY from env") without
    /// needing to tail logs.
    last_message: Option<SharedString>,
}

impl EventEmitter<OnboardingCompleted> for OnboardingView {}

impl OnboardingView {
    pub fn new(
        registry: Arc<HarnessRegistry>,
        credentials: Arc<dyn CredentialStore>,
        initial_default: Option<AgentType>,
        _cx: &mut Context<Self>,
    ) -> Self {
        let rows = block_on(probe_registry(
            registry.as_ref(),
            &WhichProbe,
            credentials.as_ref(),
        ));
        // If the user already picked a default previously but it's no
        // longer ready, drop it so they reselect.
        let selected_default = initial_default.and_then(|d| {
            rows.iter()
                .find(|r| r.agent == d && r.status() == AvailabilityStatus::Ready)
                .map(|r| r.agent.clone())
        });
        Self {
            registry,
            credentials,
            rows,
            selected_default,
            last_message: None,
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.rows = block_on(probe_registry(
            self.registry.as_ref(),
            &WhichProbe,
            self.credentials.as_ref(),
        ));
        if let Some(ref d) = self.selected_default {
            let still_ready = self
                .rows
                .iter()
                .any(|r| r.agent == *d && r.status() == AvailabilityStatus::Ready);
            if !still_ready {
                self.selected_default = None;
            }
        }
        cx.notify();
    }

    fn import_from_shell_env(&mut self, agent: AgentType, cx: &mut Context<Self>) {
        let Some(row) = self.rows.iter().find(|r| r.agent == agent).cloned() else {
            return;
        };
        let provider = row.provider_id.clone();
        let mut imported = Vec::new();
        let mut missing = Vec::new();
        for req in &row.required_env {
            if req.present {
                continue;
            }
            match std::env::var(&req.key) {
                Ok(v) if !v.is_empty() => {
                    if let Err(e) = self.credentials.set(&provider, &req.key, &v) {
                        log::warn!(
                            "failed to save {}:{} to credential store: {e}",
                            provider,
                            req.key,
                        );
                        missing.push(req.key.clone());
                    } else {
                        imported.push(req.key.clone());
                    }
                }
                _ => missing.push(req.key.clone()),
            }
        }
        self.last_message = Some(SharedString::from(format!(
            "[{}] imported {} • missing {}",
            row.display_name,
            if imported.is_empty() {
                "—".to_string()
            } else {
                imported.join(", ")
            },
            if missing.is_empty() {
                "—".to_string()
            } else {
                missing.join(", ")
            },
        )));
        self.refresh(cx);
    }

    fn select_default(&mut self, agent: AgentType, cx: &mut Context<Self>) {
        // Only ready harnesses can be the default — clicking a not-ready
        // row is a no-op rather than an error.
        let can_pick = self
            .rows
            .iter()
            .any(|r| r.agent == agent && r.status() == AvailabilityStatus::Ready);
        if !can_pick {
            return;
        }
        self.selected_default = Some(agent);
        cx.notify();
    }

    fn can_continue(&self) -> bool {
        self.selected_default.is_some()
    }

    fn complete(&mut self, cx: &mut Context<Self>) {
        if let Some(ref d) = self.selected_default {
            cx.emit(OnboardingCompleted {
                default_harness: d.clone(),
            });
        }
    }

    fn render_row(
        &self,
        row: &HarnessAvailability,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let status = row.status();
        let (badge_label, badge_color) = match status {
            AvailabilityStatus::Ready => ("ready", theme.success),
            AvailabilityStatus::NeedsCredentials => ("needs credentials", theme.warning),
            AvailabilityStatus::NotInstalled => ("not installed", theme.error),
        };

        let is_selected_default = self
            .selected_default
            .as_ref()
            .map(|d| *d == row.agent)
            .unwrap_or(false);

        let radio_color = if is_selected_default {
            theme.accent
        } else if status == AvailabilityStatus::Ready {
            theme.text_muted
        } else {
            theme.border
        };

        let mut row_el = div()
            .id(SharedString::from(format!("onboarding-row-{}", row.provider_id)))
            .flex()
            .flex_col()
            .gap_1()
            .p_3()
            .rounded_md()
            .bg(rgb(theme.bg_primary))
            .border_1()
            .border_color(rgb(if is_selected_default {
                theme.accent
            } else {
                theme.border
            }));

        // Header: radio + name + version + badge
        row_el = row_el.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_3()
                .child(
                    // Radio disc
                    div()
                        .size(px(12.0))
                        .rounded_full()
                        .border_1()
                        .border_color(rgb(radio_color))
                        .child(if is_selected_default {
                            div()
                                .size_full()
                                .rounded_full()
                                .bg(rgb(theme.accent))
                                .into_any_element()
                        } else {
                            div().into_any_element()
                        }),
                )
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(theme.text_primary))
                        .child(SharedString::from(row.display_name.clone())),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(theme.text_muted))
                        .child(SharedString::from(
                            row.version.clone().unwrap_or_default(),
                        )),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .px_2()
                        .py_0p5()
                        .rounded_md()
                        .bg(rgb(theme.bg_secondary))
                        .text_xs()
                        .font_weight(FontWeight::BOLD)
                        .text_color(rgb(badge_color))
                        .child(badge_label),
                ),
        );

        // Sub-line: env vars + install hint
        let env_line: String = if row.required_env.is_empty() {
            "no credentials required".to_string()
        } else {
            row.required_env
                .iter()
                .map(|e| {
                    if e.present {
                        format!("{} ✓", e.key)
                    } else {
                        format!("{} ✗", e.key)
                    }
                })
                .collect::<Vec<_>>()
                .join("  ")
        };
        row_el = row_el.child(
            div()
                .text_xs()
                .font_family("Lilex")
                .text_color(rgb(theme.text_secondary))
                .child(SharedString::from(env_line)),
        );

        if let Some(hint) = row.install_hint.as_ref() {
            if !row.installed {
                row_el = row_el.child(
                    div()
                        .text_xs()
                        .font_family("Lilex")
                        .text_color(rgb(theme.text_muted))
                        .child(SharedString::from(format!("install: {hint}"))),
                );
            }
        }

        // Actions row: Import from env (if needed), Pick as default (if ready)
        let mut actions = div().flex().flex_row().gap_2().mt_1();

        if status == AvailabilityStatus::NeedsCredentials {
            let agent_clone = row.agent.clone();
            actions = actions.child(
                div()
                    .id(SharedString::from(format!(
                        "import-btn-{}",
                        row.provider_id
                    )))
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(theme.bg_secondary))
                    .border_1()
                    .border_color(rgb(theme.border))
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(theme.warning))
                    .hover(|s| s.bg(rgb(theme.bg_hover)))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _evt, _win, cx| {
                        this.import_from_shell_env(agent_clone.clone(), cx);
                    }))
                    .child("Import from shell env"),
            );
        }

        if status == AvailabilityStatus::Ready {
            let agent_clone = row.agent.clone();
            let label = if is_selected_default {
                "✓ default"
            } else {
                "Set as default"
            };
            actions = actions.child(
                div()
                    .id(SharedString::from(format!(
                        "default-btn-{}",
                        row.provider_id
                    )))
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(if is_selected_default {
                        theme.accent
                    } else {
                        theme.bg_secondary
                    }))
                    .border_1()
                    .border_color(rgb(theme.border))
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(if is_selected_default {
                        theme.bg_primary
                    } else {
                        theme.text_primary
                    }))
                    .hover(|s| s.bg(rgb(theme.bg_hover)))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _evt, _win, cx| {
                        this.select_default(agent_clone.clone(), cx);
                    }))
                    .child(label),
            );
        }

        row_el.child(actions).into_any_element()
    }
}

impl Render for OnboardingView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::dark();
        let can_continue = self.can_continue();
        let row_count = self.rows.len();

        let rows_rendered: Vec<AnyElement> = (0..row_count)
            .map(|i| {
                let row = self.rows[i].clone();
                self.render_row(&row, &theme, cx)
            })
            .collect();

        let message = self
            .last_message
            .clone()
            .unwrap_or_else(|| SharedString::from(""));

        // Full-screen dim backdrop + centered card.
        div()
            .id("onboarding-backdrop")
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::hsla(0.0, 0.0, 0.0, 0.55))
            .child(
                div()
                    .id("onboarding-card")
                    .flex()
                    .flex_col()
                    .w(px(640.0))
                    .max_h(px(720.0))
                    .bg(rgb(theme.bg_secondary))
                    .border_1()
                    .border_color(rgb(theme.border))
                    .rounded_md()
                    .p_5()
                    .gap_3()
                    // Header
                    .child(
                        div()
                            .text_base()
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(theme.text_primary))
                            .child("Configure agent harnesses"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(theme.text_muted))
                            .child(
                                "Cuartel probes your PATH for supported agent CLIs \
                                 and reads API keys from the keychain. Import \
                                 credentials from the shell environment (export \
                                 them in your terminal, then click Import), then \
                                 pick a default harness to launch new sessions.",
                            ),
                    )
                    // Rows
                    .child(
                        div()
                            .id("onboarding-rows")
                            .flex()
                            .flex_col()
                            .gap_2()
                            .max_h(px(440.0))
                            .overflow_y_scroll()
                            .children(rows_rendered),
                    )
                    // Status line (per-action feedback)
                    .child(
                        div()
                            .min_h(px(16.0))
                            .text_xs()
                            .font_family("Lilex")
                            .text_color(rgb(theme.text_muted))
                            .child(message),
                    )
                    // Footer: refresh + continue
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_2()
                            .justify_end()
                            .child(
                                div()
                                    .id("onboarding-refresh")
                                    .px_3()
                                    .py_1p5()
                                    .rounded_md()
                                    .bg(rgb(theme.bg_primary))
                                    .border_1()
                                    .border_color(rgb(theme.border))
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(theme.text_secondary))
                                    .hover(|s| s.bg(rgb(theme.bg_hover)))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _evt, _win, cx| {
                                        this.refresh(cx);
                                    }))
                                    .child("Refresh"),
                            )
                            .child(
                                div()
                                    .id("onboarding-continue")
                                    .px_4()
                                    .py_1p5()
                                    .rounded_md()
                                    .bg(rgb(if can_continue {
                                        theme.accent
                                    } else {
                                        theme.bg_primary
                                    }))
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(if can_continue {
                                        theme.bg_primary
                                    } else {
                                        theme.text_muted
                                    }))
                                    .when(can_continue, |s| {
                                        s.hover(|s| s.bg(rgb(theme.accent)))
                                            .cursor_pointer()
                                    })
                                    .on_click(cx.listener(|this, _evt, _win, cx| {
                                        if this.can_continue() {
                                            this.complete(cx);
                                        }
                                    }))
                                    .child("Save and continue"),
                            ),
                    ),
            )
    }
}
