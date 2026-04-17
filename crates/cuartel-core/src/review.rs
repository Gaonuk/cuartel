//! Accept/reject review logic (spec task 4f).
//!
//! Given a set of [`FileDiff`]s and per-file/per-hunk accept decisions,
//! this module computes the target file contents and applies them to the
//! host filesystem.
//!
//! The core operation — [`apply_hunks`] — is a pure function over strings
//! and hunk descriptors. It reconstructs the target file by walking the
//! original text forward through each hunk region:
//!
//! * **Accepted hunks** emit their new-side lines (context + added,
//!   skipping removed).
//! * **Rejected hunks** emit their old-side lines (context + removed,
//!   skipping added) — identical to the original text in that region.
//! * **Inter-hunk gaps** copy verbatim from the original.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

use crate::overlay::{DiffHunk, DiffKind, DiffLine, FileDiff};

/// Per-file review decision.
///
/// `accepted_hunks` contains the zero-based indices of hunks the user
/// wants applied. An empty set means "reject entire file". When all
/// hunks are present it is equivalent to "accept file".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileReview {
    pub file_index: usize,
    pub accepted_hunks: BTreeSet<usize>,
}

impl FileReview {
    pub fn accept_all(file_index: usize, hunk_count: usize) -> Self {
        Self {
            file_index,
            accepted_hunks: (0..hunk_count).collect(),
        }
    }

    pub fn reject_all(file_index: usize) -> Self {
        Self {
            file_index,
            accepted_hunks: BTreeSet::new(),
        }
    }
}

/// One entry in a [`ReviewPlan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewAction {
    /// Write `content` to `path` (relative to workspace root).
    Write { path: PathBuf, content: Vec<u8> },
    /// Delete `path` (relative to workspace root).
    Delete { path: PathBuf },
}

/// The computed set of filesystem mutations to execute.
#[derive(Debug, Clone, Default)]
pub struct ReviewPlan {
    pub actions: Vec<ReviewAction>,
    pub skipped: Vec<PathBuf>,
}

/// Build a [`ReviewPlan`] from diffs and review decisions.
///
/// `host_root` is the workspace directory on the host. File paths in
/// `diffs` are relative and resolved against it for reading the current
/// base content. The plan itself contains relative paths — the caller
/// resolves them against `host_root` at execution time.
///
/// Files whose `file_index` does not appear in any `FileReview` are
/// treated as fully rejected (no changes applied).
pub fn plan_review(
    diffs: &[FileDiff],
    decisions: &[FileReview],
    host_root: &Path,
) -> Result<ReviewPlan> {
    let mut plan = ReviewPlan::default();

    for decision in decisions {
        let diff = diffs
            .get(decision.file_index)
            .ok_or_else(|| anyhow!("file_index {} out of range", decision.file_index))?;

        if decision.accepted_hunks.is_empty() {
            plan.skipped.push(diff.path.clone());
            continue;
        }

        match diff.kind {
            DiffKind::Added => {
                let content = reconstruct_added(diff);
                plan.actions.push(ReviewAction::Write {
                    path: diff.path.clone(),
                    content,
                });
            }
            DiffKind::Deleted => {
                plan.actions.push(ReviewAction::Delete {
                    path: diff.path.clone(),
                });
            }
            DiffKind::Modified => {
                if diff.binary {
                    plan.skipped.push(diff.path.clone());
                    continue;
                }
                let host_path = host_root.join(&diff.path);
                let original = fs::read_to_string(&host_path).with_context(|| {
                    format!("read original file {}", host_path.display())
                })?;
                let result = apply_hunks(&original, &diff.hunks, &decision.accepted_hunks);
                plan.actions.push(ReviewAction::Write {
                    path: diff.path.clone(),
                    content: result.into_bytes(),
                });
            }
        }
    }

    Ok(plan)
}

