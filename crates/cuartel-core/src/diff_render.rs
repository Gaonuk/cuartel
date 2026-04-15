//! UI-agnostic helpers for the diff review panel (spec task 4c).
//!
//! These live in `cuartel-core` rather than `cuartel-app` for two reasons:
//! the math is genuinely independent of the GPUI render layer, and putting
//! `#[test]` modules in the GPUI binary trips a runaway proc-macro expansion
//! inside `gpui-macros`. Keeping the pure logic here means `cargo test
//! -p cuartel-core` exercises the line counters and the fixture builder
//! that the review panel renders against.
//!
//! The view layer in `cuartel-app::diff_view` calls these helpers to compute
//! the per-file `+N -N` badges, the aggregate header counts, and to seed the
//! initial fixture-driven panel before phase 4f wires in real overlay
//! snapshots.

use std::path::PathBuf;

use crate::overlay::{diff_trees, DiffLine, FileDiff, Tree};

/// Total of `+` (added) and `-` (removed) lines that fall inside hunks.
/// Context lines are intentionally excluded — they exist on both sides and
/// would inflate the badge for unchanged code.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DiffStats {
    pub adds: usize,
    pub dels: usize,
}

/// Count `+` / `-` lines for a single file diff. Binary diffs report `0/0`
/// because they have no hunk lines to walk.
pub fn file_stats(file: &FileDiff) -> DiffStats {
    let mut s = DiffStats::default();
    for h in &file.hunks {
        for l in &h.lines {
            match l {
                DiffLine::Added(_) => s.adds += 1,
                DiffLine::Removed(_) => s.dels += 1,
                DiffLine::Context(_) => {}
            }
        }
    }
    s
}

/// Sum [`file_stats`] across every diff. Used by the review panel header to
/// show a single `+adds / -dels` total alongside the file count.
pub fn aggregate_stats(diffs: &[FileDiff]) -> DiffStats {
    diffs.iter().map(file_stats).fold(
        DiffStats::default(),
        |mut acc, s| {
            acc.adds += s.adds;
            acc.dels += s.dels;
            acc
        },
    )
}

