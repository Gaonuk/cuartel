//! Per-session git worktrees.
//!
//! Layout: `~/cuartel/worktrees/<workspace-name>/<session-id>/`,
//! branched from the workspace's HEAD as `cuartel/<session-id>`. Each
//! session gets its own isolated working tree so parallel sessions
//! never see each other's edits.
//!
//! Best-effort: callers pass an optional workspace path. If it's
//! `None`, not a directory, or not a git repo, the helpers no-op and
//! return `None`. The session still gets created — it just runs in
//! the workspace root (or whatever cwd the agent picks). Once
//! Workspace selection lands in Phase C3 every session will have a
//! real worktree.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Resolve `~/cuartel/worktrees/`. Falls back to `/tmp/cuartel/worktrees`
/// if the home dir is unavailable (CI / sandboxed contexts).
pub fn root() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join("cuartel/worktrees"))
        .unwrap_or_else(|| PathBuf::from("/tmp/cuartel/worktrees"))
}

fn workspace_name(path: &Path) -> String {
    path.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("workspace")
        .to_string()
}

fn target_for(workspace: &Path, session_id: &str) -> PathBuf {
    root().join(workspace_name(workspace)).join(session_id)
}

fn is_git_repo(path: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(path)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Create a git worktree for `session_id` rooted at the standard
/// per-workspace path. Returns the worktree path on success, `None`
/// when worktree creation is skipped (non-git workspace, no workspace
/// path) or fails. Errors are logged; the caller continues without
/// a worktree.
pub fn create_for(workspace: &Option<PathBuf>, session_id: &str) -> Option<PathBuf> {
    let workspace = workspace.as_ref()?;
    if !workspace.is_dir() {
        log::debug!(
            "[worktree] workspace {} is not a directory; skipping",
            workspace.display()
        );
        return None;
    }
    if !is_git_repo(workspace) {
        log::debug!(
            "[worktree] workspace {} is not a git repo; skipping",
            workspace.display()
        );
        return None;
    }

    let target = target_for(workspace, session_id);
    if let Some(parent) = target.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            log::warn!(
                "[worktree] failed to create parent dir {}: {e}",
                parent.display()
            );
            return None;
        }
    }

    let branch = format!("cuartel/{session_id}");
    // `git worktree add -b <branch> <path>` creates the branch from
    // current HEAD and checks it out at the target path. If the branch
    // already exists (re-creation after a botched cleanup), retry
    // without -b.
    let output = Command::new("git")
        .args([
            "worktree",
            "add",
            "-b",
            &branch,
            target.to_str().unwrap_or(""),
        ])
        .current_dir(workspace)
        .output();

    match output {
        Ok(o) if o.status.success() => {
            log::info!(
                "[worktree] created {} on branch {branch}",
                target.display()
            );
            Some(target)
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            log::warn!(
                "[worktree] git worktree add failed (exit={:?}): {}",
                o.status.code(),
                stderr.trim()
            );
            None
        }
        Err(e) => {
            log::warn!("[worktree] could not run git: {e}");
            None
        }
    }
}

/// Best-effort worktree teardown. Removes the working dir and prunes
/// the worktree registration. Branch is intentionally kept around so
/// the user can recover their work via `git checkout cuartel/<id>` if
/// they close the session by accident.
pub fn remove_for(workspace: &Option<PathBuf>, session_id: &str) {
    let Some(workspace) = workspace else {
        return;
    };
    if !is_git_repo(workspace) {
        return;
    }
    let target = target_for(workspace, session_id);
    if !target.exists() {
        return;
    }

    let output = Command::new("git")
        .args([
            "worktree",
            "remove",
            "--force",
            target.to_str().unwrap_or(""),
        ])
        .current_dir(workspace)
        .output();

    match output {
        Ok(o) if o.status.success() => {
            log::info!("[worktree] removed {}", target.display());
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            log::warn!(
                "[worktree] git worktree remove failed (exit={:?}): {}",
                o.status.code(),
                stderr.trim()
            );
        }
        Err(e) => {
            log::warn!("[worktree] could not run git remove: {e}");
        }
    }
}