/// Execute a [`ReviewPlan`] against the host filesystem.
pub fn execute_review(plan: &ReviewPlan, host_root: &Path) -> Result<ExecutionReport> {
    let mut report = ExecutionReport::default();

    for action in &plan.actions {
        match action {
            ReviewAction::Write { path, content } => {
                let full = host_root.join(path);
                if let Some(parent) = full.parent() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("mkdir -p {}", parent.display()))?;
                }
                fs::write(&full, content)
                    .with_context(|| format!("write {}", full.display()))?;
                report.files_written += 1;
            }
            ReviewAction::Delete { path } => {
                let full = host_root.join(path);
                if full.exists() {
                    fs::remove_file(&full)
                        .with_context(|| format!("delete {}", full.display()))?;
                    report.files_deleted += 1;
                }
            }
        }
    }

    report.files_skipped = plan.skipped.len();
    Ok(report)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecutionReport {
    pub files_written: usize,
    pub files_deleted: usize,
    pub files_skipped: usize,
}

/// Reconstruct the full content of an added file from its diff hunks.
fn reconstruct_added(diff: &FileDiff) -> Vec<u8> {
    let mut out = String::new();
    for hunk in &diff.hunks {
        for line in &hunk.lines {
            match line {
                DiffLine::Added(s) | DiffLine::Context(s) => {
                    out.push_str(s);
                    out.push('\n');
                }
                DiffLine::Removed(_) => {}
            }
        }
    }
    out.into_bytes()
}

