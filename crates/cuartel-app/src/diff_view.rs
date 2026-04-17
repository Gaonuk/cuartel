//! Diff review panel UI (spec tasks 4c + 4f).
//!
//! Two-pane review surface with per-file and per-hunk accept/reject
//! checkboxes. The header carries aggregate stats and action buttons
//! (Accept All / Reject All / Apply). Clicking Apply emits a
//! [`ReviewApply`] event carrying the user's decisions as
//! [`FileReview`] values that the parent can feed into
//! `cuartel_core::review::plan_review`.

use crate::theme::Theme;
use cuartel_core::diff_render::{aggregate_stats, file_stats};
use cuartel_core::overlay::{DiffKind, DiffLine, FileDiff};
use cuartel_core::review::FileReview;
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::collections::BTreeSet;

pub use cuartel_core::diff_render::fixture_diffs;

// --- Events ---------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct ReviewApply {
    pub decisions: Vec<FileReview>,
}

impl EventEmitter<ReviewApply> for DiffView {}

// --- Selection state ------------------------------------------------------

#[derive(Debug, Clone)]
struct FileAcceptState {
    accepted: Vec<bool>,
}

impl FileAcceptState {
    fn new(hunk_count: usize) -> Self {
        Self {
            accepted: vec![true; hunk_count],
        }
    }

    fn all_accepted(&self) -> bool {
        self.accepted.iter().all(|a| *a)
    }

    fn none_accepted(&self) -> bool {
        self.accepted.iter().all(|a| !*a)
    }

    fn toggle_hunk(&mut self, idx: usize) {
        if let Some(v) = self.accepted.get_mut(idx) {
            *v = !*v;
        }
    }

    fn set_all(&mut self, val: bool) {
        self.accepted.iter_mut().for_each(|a| *a = val);
    }

    fn to_accepted_set(&self) -> BTreeSet<usize> {
        self.accepted
            .iter()
            .enumerate()
            .filter_map(|(i, a)| if *a { Some(i) } else { None })
            .collect()
    }
}

// --- DiffView -------------------------------------------------------------

pub struct DiffView {
    diffs: Vec<FileDiff>,
    states: Vec<FileAcceptState>,
    selected_index: Option<usize>,
}

impl DiffView {
    pub fn new(diffs: Vec<FileDiff>, _cx: &mut Context<Self>) -> Self {
        let states = diffs
            .iter()
            .map(|d| FileAcceptState::new(d.hunks.len().max(1)))
            .collect();
        let selected_index = if diffs.is_empty() { None } else { Some(0) };
        Self {
            diffs,
            states,
            selected_index,
        }
    }

    #[allow(dead_code)]
    pub fn with_fixtures(cx: &mut Context<Self>) -> Self {
        Self::new(fixture_diffs(), cx)
    }

    #[allow(dead_code)]
    pub fn set_diffs(&mut self, diffs: Vec<FileDiff>, cx: &mut Context<Self>) {
        self.states = diffs
            .iter()
            .map(|d| FileAcceptState::new(d.hunks.len().max(1)))
            .collect();
        self.selected_index = if diffs.is_empty() { None } else { Some(0) };
        self.diffs = diffs;
        cx.notify();
    }

    pub fn diffs(&self) -> &[FileDiff] {
        &self.diffs
    }

    pub fn len(&self) -> usize {
        self.diffs.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.diffs.is_empty()
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.diffs.len() || self.selected_index == Some(index) {
            return;
        }
        self.selected_index = Some(index);
        cx.notify();
    }

    fn toggle_file(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(state) = self.states.get_mut(index) {
            let new_val = !state.all_accepted();
            state.set_all(new_val);
            cx.notify();
        }
    }

    fn toggle_hunk(&mut self, file_idx: usize, hunk_idx: usize, cx: &mut Context<Self>) {
        if let Some(state) = self.states.get_mut(file_idx) {
            state.toggle_hunk(hunk_idx);
            cx.notify();
        }
    }

    fn accept_all(&mut self, cx: &mut Context<Self>) {
        for state in &mut self.states {
            state.set_all(true);
        }
        cx.notify();
    }

    fn reject_all(&mut self, cx: &mut Context<Self>) {
        for state in &mut self.states {
            state.set_all(false);
        }
        cx.notify();
    }

    fn any_accepted(&self) -> bool {
        self.states.iter().any(|s| !s.none_accepted())
    }

