//! Port forwarding management panel (spec task 5e).
//!
//! Workspace tab that displays configured port forwards for the active
//! session and provides controls to add, remove, and toggle them. The
//! panel is data-driven: callers push entries via [`PortsPanel::set_entries`]
//! and subscribe to the events emitted when the user triggers an action.

use crate::theme::Theme;
use cuartel_rivet::network::PortForwardDirection;
use gpui::prelude::FluentBuilder;
use gpui::*;

// --- Events ------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct PortForwardAdd {
    pub direction: PortForwardDirection,
    pub sandbox_port: u16,
    pub host_port: u16,
}

#[derive(Clone, Debug)]
pub struct PortForwardRemove {
    pub id: String,
}

#[derive(Clone, Debug)]
pub struct PortForwardToggle {
    pub id: String,
    pub enabled: bool,
}

impl EventEmitter<PortForwardAdd> for PortsPanel {}
impl EventEmitter<PortForwardRemove> for PortsPanel {}
impl EventEmitter<PortForwardToggle> for PortsPanel {}

// --- Display model -----------------------------------------------------------

#[derive(Clone, Debug)]
pub struct PortEntry {
    pub id: String,
    pub direction: PortForwardDirection,
    pub sandbox_port: u16,
    pub host_port: u16,
    pub enabled: bool,
}

// --- Panel -------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum InputField {
    SandboxPort,
    HostPort,
}

pub struct PortsPanel {
    entries: Vec<PortEntry>,
    new_direction: PortForwardDirection,
    new_sandbox_port: String,
    new_host_port: String,
    active_field: Option<InputField>,
    #[allow(dead_code)]
    session_id: String,
    focus_handle: FocusHandle,
}

impl PortsPanel {
    pub fn new(session_id: String, cx: &mut Context<Self>) -> Self {
        Self {
            entries: Vec::new(),
            new_direction: PortForwardDirection::HostToSandbox,
            new_sandbox_port: String::new(),
            new_host_port: String::new(),
            active_field: None,
            session_id,
            focus_handle: cx.focus_handle(),
        }
    }

    pub fn set_entries(&mut self, entries: Vec<PortEntry>, cx: &mut Context<Self>) {
        self.entries = entries;
        cx.notify();
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn toggle_direction(&mut self, cx: &mut Context<Self>) {
        self.new_direction = match self.new_direction {
            PortForwardDirection::SandboxToHost => PortForwardDirection::HostToSandbox,
            PortForwardDirection::HostToSandbox => PortForwardDirection::SandboxToHost,
        };
        cx.notify();
    }

    fn focus_field(&mut self, field: InputField, cx: &mut Context<Self>) {
        self.active_field = Some(field);
        cx.notify();
    }

    fn can_add(&self) -> bool {
        self.new_sandbox_port.parse::<u16>().map_or(false, |p| p > 0)
            && self.new_host_port.parse::<u16>().map_or(false, |p| p > 0)
    }

    fn try_add(&mut self, cx: &mut Context<Self>) {
        let sandbox = match self.new_sandbox_port.parse::<u16>() {
            Ok(p) if p > 0 => p,
            _ => return,
        };
        let host = match self.new_host_port.parse::<u16>() {
            Ok(p) if p > 0 => p,
            _ => return,
        };
        cx.emit(PortForwardAdd {
            direction: self.new_direction,
            sandbox_port: sandbox,
            host_port: host,
        });
        self.new_sandbox_port.clear();
        self.new_host_port.clear();
        self.active_field = None;
        cx.notify();
    }

    fn remove(&mut self, id: String, cx: &mut Context<Self>) {
        cx.emit(PortForwardRemove { id });
    }

    fn toggle(&mut self, id: String, enabled: bool, cx: &mut Context<Self>) {
        cx.emit(PortForwardToggle { id, enabled });
    }

    fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(field) = self.active_field else {
            return;
        };
        let ks = &event.keystroke;
        let mods = &ks.modifiers;

        if mods.control || mods.alt || mods.platform {
            return;
        }

        match ks.key.as_str() {
            "enter" => {
                self.try_add(cx);
                return;
            }
            "tab" => {
                self.active_field = Some(match field {
                    InputField::SandboxPort => InputField::HostPort,
                    InputField::HostPort => InputField::SandboxPort,
                });
                cx.notify();
                return;
            }
            "escape" => {
                self.active_field = None;
                cx.notify();
                return;
            }
            "backspace" => {
                let target = match field {
                    InputField::SandboxPort => &mut self.new_sandbox_port,
                    InputField::HostPort => &mut self.new_host_port,
                };
                target.pop();
                cx.notify();
                return;
            }
            _ => {}
        }

        if let Some(ch) = ks.key_char.as_ref() {
            let s = ch.as_str();
            if s.len() == 1
                && s.chars()
                    .next()
                    .map(|c| c.is_ascii_digit())
                    .unwrap_or(false)
            {
                let target = match field {
                    InputField::SandboxPort => &mut self.new_sandbox_port,
                    InputField::HostPort => &mut self.new_host_port,
                };
                if target.len() < 5 {
                    target.push_str(s);
                    cx.notify();
                }
            }
        }
    }

