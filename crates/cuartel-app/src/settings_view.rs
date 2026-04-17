//! Settings modal for managing API keys and default harness (spec task 5b).
//!
//! Renders a harness matrix similar to the onboarding view but accessible
//! on demand via the sidebar gear button. Supports importing credentials
//! from the shell environment, deleting stored credentials, and changing
//! the default harness.

use crate::theme::Theme;
use cuartel_core::agent::{AgentType, HarnessRegistry};
use cuartel_core::availability::{
    probe_registry, AvailabilityStatus, HarnessAvailability, WhichProbe,
};
use cuartel_core::credential_store::CredentialStore;
use futures::executor::block_on;
use gpui::*;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct SettingsDismissed {
    pub default_harness: Option<AgentType>,
}

pub struct SettingsView {
    registry: Arc<HarnessRegistry>,
    credentials: Arc<dyn CredentialStore>,
    rows: Vec<HarnessAvailability>,
    selected_default: Option<AgentType>,
    last_message: Option<SharedString>,
}

impl EventEmitter<SettingsDismissed> for SettingsView {}

impl SettingsView {
    pub fn new(
        registry: Arc<HarnessRegistry>,
        credentials: Arc<dyn CredentialStore>,
        current_default: Option<AgentType>,
        _cx: &mut Context<Self>,
    ) -> Self {
        let rows = block_on(probe_registry(
            registry.as_ref(),
            &WhichProbe,
            credentials.as_ref(),
        ));
        Self {
            registry,
            credentials,
            rows,
            selected_default: current_default,
            last_message: None,
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.rows = block_on(probe_registry(
            self.registry.as_ref(),
            &WhichProbe,
            self.credentials.as_ref(),
        ));
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
                "\u{2014}".to_string()
            } else {
                imported.join(", ")
            },
            if missing.is_empty() {
                "\u{2014}".to_string()
            } else {
                missing.join(", ")
            },
        )));
        self.refresh(cx);
    }

    fn delete_credential(
        &mut self,
        provider: String,
        key: String,
        cx: &mut Context<Self>,
    ) {
        match self.credentials.delete(&provider, &key) {
            Ok(()) => {
                self.last_message = Some(SharedString::from(format!(
                    "Deleted {provider}:{key}",
                )));
            }
            Err(e) => {
                log::warn!("failed to delete {provider}:{key}: {e}");
                self.last_message = Some(SharedString::from(format!(
                    "Failed to delete {provider}:{key}",
                )));
            }
        }
        self.refresh(cx);
    }

    fn select_default(&mut self, agent: AgentType, cx: &mut Context<Self>) {
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

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        cx.emit(SettingsDismissed {
            default_harness: self.selected_default.clone(),
        });
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
            .id(SharedString::from(format!(
                "settings-row-{}",
                row.agent.rivet_name()
            )))
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

        // Env var status line
        let env_line: String = if row.required_env.is_empty() {
            "no credentials required".to_string()
        } else {
            row.required_env
                .iter()
                .map(|e| {
                    if e.present {
                        format!("{} \u{2713}", e.key)
                    } else {
                        format!("{} \u{2717}", e.key)
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

        // Actions row
        let mut actions = div().flex().flex_row().gap_2().mt_1();

        // Import button for harnesses that need credentials
        if status == AvailabilityStatus::NeedsCredentials {
            let agent_clone = row.agent.clone();
            actions = actions.child(
                div()
                    .id(SharedString::from(format!(
                        "settings-import-{}",
                        row.agent.rivet_name()
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

        // Delete buttons for credentials that are present
        for req in &row.required_env {
            if req.present {
                let provider = row.provider_id.clone();
                let key = req.key.clone();
                actions = actions.child(
                    div()
                        .id(SharedString::from(format!(
                            "settings-del-{}-{}",
                            row.agent.rivet_name(),
                            req.key,
                        )))
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .bg(rgb(theme.bg_secondary))
                        .border_1()
                        .border_color(rgb(theme.border))
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(theme.error))
                        .hover(|s| s.bg(rgb(theme.bg_hover)))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _evt, _win, cx| {
                            this.delete_credential(
                                provider.clone(),
                                key.clone(),
                                cx,
                            );
                        }))
                        .child(SharedString::from(format!(
                            "Delete {}",
                            req.key
                        ))),
                );
            }
        }

        // Default picker button for ready harnesses
        if status == AvailabilityStatus::Ready {
            let agent_clone = row.agent.clone();
            let label = if is_selected_default {
                "\u{2713} default"
            } else {
                "Set as default"
            };
            actions = actions.child(
                div()
                    .id(SharedString::from(format!(
                        "settings-default-{}",
                        row.agent.rivet_name()
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

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::dark();
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

        div()
            .id("settings-backdrop")
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::hsla(0.0, 0.0, 0.0, 0.55))
            .child(
                div()
                    .id("settings-card")
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
                            .child("Settings"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(theme.text_muted))
                            .child(
                                "Manage API keys and choose the default agent \
                                 harness for new sessions. Import credentials \
                                 from your shell environment or delete stored \
                                 keys. Changes to the default harness take \
                                 effect on new sessions.",
                            ),
                    )
                    // Section label
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(theme.text_muted))
                            .child("CREDENTIALS & HARNESSES"),
                    )
                    // Rows
                    .child(
                        div()
                            .id("settings-rows")
                            .flex()
                            .flex_col()
                            .gap_2()
                            .max_h(px(420.0))
                            .overflow_y_scroll()
                            .children(rows_rendered),
                    )
                    // Status line
                    .child(
                        div()
                            .min_h(px(16.0))
                            .text_xs()
                            .font_family("Lilex")
                            .text_color(rgb(theme.text_muted))
                            .child(message),
                    )
                    // Footer
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_2()
                            .justify_end()
                            .child(
                                div()
                                    .id("settings-refresh")
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
                                    .id("settings-close")
                                    .px_4()
                                    .py_1p5()
                                    .rounded_md()
                                    .bg(rgb(theme.accent))
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(theme.bg_primary))
                                    .hover(|s| s.bg(rgb(theme.bg_hover)))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _evt, _win, cx| {
                                        this.dismiss(cx);
                                    }))
                                    .child("Close"),
                            ),
                    ),
            )
    }
}