    fn apply(&mut self, cx: &mut Context<Self>) {
        let decisions: Vec<FileReview> = self
            .states
            .iter()
            .enumerate()
            .filter(|(_, s)| !s.none_accepted())
            .map(|(i, s)| FileReview {
                file_index: i,
                accepted_hunks: s.to_accepted_set(),
            })
            .collect();
        if !decisions.is_empty() {
            cx.emit(ReviewApply { decisions });
        }
    }
}

impl Render for DiffView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::dark();

        if self.diffs.is_empty() {
            return div()
                .id("diff-view-empty")
                .flex()
                .flex_1()
                .items_center()
                .justify_center()
                .bg(rgb(theme.bg_primary))
                .text_color(rgb(theme.text_muted))
                .text_sm()
                .child("No pending changes")
                .into_any_element();
        }

        let total = aggregate_stats(&self.diffs);
        let can_apply = self.any_accepted();

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
                    .gap_3()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(theme.text_secondary))
                            .child(SharedString::from(format!(
                                "{} {} changed",
                                self.diffs.len(),
                                if self.diffs.len() == 1 { "file" } else { "files" }
                            ))),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_3()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(theme.success))
                                    .child(SharedString::from(format!("+{}", total.adds))),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(theme.error))
                                    .child(SharedString::from(format!("-{}", total.dels))),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(action_button(
                        "accept-all",
                        "Accept All",
                        theme.success,
                        &theme,
                        cx,
                        |this, cx| this.accept_all(cx),
                    ))
                    .child(action_button(
                        "reject-all",
                        "Reject All",
                        theme.error,
                        &theme,
                        cx,
                        |this, cx| this.reject_all(cx),
                    ))
                    .child(apply_button(can_apply, &theme, cx)),
            );

        let selected_index = self.selected_index;
        let file_rows: Vec<AnyElement> = self
            .diffs
            .iter()
            .enumerate()
            .map(|(idx, file)| {
                let file_state = &self.states[idx];
                self.render_file_row(
                    idx,
                    file,
                    file_state,
                    selected_index == Some(idx),
                    &theme,
                    cx,
                )
            })
            .collect();

        let file_list = div()
            .id("diff-file-list")
            .flex()
            .flex_col()
            .w(px(260.0))
            .h_full()
            .bg(rgb(theme.bg_sidebar))
            .border_r_1()
            .border_color(rgb(theme.border))
            .overflow_y_scroll()
            .py_1()
            .children(file_rows);

        let detail: AnyElement =
            match selected_index.and_then(|i| self.diffs.get(i).zip(self.states.get(i))) {
                Some((file, state)) => {
                    let fi = selected_index.unwrap();
                    render_file_detail(fi, file, state, &theme, cx).into_any_element()
                }
                None => div()
                    .id("diff-detail-empty")
                    .flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .text_color(rgb(theme.text_muted))
                    .text_sm()
                    .child("Select a file")
                    .into_any_element(),
            };

        div()
            .id("diff-view")
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
                    .child(file_list)
                    .child(
                        div()
                            .id("diff-detail")
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

// --- File row with checkbox -----------------------------------------------

impl DiffView {
    fn render_file_row(
        &self,
        index: usize,
        file: &FileDiff,
        file_state: &FileAcceptState,
        selected: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let stats = file_stats(file);
        let (glyph, glyph_color) = kind_glyph(&file.kind, theme);
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
        let path_label = SharedString::from(file.path.display().to_string());
        let row_id = ElementId::Name(format!("diff-row-{index}").into());
        let cb_id = ElementId::Name(format!("diff-cb-{index}").into());

        let all = file_state.all_accepted();
        let none = file_state.none_accepted();
        let check_glyph = if all {
            "\u{2611}" // ☑
        } else if none {
            "\u{2610}" // ☐
        } else {
            "\u{229F}" // ⊟ (mixed)
        };
        let check_color = if none { theme.text_muted } else { theme.accent };

        div()
            .id(row_id)
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .mx_1()
            .px_1()
            .py_1p5()
            .rounded_md()
            .bg(rgb(bg))
            .hover(move |s| s.bg(rgb(hover_bg)))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _evt, _win, cx| this.select(index, cx)))
            .child(
                div()
                    .id(cb_id)
                    .flex_none()
                    .w(px(18.0))
                    .h(px(18.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .text_color(rgb(check_color))
                    .rounded_sm()
                    .hover(|s| s.bg(rgb(0x45475a)))
                    .on_click(
                        cx.listener(move |this, _evt, _win, cx| this.toggle_file(index, cx)),
                    )
                    .child(SharedString::from(check_glyph)),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(12.0))
                    .text_xs()
                    .font_family("Lilex")
                    .font_weight(FontWeight::BOLD)
                    .text_color(rgb(glyph_color))
                    .child(SharedString::from(glyph.to_string())),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .text_color(rgb(theme.text_primary))
                    .font_weight(if selected {
                        FontWeight::SEMIBOLD
                    } else {
                        FontWeight::NORMAL
                    })
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(path_label),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_1()
                    .flex_none()
                    .child(
                        div()
                            .text_xs()
                            .font_family("Lilex")
                            .text_color(rgb(theme.success))
                            .child(SharedString::from(format!("+{}", stats.adds))),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_family("Lilex")
                            .text_color(rgb(theme.error))
                            .child(SharedString::from(format!("-{}", stats.dels))),
                    ),
            )
            .into_any_element()
    }
}