    fn render_add_form(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let direction = self.new_direction;
        let sandbox_text = if self.new_sandbox_port.is_empty() {
            SharedString::from("port")
        } else {
            SharedString::from(self.new_sandbox_port.clone())
        };
        let host_text = if self.new_host_port.is_empty() {
            SharedString::from("port")
        } else {
            SharedString::from(self.new_host_port.clone())
        };
        let sandbox_active = self.active_field == Some(InputField::SandboxPort);
        let host_active = self.active_field == Some(InputField::HostPort);
        let can_add = self.can_add();

        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .h(px(40.0))
            .px_3()
            .bg(rgb(theme.bg_secondary))
            .border_b_1()
            .border_color(rgb(theme.border))
            .child(
                div()
                    .id("port-direction")
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(theme.bg_primary))
                    .border_1()
                    .border_color(rgb(theme.accent))
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(theme.accent))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(theme.bg_hover)))
                    .on_click(cx.listener(|this, _evt, _win, cx| {
                        this.toggle_direction(cx);
                    }))
                    .child(SharedString::from(direction.label())),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(theme.text_muted))
                    .child("sandbox:"),
            )
            .child(port_input_field(
                "port-sandbox",
                sandbox_text,
                sandbox_active,
                self.new_sandbox_port.is_empty(),
                theme,
                cx,
                InputField::SandboxPort,
            ))
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(theme.text_muted))
                    .child("\u{2194}"),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(theme.text_muted))
                    .child("host:"),
            )
            .child(port_input_field(
                "port-host",
                host_text,
                host_active,
                self.new_host_port.is_empty(),
                theme,
                cx,
                InputField::HostPort,
            ))
            .child(div().flex_1())
            .child(
                div()
                    .id("port-add")
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(if can_add {
                        theme.accent
                    } else {
                        theme.bg_primary
                    }))
                    .border_1()
                    .border_color(rgb(if can_add {
                        theme.accent
                    } else {
                        theme.border
                    }))
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(if can_add {
                        theme.bg_primary
                    } else {
                        theme.text_muted
                    }))
                    .when(can_add, |el| {
                        el.cursor_pointer()
                            .hover(|s| s.bg(rgb(theme.bg_hover)))
                            .on_click(cx.listener(|this, _evt, _win, cx| {
                                this.try_add(cx);
                            }))
                    })
                    .child("Add"),
            )
    }

    fn render_entry_row(
        &self,
        entry: &PortEntry,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let enabled = entry.enabled;
        let (status_label, status_color) = if enabled {
            ("active", theme.success)
        } else {
            ("paused", theme.text_muted)
        };

        let direction_label = entry.direction.label();
        let port_label = format!(":{} \u{2194} :{}", entry.sandbox_port, entry.host_port);

        let toggle_id = entry.id.clone();
        let remove_id = entry.id.clone();

        div()
            .id(SharedString::from(format!("pf-row-{}", entry.id)))
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .px_3()
            .py_2()
            .rounded_md()
            .bg(rgb(theme.bg_secondary))
            .border_1()
            .border_color(rgb(if enabled {
                theme.border
            } else {
                theme.bg_hover
            }))
            .child(
                div()
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .bg(rgb(theme.bg_primary))
                    .text_xs()
                    .text_color(rgb(theme.accent))
                    .child(SharedString::from(direction_label)),
            )
            .child(
                div()
                    .text_sm()
                    .font_family("Lilex")
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(if enabled {
                        theme.text_primary
                    } else {
                        theme.text_muted
                    }))
                    .child(SharedString::from(port_label)),
            )
            .child(div().flex_1())
            .child(
                div()
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .bg(rgb(theme.bg_primary))
                    .text_xs()
                    .font_weight(FontWeight::BOLD)
                    .text_color(rgb(status_color))
                    .child(status_label),
            )
            .child(
                div()
                    .id(SharedString::from(format!("pf-toggle-{}", entry.id)))
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(theme.border))
                    .text_xs()
                    .text_color(rgb(theme.text_secondary))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(theme.bg_hover)))
                    .on_click(cx.listener(move |this, _evt, _win, cx| {
                        this.toggle(toggle_id.clone(), !enabled, cx);
                    }))
                    .child(if enabled { "Pause" } else { "Resume" }),
            )
            .child(
                div()
                    .id(SharedString::from(format!("pf-remove-{}", entry.id)))
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(theme.border))
                    .text_xs()
                    .text_color(rgb(theme.error))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(theme.bg_hover)))
                    .on_click(cx.listener(move |this, _evt, _win, cx| {
                        this.remove(remove_id.clone(), cx);
                    }))
                    .child("Remove"),
            )
            .into_any_element()
    }
}