/// Apply selected hunks to `original` text, returning the result.
///
/// Hunks whose index appears in `accepted` have their new-side lines
/// emitted. All other hunks emit their old-side lines (which exactly
/// reproduce the original text in that span). Inter-hunk regions are
/// copied verbatim from the original.
///
/// Precondition: `hunks` are sorted by `old_start` and non-overlapping
/// (guaranteed by `similar`'s `grouped_ops`).
pub fn apply_hunks(original: &str, hunks: &[DiffHunk], accepted: &BTreeSet<usize>) -> String {
    let lines: Vec<&str> = original.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut cursor: usize = 0; // 0-based index into `lines`

    for (hunk_idx, hunk) in hunks.iter().enumerate() {
        let hunk_start = if hunk.old_count == 0 {
            hunk.new_start.saturating_sub(1)
        } else {
            hunk.old_start - 1
        };

        if hunk_start > cursor {
            for line in &lines[cursor..hunk_start] {
                out.push((*line).to_string());
            }
        }

        if accepted.contains(&hunk_idx) {
            for line in &hunk.lines {
                match line {
                    DiffLine::Context(s) | DiffLine::Added(s) => {
                        out.push(s.clone());
                    }
                    DiffLine::Removed(_) => {}
                }
            }
        } else {
            for line in &hunk.lines {
                match line {
                    DiffLine::Context(s) | DiffLine::Removed(s) => {
                        out.push(s.clone());
                    }
                    DiffLine::Added(_) => {}
                }
            }
        }

        cursor = if hunk.old_count == 0 {
            hunk_start
        } else {
            (hunk.old_start - 1) + hunk.old_count
        };
    }

    if cursor < lines.len() {
        for line in &lines[cursor..] {
            out.push((*line).to_string());
        }
    }

    let mut result = out.join("\n");
    if !result.is_empty()
        && !result.ends_with('\n')
        && (original.is_empty() || original.ends_with('\n'))
    {
        result.push('\n');
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay::{diff_trees, Tree};
    use std::path::PathBuf;

    fn tree<const N: usize>(entries: [(&str, &[u8]); N]) -> Tree {
        entries
            .into_iter()
            .map(|(p, b)| (PathBuf::from(p), b.to_vec()))
            .collect()
    }

    // --- apply_hunks tests ---

    #[test]
    fn accept_all_hunks_reproduces_new_file() {
        let old = "a\nb\nc\nd\ne\n";
        let new = "a\nb\nC\nd\ne\n";
        let base = tree([("f.txt", old.as_bytes())]);
        let overlay = tree([("f.txt", new.as_bytes())]);
        let diffs = diff_trees(&base, &overlay);
        let diff = &diffs[0];
        let accepted: BTreeSet<usize> = (0..diff.hunks.len()).collect();
        let result = apply_hunks(old, &diff.hunks, &accepted);
        assert_eq!(result, new);
    }

    #[test]
    fn reject_all_hunks_reproduces_original() {
        let old = "a\nb\nc\nd\ne\n";
        let new = "a\nb\nC\nd\ne\n";
        let base = tree([("f.txt", old.as_bytes())]);
        let overlay = tree([("f.txt", new.as_bytes())]);
        let diffs = diff_trees(&base, &overlay);
        let diff = &diffs[0];
        let accepted: BTreeSet<usize> = BTreeSet::new();
        let result = apply_hunks(old, &diff.hunks, &accepted);
        assert_eq!(result, old);
    }

    #[test]
    fn accept_first_hunk_only() {
        // Two separate changes far enough apart to produce two hunks.
        let old = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n13\n14\n15\n";
        let new = "1\n2\n3\nFOUR\n5\n6\n7\n8\n9\n10\n11\n12\n13\nFOURTEEN\n15\n";
        let base = tree([("f.txt", old.as_bytes())]);
        let overlay = tree([("f.txt", new.as_bytes())]);
        let diffs = diff_trees(&base, &overlay);
        let diff = &diffs[0];
        assert!(
            diff.hunks.len() >= 2,
            "expected 2 hunks, got {}",
            diff.hunks.len()
        );

        // Accept only the first hunk (line 4 change).
        let accepted: BTreeSet<usize> = [0].into();
        let result = apply_hunks(old, &diff.hunks, &accepted);
        assert!(result.contains("FOUR\n"), "first hunk should be applied");
        assert!(
            !result.contains("FOURTEEN"),
            "second hunk should NOT be applied"
        );
        assert!(result.contains("14\n"), "original line 14 should remain");
    }

    #[test]
    fn accept_second_hunk_only() {
        let old = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n13\n14\n15\n";
        let new = "1\n2\n3\nFOUR\n5\n6\n7\n8\n9\n10\n11\n12\n13\nFOURTEEN\n15\n";
        let base = tree([("f.txt", old.as_bytes())]);
        let overlay = tree([("f.txt", new.as_bytes())]);
        let diffs = diff_trees(&base, &overlay);
        let diff = &diffs[0];
        assert!(diff.hunks.len() >= 2);

        let accepted: BTreeSet<usize> = [1].into();
        let result = apply_hunks(old, &diff.hunks, &accepted);
        assert!(result.contains("4\n"), "original line 4 should remain");
        assert!(
            result.contains("FOURTEEN"),
            "second hunk should be applied"
        );
    }

    #[test]
    fn apply_hunks_empty_original() {
        let old = "";
        let new = "hello\nworld\n";
        let base = tree([("f.txt", old.as_bytes())]);
        let overlay = tree([("f.txt", new.as_bytes())]);
        let diffs = diff_trees(&base, &overlay);
        let diff = &diffs[0];
        let accepted: BTreeSet<usize> = (0..diff.hunks.len()).collect();
        let result = apply_hunks(old, &diff.hunks, &accepted);
        assert_eq!(result, new);
    }

    #[test]
    fn preserves_trailing_newline() {
        let old = "a\nb\n";
        let new = "a\nB\n";
        let base = tree([("f.txt", old.as_bytes())]);
        let overlay = tree([("f.txt", new.as_bytes())]);
        let diffs = diff_trees(&base, &overlay);
        let diff = &diffs[0];
        let accepted: BTreeSet<usize> = (0..diff.hunks.len()).collect();
        let result = apply_hunks(old, &diff.hunks, &accepted);
        assert!(result.ends_with('\n'));
    }

    // --- FileReview helpers ---

    #[test]
    fn accept_all_populates_set() {
        let r = FileReview::accept_all(0, 5);
        assert_eq!(r.accepted_hunks.len(), 5);
        assert!(r.accepted_hunks.contains(&0));
        assert!(r.accepted_hunks.contains(&4));
    }

    #[test]
    fn reject_all_is_empty_set() {
        let r = FileReview::reject_all(0);
        assert!(r.accepted_hunks.is_empty());
    }

    // --- plan_review + execute_review integration ---

    #[test]
    fn plan_and_execute_added_file() {
        let dir = tempfile::tempdir().unwrap();
        let base = Tree::new();
        let overlay = tree([("new.txt", b"hello\nworld\n")]);
        let diffs = diff_trees(&base, &overlay);

        let decisions = vec![FileReview::accept_all(0, diffs[0].hunks.len())];
        let plan = plan_review(&diffs, &decisions, dir.path()).unwrap();
        assert_eq!(plan.actions.len(), 1);

        let report = execute_review(&plan, dir.path()).unwrap();
        assert_eq!(report.files_written, 1);

        let content = fs::read_to_string(dir.path().join("new.txt")).unwrap();
        assert!(content.contains("hello"));
    }

    #[test]
    fn plan_and_execute_deleted_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("gone.txt");
        fs::write(&file, b"original").unwrap();

        let base = tree([("gone.txt", b"original\n")]);
        let overlay = Tree::new();
        let diffs = diff_trees(&base, &overlay);

        let decisions = vec![FileReview::accept_all(0, diffs[0].hunks.len())];
        let plan = plan_review(&diffs, &decisions, dir.path()).unwrap();
        let report = execute_review(&plan, dir.path()).unwrap();
        assert_eq!(report.files_deleted, 1);
        assert!(!file.exists());
    }

    #[test]
    fn plan_and_execute_modified_with_partial_hunks() {
        let dir = tempfile::tempdir().unwrap();
        let old = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n13\n14\n15\n";
        let new = "1\n2\n3\nFOUR\n5\n6\n7\n8\n9\n10\n11\n12\n13\nFOURTEEN\n15\n";
        fs::write(dir.path().join("f.txt"), old).unwrap();

        let base = tree([("f.txt", old.as_bytes())]);
        let overlay = tree([("f.txt", new.as_bytes())]);
        let diffs = diff_trees(&base, &overlay);
        assert!(diffs[0].hunks.len() >= 2);

        let mut accepted = BTreeSet::new();
        accepted.insert(0); // only first hunk
        let decisions = vec![FileReview {
            file_index: 0,
            accepted_hunks: accepted,
        }];
        let plan = plan_review(&diffs, &decisions, dir.path()).unwrap();
        let report = execute_review(&plan, dir.path()).unwrap();
        assert_eq!(report.files_written, 1);

        let result = fs::read_to_string(dir.path().join("f.txt")).unwrap();
        assert!(result.contains("FOUR\n"));
        assert!(!result.contains("FOURTEEN"));
    }

    #[test]
    fn rejected_file_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let old = "original\n";
        fs::write(dir.path().join("f.txt"), old).unwrap();

        let base = tree([("f.txt", old.as_bytes())]);
        let overlay = tree([("f.txt", b"changed\n")]);
        let diffs = diff_trees(&base, &overlay);

        let decisions = vec![FileReview::reject_all(0)];
        let plan = plan_review(&diffs, &decisions, dir.path()).unwrap();
        assert!(plan.actions.is_empty());
        assert_eq!(plan.skipped.len(), 1);

        let report = execute_review(&plan, dir.path()).unwrap();
        assert_eq!(report.files_skipped, 1);
        let content = fs::read_to_string(dir.path().join("f.txt")).unwrap();
        assert_eq!(content, old);
    }

    #[test]
    fn binary_modified_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("img.bin"), &[0xff, 0xfe]).unwrap();

        let base = tree([("img.bin", &[0xff, 0xfe][..])]);
        let overlay = tree([("img.bin", &[0xff, 0xfd][..])]);
        let diffs = diff_trees(&base, &overlay);
        assert!(diffs[0].binary);

        let decisions = vec![FileReview {
            file_index: 0,
            accepted_hunks: [0].into(),
        }];
        let plan = plan_review(&diffs, &decisions, dir.path()).unwrap();
        assert!(plan.actions.is_empty());
        assert_eq!(plan.skipped.len(), 1);
    }

    #[test]
    fn execute_creates_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let base = Tree::new();
        let overlay = tree([("deep/nested/file.txt", b"content\n")]);
        let diffs = diff_trees(&base, &overlay);

        let decisions = vec![FileReview::accept_all(0, diffs[0].hunks.len())];
        let plan = plan_review(&diffs, &decisions, dir.path()).unwrap();
        execute_review(&plan, dir.path()).unwrap();

        assert!(dir.path().join("deep/nested/file.txt").exists());
    }

    #[test]
    fn deleted_file_already_missing_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        let base = tree([("gone.txt", b"x\n")]);
        let overlay = Tree::new();
        let diffs = diff_trees(&base, &overlay);

        let decisions = vec![FileReview::accept_all(0, diffs[0].hunks.len())];
        let plan = plan_review(&diffs, &decisions, dir.path()).unwrap();
        let report = execute_review(&plan, dir.path()).unwrap();
        assert_eq!(report.files_deleted, 0);
    }
}