// --- File detail with per-hunk checkboxes ---------------------------------

fn render_file_detail(
    file_idx: usize,
    file: &FileDiff,
    state: &FileAcceptState,
    theme: &Theme,
    cx: &mut Context<DiffView>,
) -> Div {
    let stats = file_stats(file);
    let (glyph, glyph_color) = kind_glyph(&file.kind, theme);
    let path_label = SharedString::from(file.path.display().to_string());

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
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .font_family("Lilex")
                        .font_weight(FontWeight::BOLD)
                        .text_color(rgb(glyph_color))
                        .child(SharedString::from(glyph.to_string())),
                )
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(theme.text_primary))
                        .child(path_label),
                ),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .gap_3()
                .child(
                    div()
                        .text_xs()
                        .font_family("Lilex")
                        .text_color(rgb(theme.success))
                        .child(SharedString::from(format!("+{}", stats.adds))),
                )
                .child(
                    div()
                        .text_xs()
                        .font_family("Lilex")
                        .text_color(rgb(theme.error))
                        .child(SharedString::from(format!("-{}", stats.dels))),
                ),
        );

    if file.binary {
        return div()
            .flex()
            .flex_col()
            .child(header)
            .child(
                div()
                    .px_4()
                    .py_3()
                    .text_sm()
                    .text_color(rgb(theme.text_muted))
                    .font_family("Lilex")
                    .child("Binary files differ"),
            );
    }

    let mut hunks: Vec<AnyElement> = Vec::new();
    for (hunk_idx, hunk) in file.hunks.iter().enumerate() {
        let accepted = state
            .accepted
            .get(hunk_idx)
            .copied()
            .unwrap_or(true);

        let check_glyph = if accepted { "\u{2611}" } else { "\u{2610}" };
        let check_color = if accepted { theme.accent } else { theme.text_muted };
        let cb_id = ElementId::Name(format!("hunk-cb-{file_idx}-{hunk_idx}").into());

        let header_text = SharedString::from(format!(
            "@@ -{},{} +{},{} @@",
            hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count
        ));

        let dimmed = !accepted;

        hunks.push(
            div()
                .flex()
                .flex_col()
                .mt_2()
                .when(dimmed, |el| el.opacity(0.4))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .px_3()
                        .py_1()
                        .bg(rgb(theme.bg_secondary))
                        .border_t_1()
                        .border_b_1()
                        .border_color(rgb(theme.border))
                        .child(
                            div()
                                .id(cb_id)
                                .flex_none()
                                .w(px(16.0))
                                .h(px(16.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_xs()
                                .text_color(rgb(check_color))
                                .rounded_sm()
                                .cursor_pointer()
                                .hover(|s| s.bg(rgb(0x45475a)))
                                .on_click(cx.listener(move |this, _evt, _win, cx| {
                                    this.toggle_hunk(file_idx, hunk_idx, cx)
                                }))
                                .child(SharedString::from(check_glyph)),
                        )
                        .child(
                            div()
                                .text_xs()
                                .font_family("Lilex")
                                .text_color(rgb(theme.text_muted))
                                .child(header_text),
                        ),
                )
                .children(render_hunk_lines(
                    hunk.old_start,
                    hunk.new_start,
                    &hunk.lines,
                    theme,
                ))
                .into_any_element(),
        );
    }

    div().flex().flex_col().child(header).children(hunks)
}

// --- Shared rendering helpers ---------------------------------------------