impl Focusable for PortsPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PortsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::dark();
        let count = self.entries.len();

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
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(theme.text_secondary))
                    .child(SharedString::from(format!(
                        "{count} port forward{}",
                        if count == 1 { "" } else { "s" }
                    ))),
            );

        let add_form = self.render_add_form(&theme, cx);

        let rows: Vec<AnyElement> = (0..self.entries.len())
            .map(|i| {
                let entry = self.entries[i].clone();
                self.render_entry_row(&entry, &theme, cx)
            })
            .collect();

        let body = if rows.is_empty() {
            div()
                .id("ports-empty")
                .flex()
                .flex_1()
                .flex_col()
                .items_center()
                .justify_center()
                .text_color(rgb(theme.text_muted))
                .text_sm()
                .child("No port forwards configured")
                .child(
                    div()
                        .text_xs()
                        .mt_1()
                        .text_color(rgb(theme.text_muted))
                        .child(
                            "Add a forward above to expose ports between sandbox and host",
                        ),
                )
                .into_any_element()
        } else {
            div()
                .id("ports-list")
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .py_2()
                .px_3()
                .gap_2()
                .children(rows)
                .into_any_element()
        };

        div()
            .id("ports-panel")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::handle_key_down))
            .flex()
            .flex_col()
            .flex_1()
            .bg(rgb(theme.bg_primary))
            .font_family("IBM Plex Sans")
            .child(header)
            .child(add_form)
            .child(body)
    }
}

fn port_input_field(
    id: &'static str,
    text: SharedString,
    active: bool,
    is_placeholder: bool,
    theme: &Theme,
    cx: &mut Context<PortsPanel>,
    field: InputField,
) -> Stateful<Div> {
    div()
        .id(id)
        .w(px(56.0))
        .px_2()
        .py_1()
        .rounded_md()
        .bg(rgb(theme.bg_primary))
        .border_1()
        .border_color(rgb(if active { theme.accent } else { theme.border }))
        .text_xs()
        .font_family("Lilex")
        .text_color(rgb(if is_placeholder {
            theme.text_muted
        } else {
            theme.text_primary
        }))
        .cursor_pointer()
        .on_click(cx.listener(move |this, _evt, _win, cx| {
            this.focus_field(field, cx);
        }))
        .child(text)
}
