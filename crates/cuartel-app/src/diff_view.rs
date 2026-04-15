//! Diff review panel UI (spec task 4c).
//!
//! Pure UI built against fixture diffs from `cuartel-core::overlay`. The
//! component takes a `Vec<FileDiff>` (or any source that produces one) and
//! renders a two-pane review surface:
//!
//! * Left: a sortable list of changed files with a status glyph
//!   (`A`/`M`/`D`), the path, and a per-file `+adds / -dels` counter.
//! * Right: the hunks of the currently-selected file. Each hunk shows the
//!   standard `@@ -old,n +new,n @@` header followed by lines coloured by
//!   kind. A two-column gutter on the left of each line carries the old /
//!   new line numbers so reviewers can map a hunk back into the source.
//!
//! Phase 4c is the "fixture-driven" milestone — the panel renders without
//! any VM, sidecar, or rivet round trip. Phase 4f will swap the fixture for
//! a real overlay snapshot and wire the (still TODO) accept/reject buttons
//! into the host filesystem application path.

use crate::theme::Theme;
use cuartel_core::diff_render::{aggregate_stats, file_stats};
use cuartel_core::overlay::{DiffKind, DiffLine, FileDiff};
use gpui::*;

// Re-export so callers (e.g. `app.rs`) can grab the fixture data through
// the same module they import `DiffView` from.
pub use cuartel_core::diff_render::fixture_diffs;

pub struct DiffView {
    diffs: Vec<FileDiff>,
    selected_index: Option<usize>,
}

impl DiffView {
    pub fn new(diffs: Vec<FileDiff>, _cx: &mut Context<Self>) -> Self {
        let selected_index = if diffs.is_empty() { None } else { Some(0) };
        Self {
            diffs,
            selected_index,
        }
    }

    /// Convenience for tests / playground entries that want a `DiffView`
    /// preloaded with the fixture data. Phase 4f will replace direct
    /// callers with real overlay snapshots.
    #[allow(dead_code)]
    pub fn with_fixtures(cx: &mut Context<Self>) -> Self {
        Self::new(fixture_diffs(), cx)
    }

    /// Replace the current diff set. Used by 4f when an overlay snapshot
    /// arrives from the running session; not yet wired from anywhere in 4c.
    #[allow(dead_code)]
    pub fn set_diffs(&mut self, diffs: Vec<FileDiff>, cx: &mut Context<Self>) {
        self.selected_index = if diffs.is_empty() { None } else { Some(0) };
        self.diffs = diffs;
        cx.notify();
    }

    pub fn len(&self) -> usize {
        self.diffs.len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.diffs.is_empty()
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.diffs.len() {
            return;
        }
        if self.selected_index == Some(index) {
            return;
        }
        self.selected_index = Some(index);
        cx.notify();
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
            );

        let selected_index = self.selected_index;
        let file_rows: Vec<AnyElement> = self
            .diffs
            .iter()
            .enumerate()
            .map(|(idx, file)| {
                self.render_file_row(idx, file, selected_index == Some(idx), &theme, cx)
            })
            .collect();

        let file_list = div()
            .id("diff-file-list")
            .flex()
            .flex_col()
            .w(px(240.0))
            .h_full()
            .bg(rgb(theme.bg_sidebar))
            .border_r_1()
            .border_color(rgb(theme.border))
            .overflow_y_scroll()
            .py_1()
            .children(file_rows);

        let detail: AnyElement = match selected_index.and_then(|i| self.diffs.get(i)) {
            Some(file) => render_file_detail(file, &theme).into_any_element(),
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

impl DiffView {
    fn render_file_row(
        &self,
        index: usize,
        file: &FileDiff,
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

        div()
            .id(row_id)
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
            .on_click(cx.listener(move |this, _evt, _win, cx| this.select(index, cx)))
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

fn render_file_detail(file: &FileDiff, theme: &Theme) -> Div {
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
    for hunk in &file.hunks {
        let header_text = SharedString::from(format!(
            "@@ -{},{} +{},{} @@",
            hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count
        ));
        hunks.push(
            div()
                .flex()
                .flex_col()
                .mt_2()
                .child(
                    div()
                        .px_3()
                        .py_1()
                        .bg(rgb(theme.bg_secondary))
                        .border_t_1()
                        .border_b_1()
                        .border_color(rgb(theme.border))
                        .text_xs()
                        .font_family("Lilex")
                        .text_color(rgb(theme.text_muted))
                        .child(header_text),
                )
                .children(render_hunk_lines(hunk.old_start, hunk.new_start, &hunk.lines, theme))
                .into_any_element(),
        );
    }

    div().flex().flex_col().child(header).children(hunks)
}

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

/// Mix `base` toward `tint` by ~22% to produce the soft +/- background bands.
/// Operates on packed `0xRRGGBB` ints so callers can stay in the existing
/// `Theme: u32` palette without going through `Hsla` plumbing.
fn blend(base: u32, tint: u32) -> u32 {
    let (br, bg, bb) = ((base >> 16) & 0xff, (base >> 8) & 0xff, base & 0xff);
    let (tr, tg, tb) = ((tint >> 16) & 0xff, (tint >> 8) & 0xff, tint & 0xff);
    let mix = |a: u32, b: u32| -> u32 { (a * 78 + b * 22) / 100 };
    let r = mix(br, tr);
    let g = mix(bg, tg);
    let b = mix(bb, tb);
    (r << 16) | (g << 8) | b
}

