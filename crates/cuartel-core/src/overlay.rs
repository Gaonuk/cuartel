use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use similar::{ChangeTag, TextDiff};

/// A "tree" is a flat mapping of relative path → file content bytes.
///
/// This matches how we snapshot an overlay filesystem: every regular file is
/// listed explicitly, directories are implicit. BTreeMap keeps iteration
/// deterministic so `diff_trees` output is stable across runs.
pub type Tree = BTreeMap<PathBuf, Vec<u8>>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiffKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDiff {
    pub path: PathBuf,
    pub kind: DiffKind,
    /// `true` when either side is not valid UTF-8. Binary diffs have no hunks.
    pub binary: bool,
    pub hunks: Vec<DiffHunk>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffHunk {
    /// 1-based line number in the old file where this hunk starts.
    /// `0` when `old_count == 0` (added file), per unified-diff convention.
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiffLine {
    Context(String),
    Added(String),
    Removed(String),
}

/// Pure function: compute file-level diffs between a base tree and an overlay tree.
///
/// Paths present in both sides with identical bytes are omitted. Text diffs use
/// Myers' algorithm via the `similar` crate with 3 lines of context per hunk.
/// Any file whose bytes are not valid UTF-8 on either side is marked `binary`
/// with an empty hunk list.
pub fn diff_trees(base: &Tree, overlay: &Tree) -> Vec<FileDiff> {
    let mut out: Vec<FileDiff> = Vec::new();

    for (path, new_bytes) in overlay {
        match base.get(path) {
            None => out.push(build_file_diff(path, &[], new_bytes, DiffKind::Added)),
            Some(old_bytes) => {
                if old_bytes != new_bytes {
                    out.push(build_file_diff(
                        path,
                        old_bytes,
                        new_bytes,
                        DiffKind::Modified,
                    ));
                }
            }
        }
    }
    for (path, old_bytes) in base {
        if !overlay.contains_key(path) {
            out.push(build_file_diff(path, old_bytes, &[], DiffKind::Deleted));
        }
    }

    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

fn build_file_diff(path: &Path, old: &[u8], new: &[u8], kind: DiffKind) -> FileDiff {
    match (std::str::from_utf8(old), std::str::from_utf8(new)) {
        (Ok(o), Ok(n)) => FileDiff {
            path: path.to_path_buf(),
            kind,
            binary: false,
            hunks: text_hunks(o, n),
        },
        _ => FileDiff {
            path: path.to_path_buf(),
            kind,
            binary: true,
            hunks: Vec::new(),
        },
    }
}

fn text_hunks(old: &str, new: &str) -> Vec<DiffHunk> {
    let diff = TextDiff::from_lines(old, new);
    let mut hunks = Vec::new();

    for group in diff.grouped_ops(3) {
        if group.is_empty() {
            continue;
        }

        let first = group.first().unwrap();
        let last = group.last().unwrap();
        let old_lo = first.old_range().start;
        let old_hi = last.old_range().end;
        let new_lo = first.new_range().start;
        let new_hi = last.new_range().end;

        let old_count = old_hi - old_lo;
        let new_count = new_hi - new_lo;

        let mut lines: Vec<DiffLine> = Vec::new();
        for op in &group {
            for change in diff.iter_changes(op) {
                let value = strip_trailing_newline(change.value());
                let line = match change.tag() {
                    ChangeTag::Equal => DiffLine::Context(value),
                    ChangeTag::Insert => DiffLine::Added(value),
                    ChangeTag::Delete => DiffLine::Removed(value),
                };
                lines.push(line);
            }
        }

        hunks.push(DiffHunk {
            old_start: if old_count == 0 { 0 } else { old_lo + 1 },
            old_count,
            new_start: if new_count == 0 { 0 } else { new_lo + 1 },
            new_count,
            lines,
        });
    }

    hunks
}

fn strip_trailing_newline(s: &str) -> String {
    s.strip_suffix("\r\n")
        .or_else(|| s.strip_suffix('\n'))
        .unwrap_or(s)
        .to_string()
}

/// Render a [`FileDiff`] as a git-style unified diff block (one file header +
/// hunks). Useful for logs, fixture snapshots, and the review panel.
pub fn to_unified_string(diff: &FileDiff) -> String {
    let path = diff.path.display();
    let mut out = String::new();

    let (old_label, new_label) = match diff.kind {
        DiffKind::Added => ("/dev/null".to_string(), format!("b/{path}")),
        DiffKind::Deleted => (format!("a/{path}"), "/dev/null".to_string()),
        DiffKind::Modified => (format!("a/{path}"), format!("b/{path}")),
    };

    let _ = writeln!(out, "--- {old_label}");
    let _ = writeln!(out, "+++ {new_label}");

    if diff.binary {
        let _ = writeln!(out, "Binary files differ");
        return out;
    }

    for hunk in &diff.hunks {
        let _ = writeln!(
            out,
            "@@ -{},{} +{},{} @@",
            hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count
        );
        for line in &hunk.lines {
            match line {
                DiffLine::Context(s) => {
                    let _ = writeln!(out, " {s}");
                }
                DiffLine::Added(s) => {
                    let _ = writeln!(out, "+{s}");
                }
                DiffLine::Removed(s) => {
                    let _ = writeln!(out, "-{s}");
                }
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree<const N: usize>(entries: [(&str, &[u8]); N]) -> Tree {
        entries
            .into_iter()
            .map(|(p, b)| (PathBuf::from(p), b.to_vec()))
            .collect()
    }

    #[test]
    fn empty_trees_produce_no_diff() {
        assert!(diff_trees(&Tree::new(), &Tree::new()).is_empty());
    }

    #[test]
    fn identical_trees_produce_no_diff() {
        let t = tree([("a.txt", b"hello\n"), ("b.txt", b"world\n")]);
        assert!(diff_trees(&t, &t.clone()).is_empty());
    }

    #[test]
    fn added_file_is_reported() {
        let base = Tree::new();
        let overlay = tree([("new.txt", b"line1\nline2\n")]);
        let diffs = diff_trees(&base, &overlay);
        assert_eq!(diffs.len(), 1);
        let d = &diffs[0];
        assert_eq!(d.path, PathBuf::from("new.txt"));
        assert_eq!(d.kind, DiffKind::Added);
        assert!(!d.binary);
        assert_eq!(d.hunks.len(), 1);
        let h = &d.hunks[0];
        assert_eq!(h.old_start, 0);
        assert_eq!(h.old_count, 0);
        assert_eq!(h.new_start, 1);
        assert_eq!(h.new_count, 2);
        assert_eq!(
            h.lines,
            vec![
                DiffLine::Added("line1".into()),
                DiffLine::Added("line2".into()),
            ]
        );
    }

    #[test]
    fn deleted_file_is_reported() {
        let base = tree([("gone.txt", b"only\n")]);
        let overlay = Tree::new();
        let diffs = diff_trees(&base, &overlay);
        assert_eq!(diffs.len(), 1);
        let d = &diffs[0];
        assert_eq!(d.kind, DiffKind::Deleted);
        let h = &d.hunks[0];
        assert_eq!(h.old_start, 1);
        assert_eq!(h.old_count, 1);
        assert_eq!(h.new_start, 0);
        assert_eq!(h.new_count, 0);
        assert_eq!(h.lines, vec![DiffLine::Removed("only".into())]);
    }

    #[test]
    fn modified_file_captures_context_and_changes() {
        let base = tree([("f.txt", b"a\nb\nc\nd\ne\n")]);
        let overlay = tree([("f.txt", b"a\nb\nC\nd\ne\n")]);
        let diffs = diff_trees(&base, &overlay);
        assert_eq!(diffs.len(), 1);
        let d = &diffs[0];
        assert_eq!(d.kind, DiffKind::Modified);
        assert_eq!(d.hunks.len(), 1);
        let h = &d.hunks[0];
        assert!(h.lines.contains(&DiffLine::Removed("c".into())));
        assert!(h.lines.contains(&DiffLine::Added("C".into())));
        assert!(h.lines.contains(&DiffLine::Context("a".into())));
        assert!(h.lines.contains(&DiffLine::Context("e".into())));
    }

    #[test]
    fn unchanged_files_are_omitted_from_mixed_tree() {
        let base = tree([("keep.txt", b"same\n"), ("edit.txt", b"old\n")]);
        let overlay = tree([("keep.txt", b"same\n"), ("edit.txt", b"new\n")]);
        let diffs = diff_trees(&base, &overlay);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].path, PathBuf::from("edit.txt"));
    }

    #[test]
    fn output_is_sorted_by_path() {
        let base = tree([("z.txt", b"z\n"), ("a.txt", b"a\n")]);
        let overlay = tree([("z.txt", b"Z\n"), ("a.txt", b"A\n"), ("m.txt", b"m\n")]);
        let diffs = diff_trees(&base, &overlay);
        let paths: Vec<_> = diffs.iter().map(|d| d.path.clone()).collect();
        assert_eq!(
            paths,
            vec![
                PathBuf::from("a.txt"),
                PathBuf::from("m.txt"),
                PathBuf::from("z.txt"),
            ]
        );
    }

    #[test]
    fn binary_file_is_flagged_without_hunks() {
        let base = tree([("img.bin", &[0x00, 0xff, 0xfe][..])]);
        let overlay = tree([("img.bin", &[0x00, 0xff, 0xfd][..])]);
        let diffs = diff_trees(&base, &overlay);
        assert_eq!(diffs.len(), 1);
        assert!(diffs[0].binary);
        assert!(diffs[0].hunks.is_empty());
        assert_eq!(diffs[0].kind, DiffKind::Modified);
    }

    #[test]
    fn modified_then_deleted_file_is_deleted() {
        let base = tree([("x.txt", b"one\n")]);
        let overlay = Tree::new();
        let diffs = diff_trees(&base, &overlay);
        assert_eq!(diffs[0].kind, DiffKind::Deleted);
    }

    #[test]
    fn unified_string_renders_added_file_against_dev_null() {
        let base = Tree::new();
        let overlay = tree([("new.txt", b"hi\n")]);
        let diffs = diff_trees(&base, &overlay);
        let s = to_unified_string(&diffs[0]);
        assert!(s.starts_with("--- /dev/null\n+++ b/new.txt\n"));
        assert!(s.contains("@@ -0,0 +1,1 @@"));
        assert!(s.contains("+hi"));
    }

    #[test]
    fn unified_string_renders_binary_marker() {
        let base = tree([("b.bin", &[0xff][..])]);
        let overlay = tree([("b.bin", &[0xfe][..])]);
        let diffs = diff_trees(&base, &overlay);
        let s = to_unified_string(&diffs[0]);
        assert!(s.contains("Binary files differ"));
    }

    #[test]
    fn crlf_line_endings_do_not_leak_into_output() {
        let base = tree([("f.txt", b"a\r\nb\r\n")]);
        let overlay = tree([("f.txt", b"a\r\nB\r\n")]);
        let diffs = diff_trees(&base, &overlay);
        let h = &diffs[0].hunks[0];
        for line in &h.lines {
            let s = match line {
                DiffLine::Context(s) | DiffLine::Added(s) | DiffLine::Removed(s) => s,
            };
            assert!(!s.ends_with('\r'), "line still has CR: {s:?}");
            assert!(!s.ends_with('\n'), "line still has LF: {s:?}");
        }
    }
}