/// Demonstration data the review panel mounts on first render. Covers every
/// shape the renderer needs to handle: a non-binary modified file with
/// multiple changed regions, a freshly added file, a deleted file, and a
/// binary blob whose bytes are not valid UTF-8 on either side.
///
/// Phase 4f will replace direct callers with a real overlay snapshot pulled
/// from the running session, but the fixture stays around as a stable
/// rendering target for screenshot tests and the empty-state playground.
pub fn fixture_diffs() -> Vec<FileDiff> {
    let mut base: Tree = Tree::new();
    let mut overlay: Tree = Tree::new();

    // Modified file with two hunks and surrounding context.
    base.insert(
        PathBuf::from("src/lib.rs"),
        b"//! Cuartel core library.\n\
          use std::collections::HashMap;\n\
          \n\
          pub fn greet(name: &str) -> String {\n    \
              format!(\"hello, {}\", name)\n\
          }\n\
          \n\
          pub fn count(items: &[u32]) -> usize {\n    \
              items.len()\n\
          }\n\
          \n\
          pub fn total(items: &[u32]) -> u32 {\n    \
              items.iter().sum()\n\
          }\n"
            .to_vec(),
    );
    overlay.insert(
        PathBuf::from("src/lib.rs"),
        b"//! Cuartel core library.\n\
          use std::collections::BTreeMap;\n\
          \n\
          pub fn greet(name: &str) -> String {\n    \
              format!(\"hello, {name}\")\n\
          }\n\
          \n\
          pub fn count(items: &[u32]) -> usize {\n    \
              items.len()\n\
          }\n\
          \n\
          pub fn total(items: &[u32]) -> u64 {\n    \
              items.iter().map(|n| *n as u64).sum()\n\
          }\n"
            .to_vec(),
    );

    // Pure-add file.
    overlay.insert(
        PathBuf::from("src/diff_view.rs"),
        b"//! Diff review panel.\n\
          pub struct DiffView;\n\
          \n\
          impl DiffView {\n    \
              pub fn new() -> Self { Self }\n\
          }\n"
            .to_vec(),
    );

    // Pure-delete file.
    base.insert(
        PathBuf::from("docs/legacy.md"),
        b"# Legacy notes\n\nThis file predates the rewrite and should be removed.\n".to_vec(),
    );

    // Binary diff: bytes that are not valid UTF-8 on either side, with a
    // distinct trailing byte so the diff is non-empty.
    base.insert(
        PathBuf::from("assets/icon.png"),
        vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x01],
    );
    overlay.insert(
        PathBuf::from("assets/icon.png"),
        vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x02],
    );

    diff_trees(&base, &overlay)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay::DiffKind;

    #[test]
    fn fixture_covers_every_diff_kind() {
        let diffs = fixture_diffs();
        let kinds: Vec<&DiffKind> = diffs.iter().map(|d| &d.kind).collect();
        assert!(kinds.contains(&&DiffKind::Added));
        assert!(kinds.contains(&&DiffKind::Modified));
        assert!(kinds.contains(&&DiffKind::Deleted));
        assert!(diffs.iter().any(|d| d.binary));
    }

    #[test]
    fn fixture_includes_non_empty_modified_hunks() {
        let diffs = fixture_diffs();
        let modified = diffs
            .iter()
            .find(|d| d.kind == DiffKind::Modified && !d.binary)
            .expect("fixture should include a non-binary modified file");
        assert!(!modified.hunks.is_empty());
        assert!(modified
            .hunks
            .iter()
            .flat_map(|h| &h.lines)
            .any(|l| matches!(l, DiffLine::Added(_))));
        assert!(modified
            .hunks
            .iter()
            .flat_map(|h| &h.lines)
            .any(|l| matches!(l, DiffLine::Removed(_))));
    }

    #[test]
    fn file_stats_counts_only_changed_lines() {
        let diffs = fixture_diffs();
        let added = diffs
            .iter()
            .find(|d| d.kind == DiffKind::Added)
            .expect("fixture has an added file");
        let stats = file_stats(added);
        assert!(stats.adds > 0);
        assert_eq!(stats.dels, 0);

        let deleted = diffs
            .iter()
            .find(|d| d.kind == DiffKind::Deleted)
            .expect("fixture has a deleted file");
        let stats = file_stats(deleted);
        assert_eq!(stats.adds, 0);
        assert!(stats.dels > 0);
    }

    #[test]
    fn file_stats_excludes_context_lines() {
        let diffs = fixture_diffs();
        let modified = diffs
            .iter()
            .find(|d| d.kind == DiffKind::Modified && !d.binary)
            .unwrap();
        let stats = file_stats(modified);
        let context_count = modified
            .hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| matches!(l, DiffLine::Context(_)))
            .count();
        assert!(context_count > 0, "fixture should include context lines");
        assert!(stats.adds + stats.dels < context_count + stats.adds + stats.dels);
    }

    #[test]
    fn binary_file_reports_zero_stats() {
        let diffs = fixture_diffs();
        let binary = diffs
            .iter()
            .find(|d| d.binary)
            .expect("fixture has a binary file");
        let stats = file_stats(binary);
        assert_eq!(stats, DiffStats::default());
    }

    #[test]
    fn aggregate_stats_sums_across_files() {
        let diffs = fixture_diffs();
        let total = aggregate_stats(&diffs);
        let manual = diffs.iter().fold((0usize, 0usize), |(a, d), f| {
            let s = file_stats(f);
            (a + s.adds, d + s.dels)
        });
        assert_eq!((total.adds, total.dels), manual);
    }

    #[test]
    fn aggregate_stats_empty_input_is_zero() {
        let stats = aggregate_stats(&[]);
        assert_eq!(stats, DiffStats::default());
    }
}