fn render_hunk_lines(
    old_start: usize,
    new_start: usize,
    lines: &[DiffLine],
    theme: &Theme,
) -> Vec<AnyElement> {
    let mut old_no = old_start;
    let mut new_no = new_start;
    let mut out: Vec<AnyElement> = Vec::with_capacity(lines.len());

    for line in lines {
        let (marker, text, bg, fg, old_label, new_label, advance_old, advance_new) = match line {
            DiffLine::Context(s) => (
                ' ',
                s.clone(),
                theme.bg_primary,
                theme.text_secondary,
                line_no(old_no),
                line_no(new_no),
                true,
                true,
            ),
            DiffLine::Added(s) => (
                '+',
                s.clone(),
                blend(theme.bg_primary, theme.success),
                theme.text_primary,
                "    ".to_string(),
                line_no(new_no),
                false,
                true,
            ),
            DiffLine::Removed(s) => (
                '-',
                s.clone(),
                blend(theme.bg_primary, theme.error),
                theme.text_primary,
                line_no(old_no),
                "    ".to_string(),
                true,
                false,
            ),
        };
        if advance_old {
            old_no += 1;
        }
        if advance_new {
            new_no += 1;
        }

        out.push(
            div()
                .flex()
                .flex_row()
                .items_start()
                .bg(rgb(bg))
                .px_2()
                .child(
                    div()
                        .flex_none()
                        .w(px(36.0))
                        .text_xs()
                        .font_family("Lilex")
                        .text_color(rgb(theme.text_muted))
                        .child(SharedString::from(old_label)),
                )
                .child(
                    div()
                        .flex_none()
                        .w(px(36.0))
                        .text_xs()
                        .font_family("Lilex")
                        .text_color(rgb(theme.text_muted))
                        .child(SharedString::from(new_label)),
                )
                .child(
                    div()
                        .flex_none()
                        .w(px(14.0))
                        .text_xs()
                        .font_family("Lilex")
                        .text_color(rgb(fg))
                        .child(SharedString::from(marker.to_string())),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .font_family("Lilex")
                        .text_color(rgb(fg))
                        .child(SharedString::from(text)),
                )
                .into_any_element(),
        );
    }
    out
}

fn action_button(
    id: &'static str,
    label: &'static str,
    accent: u32,
    theme: &Theme,
    cx: &mut Context<DiffView>,
    handler: fn(&mut DiffView, &mut Context<DiffView>),
) -> Stateful<Div> {
    div()
        .id(id)
        .px_2()
        .py(px(2.0))
        .rounded_md()
        .border_1()
        .border_color(rgb(accent))
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(accent))
        .cursor_pointer()
        .hover(|s| s.bg(rgb(theme.bg_hover)))
        .on_click(cx.listener(move |this, _evt, _win, cx| handler(this, cx)))
        .child(label)
}

fn apply_button(enabled: bool, theme: &Theme, cx: &mut Context<DiffView>) -> Stateful<Div> {
    let bg = if enabled { theme.accent } else { theme.bg_hover };
    let fg = if enabled { 0x1e1e2e } else { theme.text_muted };
    div()
        .id("apply-btn")
        .px_3()
        .py(px(2.0))
        .rounded_md()
        .bg(rgb(bg))
        .text_xs()
        .font_weight(FontWeight::BOLD)
        .text_color(rgb(fg))
        .when(enabled, |el| {
            el.cursor_pointer()
                .hover(|s| s.opacity(0.85))
        })
        .when(enabled, |el| {
            el.on_click(cx.listener(|this, _evt, _win, cx| this.apply(cx)))
        })
        .child("Apply")
}

fn kind_glyph(kind: &DiffKind, theme: &Theme) -> (char, u32) {
    match kind {
        DiffKind::Added => ('A', theme.success),
        DiffKind::Modified => ('M', theme.warning),
        DiffKind::Deleted => ('D', theme.error),
    }
}

fn line_no(n: usize) -> String {
    format!("{n:>4}")
}

fn blend(base: u32, tint: u32) -> u32 {
    let (br, bg, bb) = ((base >> 16) & 0xff, (base >> 8) & 0xff, base & 0xff);
    let (tr, tg, tb) = ((tint >> 16) & 0xff, (tint >> 8) & 0xff, tint & 0xff);
    let mix = |a: u32, b: u32| -> u32 { (a * 78 + b * 22) / 100 };
    let r = mix(br, tr);
    let g = mix(bg, tg);
    let b = mix(bb, tb);
    (r << 16) | (g << 8) | b
}
