//! Mount a host project directory at a known VM path via the rivetkit
//! filesystem actions.
//!
//! Phase 4e of `SPEC.md`: walks a host project tree, classifies each
//! entry, and materializes the result inside a running agent-os actor's
//! VM filesystem. `mountFs` is intentionally *not* exposed as an actor
//! action (see the TODO in `rivetkit/src/agent-os/actor/filesystem.ts`
//! — `VirtualFileSystem` drivers aren't serializable over the wire), so
//! the "mount" here is an explicit copy: for every regular file under
//! the workspace root we issue a `writeFiles` batch through the phase
//! 4d wrappers into `<mount_point>/<relative path>`.
//!
//! The planner ([`collect_mount_plan`]) is a pure function over a host
//! directory — it never touches the network, so it can be unit-tested
//! against a tempdir. The async executor ([`mount_workspace`]) is a
//! thin loop on top of it that owns the RPC side of the operation.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use cuartel_rivet::{BatchWriteEntry, DeleteOptions, FileBytes, MkdirOptions, RivetClient};

use crate::workspace::Workspace;

/// Default directory names excluded from the mount plan.
///
/// These are the usual suspects that blow up upload time with no
/// benefit: VCS metadata, dependency caches, build artifacts, and OS
/// cruft. Callers can replace the list via [`MountOptions::with_exclude`]
/// if they want to mount a superset.
pub const DEFAULT_EXCLUDE: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    ".DS_Store",
];

/// Cap on the size of any single file that gets uploaded, in bytes.
/// Anything larger is reported as a skip rather than pushed across the
/// wire — saves us from accidentally mounting a gigabyte-sized wasm
/// artifact.
pub const DEFAULT_MAX_FILE_SIZE: u64 = 2 * 1024 * 1024;

/// Number of [`BatchWriteEntry`] values packed into a single
/// `writeFiles` RPC. Small enough that a batch payload fits comfortably
/// in the default rivetkit request size limit even when every file is
/// close to [`DEFAULT_MAX_FILE_SIZE`].
pub const DEFAULT_BATCH_SIZE: usize = 32;

/// Configuration for [`mount_workspace`] / [`collect_mount_plan`].
#[derive(Debug, Clone)]
pub struct MountOptions {
    pub mount_point: String,
    pub exclude: Vec<String>,
    pub max_file_size: u64,
    pub batch_size: usize,
}

impl Default for MountOptions {
    fn default() -> Self {
        Self {
            mount_point: "/workspace".to_string(),
            exclude: DEFAULT_EXCLUDE.iter().map(|s| s.to_string()).collect(),
            max_file_size: DEFAULT_MAX_FILE_SIZE,
            batch_size: DEFAULT_BATCH_SIZE,
        }
    }
}

impl MountOptions {
    pub fn with_mount_point(mut self, point: impl Into<String>) -> Self {
        self.mount_point = point.into();
        self
    }

    pub fn with_exclude(mut self, excludes: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.exclude = excludes.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_max_file_size(mut self, size: u64) -> Self {
        self.max_file_size = size;
        self
    }

    pub fn with_batch_size(mut self, size: usize) -> Self {
        self.batch_size = size.max(1);
        self
    }

    fn normalized_mount_point(&self) -> Result<String> {
        let trimmed = self.mount_point.trim_end_matches('/');
        if !trimmed.starts_with('/') {
            return Err(anyhow!(
                "mount point must be an absolute path, got {:?}",
                self.mount_point
            ));
        }
        if trimmed == "" {
            return Err(anyhow!("mount point must not be root"));
        }
        Ok(trimmed.to_string())
    }
}

/// Why a given host path was omitted from the mount plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkipReason {
    Excluded,
    TooLarge { size: u64 },
    NonUtf8Path,
    NotRegularFile,
}

/// One entry in [`MountPlan::skipped`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkipEntry {
    pub host_path: PathBuf,
    pub reason: SkipReason,
}

/// One file slated for upload in a mount operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountFile {
    pub host_path: PathBuf,
    pub vm_path: String,
    pub size: u64,
}

/// Snapshot of the work that [`mount_workspace`] will perform.
#[derive(Debug, Clone, Default)]
pub struct MountPlan {
    /// VM directory paths that need to exist before any files are
    /// written, in lexicographic order. The executor still passes
    /// `recursive = true` to `mkdir` defensively, so ordering isn't
    /// load-bearing.
    pub directories: Vec<String>,
    pub files: Vec<MountFile>,
    pub skipped: Vec<SkipEntry>,
}

impl MountPlan {
    pub fn total_size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

/// Result of a completed mount, suitable for surfacing to a UI layer.
#[derive(Debug, Clone, Default)]
pub struct MountReport {
    pub mount_point: String,
    pub files_uploaded: usize,
    pub directories_created: usize,
    pub bytes_uploaded: u64,
    pub skipped: Vec<SkipEntry>,
}

/// Walk `host_root` and compute a [`MountPlan`] without touching the
/// network. Pure enough to unit-test against a tempdir.
pub fn collect_mount_plan(host_root: &Path, opts: &MountOptions) -> Result<MountPlan> {
    let mount_point = opts.normalized_mount_point()?;

    if !host_root.exists() {
        return Err(anyhow!(
            "host root does not exist: {}",
            host_root.display()
        ));
    }
    if !host_root.is_dir() {
        return Err(anyhow!(
            "host root is not a directory: {}",
            host_root.display()
        ));
    }

    let exclude: BTreeSet<&str> = opts.exclude.iter().map(String::as_str).collect();

    let mut plan = MountPlan::default();
    let mut dirs_seen: BTreeSet<String> = BTreeSet::new();
    walk(
        host_root,
        host_root,
        &mount_point,
        &exclude,
        opts.max_file_size,
        &mut plan,
        &mut dirs_seen,
    )?;

    plan.directories = dirs_seen.into_iter().collect();
    // Deterministic order for tests and idempotent re-runs.
    plan.files.sort_by(|a, b| a.vm_path.cmp(&b.vm_path));
    plan.skipped.sort_by(|a, b| a.host_path.cmp(&b.host_path));
    Ok(plan)
}

fn walk(
    host_root: &Path,
    current: &Path,
    mount_point: &str,
    exclude: &BTreeSet<&str>,
    max_file_size: u64,
    plan: &mut MountPlan,
    dirs_seen: &mut BTreeSet<String>,
) -> Result<()> {
    let read_dir =
        fs::read_dir(current).with_context(|| format!("read_dir {}", current.display()))?;
    for entry in read_dir {
        let entry = entry.context("read_dir entry")?;
        let path = entry.path();
        let name = match entry.file_name().to_str() {
            Some(s) => s.to_string(),
            None => {
                plan.skipped.push(SkipEntry {
                    host_path: path,
                    reason: SkipReason::NonUtf8Path,
                });
                continue;
            }
        };
        if exclude.contains(name.as_str()) {
            plan.skipped.push(SkipEntry {
                host_path: path,
                reason: SkipReason::Excluded,
            });
            continue;
        }

        let file_type = entry.file_type().context("file_type")?;
        if file_type.is_symlink() || (!file_type.is_file() && !file_type.is_dir()) {
            plan.skipped.push(SkipEntry {
                host_path: path,
                reason: SkipReason::NotRegularFile,
            });
            continue;
        }

        let rel = path.strip_prefix(host_root).expect("inside host root");
        let vm_path = match vm_path_for_rel(mount_point, rel) {
            Some(p) => p,
            None => {
                plan.skipped.push(SkipEntry {
                    host_path: path,
                    reason: SkipReason::NonUtf8Path,
                });
                continue;
            }
        };

        if file_type.is_dir() {
            dirs_seen.insert(vm_path);
            walk(
                host_root,
                &path,
                mount_point,
                exclude,
                max_file_size,
                plan,
                dirs_seen,
            )?;
            continue;
        }

        // Regular file.
        let metadata = entry.metadata().context("metadata")?;
        let size = metadata.len();
        if size > max_file_size {
            plan.skipped.push(SkipEntry {
                host_path: path,
                reason: SkipReason::TooLarge { size },
            });
            continue;
        }
        plan.files.push(MountFile {
            host_path: path,
            vm_path,
            size,
        });
    }
    Ok(())
}

/// Join `mount_point` with a relative path using forward slashes,
/// regardless of host separator.
///
/// Returns `None` if any component is non-UTF-8 or if `rel` contains
/// a `..` / root component (which shouldn't ever happen for paths
/// derived from `strip_prefix` on a real directory walk, but we
/// defend against it).
pub fn vm_path_for_rel(mount_point: &str, rel: &Path) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for component in rel.components() {
        match component {
            Component::Normal(os) => parts.push(os.to_str()?),
            Component::CurDir => {}
            _ => return None,
        }
    }
    if parts.is_empty() {
        Some(mount_point.to_string())
    } else {
        Some(format!("{}/{}", mount_point, parts.join("/")))
    }
}

/// Materialize `workspace` into the VM at `opts.mount_point` using the
/// phase 4d filesystem actions.
///
/// The flow is: (1) plan the walk on the host, (2) `mkdir -p` the mount
/// point plus every intermediate VM directory, (3) batch `writeFiles`
/// for the collected files. Any per-file failure surfaced in a
/// `BatchWriteResult` is promoted to a hard error — a partial mount is
/// worse than no mount for the downstream overlay/diff flow.
pub async fn mount_workspace(
    client: &RivetClient,
    actor_id: &str,
    workspace: &Workspace,
    opts: MountOptions,
) -> Result<MountReport> {
    let plan = collect_mount_plan(&workspace.path, &opts)?;
    let mount_point = opts.normalized_mount_point()?;

    client
        .mkdir(actor_id, &mount_point, MkdirOptions::recursive())
        .await
        .with_context(|| format!("mkdir {mount_point}"))?;
    for dir in &plan.directories {
        client
            .mkdir(actor_id, dir, MkdirOptions::recursive())
            .await
            .with_context(|| format!("mkdir {dir}"))?;
    }

    let mut current: Vec<BatchWriteEntry> = Vec::with_capacity(opts.batch_size);
    let mut bytes_uploaded = 0u64;
    let mut batches: Vec<Vec<BatchWriteEntry>> = Vec::new();
    for f in &plan.files {
        let bytes = fs::read(&f.host_path)
            .with_context(|| format!("read {}", f.host_path.display()))?;
        bytes_uploaded += bytes.len() as u64;
        current.push(BatchWriteEntry::new(f.vm_path.clone(), FileBytes::new(bytes)));
        if current.len() >= opts.batch_size {
            batches.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        batches.push(current);
    }

    for batch in batches {
        let results = client
            .write_files(actor_id, &batch)
            .await
            .context("writeFiles")?;
        for r in &results {
            if !r.success {
                return Err(anyhow!(
                    "writeFile failed for {}: {}",
                    r.path,
                    r.error.as_deref().unwrap_or("unknown error")
                ));
            }
        }
    }

    Ok(MountReport {
        mount_point,
        files_uploaded: plan.files.len(),
        directories_created: plan.directories.len() + 1,
        bytes_uploaded,
        skipped: plan.skipped,
    })
}

/// Remove `opts.mount_point` from the VM filesystem. Idempotent: if the
/// mount point does not exist we return `Ok(())` instead of surfacing
/// the underlying `ENOENT`.
pub async fn unmount_workspace(
    client: &RivetClient,
    actor_id: &str,
    opts: &MountOptions,
) -> Result<()> {
    let mount_point = opts.normalized_mount_point()?;
    if !client.exists(actor_id, &mount_point).await? {
        return Ok(());
    }
    client
        .delete_file(actor_id, &mount_point, DeleteOptions::recursive())
        .await
        .with_context(|| format!("delete {mount_point}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn touch(path: &Path, content: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    fn fixture_project() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("README.md"), b"# project");
        touch(&root.join("src/main.rs"), b"fn main() {}");
        touch(&root.join("src/lib.rs"), b"// lib");
        touch(&root.join("src/util/helper.rs"), b"// helper");
        touch(&root.join(".git/HEAD"), b"ref: refs/heads/main");
        touch(&root.join("node_modules/foo/index.js"), b"module.exports = 1;");
        touch(&root.join("target/debug/artifact"), b"binary");
        dir
    }

    #[test]
    fn mount_options_default_exclude_has_common_ignores() {
        let opts = MountOptions::default();
        assert!(opts.exclude.iter().any(|s| s == ".git"));
        assert!(opts.exclude.iter().any(|s| s == "node_modules"));
        assert!(opts.exclude.iter().any(|s| s == "target"));
        assert_eq!(opts.mount_point, "/workspace");
        assert_eq!(opts.max_file_size, DEFAULT_MAX_FILE_SIZE);
        assert_eq!(opts.batch_size, DEFAULT_BATCH_SIZE);
    }

    #[test]
    fn normalized_mount_point_requires_absolute() {
        let opts = MountOptions::default().with_mount_point("workspace");
        assert!(opts.normalized_mount_point().is_err());
    }

    #[test]
    fn normalized_mount_point_rejects_root() {
        let opts = MountOptions::default().with_mount_point("/");
        assert!(opts.normalized_mount_point().is_err());
    }

    #[test]
    fn normalized_mount_point_trims_trailing_slash() {
        let opts = MountOptions::default().with_mount_point("/workspace/");
        assert_eq!(opts.normalized_mount_point().unwrap(), "/workspace");
    }

    #[test]
    fn with_batch_size_clamps_to_at_least_one() {
        let opts = MountOptions::default().with_batch_size(0);
        assert_eq!(opts.batch_size, 1);
    }

    #[test]
    fn vm_path_joins_with_forward_slash() {
        let p = vm_path_for_rel("/workspace", Path::new("src/main.rs")).unwrap();
        assert_eq!(p, "/workspace/src/main.rs");
    }

    #[test]
    fn vm_path_empty_rel_returns_mount_point() {
        let p = vm_path_for_rel("/workspace", Path::new("")).unwrap();
        assert_eq!(p, "/workspace");
    }

    #[test]
    fn vm_path_rejects_parent_component() {
        assert!(vm_path_for_rel("/workspace", Path::new("../escape")).is_none());
    }

    #[test]
    fn collect_mount_plan_files_and_directories_sorted() {
        let dir = fixture_project();
        let opts = MountOptions::default();
        let plan = collect_mount_plan(dir.path(), &opts).unwrap();

        let file_paths: Vec<&str> = plan.files.iter().map(|f| f.vm_path.as_str()).collect();
        assert_eq!(
            file_paths,
            vec![
                "/workspace/README.md",
                "/workspace/src/lib.rs",
                "/workspace/src/main.rs",
                "/workspace/src/util/helper.rs",
            ]
        );

        assert_eq!(
            plan.directories,
            vec![
                "/workspace/src".to_string(),
                "/workspace/src/util".to_string(),
            ]
        );
    }

    #[test]
    fn collect_mount_plan_skips_default_excludes() {
        let dir = fixture_project();
        let opts = MountOptions::default();
        let plan = collect_mount_plan(dir.path(), &opts).unwrap();

        let excluded: Vec<&SkipEntry> = plan
            .skipped
            .iter()
            .filter(|s| matches!(s.reason, SkipReason::Excluded))
            .collect();
        let names: BTreeSet<&str> = excluded
            .iter()
            .filter_map(|s| s.host_path.file_name().and_then(|n| n.to_str()))
            .collect();
        assert!(names.contains(".git"));
        assert!(names.contains("node_modules"));
        assert!(names.contains("target"));
        // Nothing underneath an excluded dir should leak into files.
        for f in &plan.files {
            assert!(!f.vm_path.contains("node_modules"), "{:?}", f.vm_path);
            assert!(!f.vm_path.contains(".git"), "{:?}", f.vm_path);
            assert!(!f.vm_path.contains("/target/"), "{:?}", f.vm_path);
        }
    }

    #[test]
    fn collect_mount_plan_skips_files_larger_than_max_size() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("tiny.txt"), b"ok");
        touch(&dir.path().join("huge.bin"), &vec![0u8; 1024]);
        let opts = MountOptions::default().with_max_file_size(100);
        let plan = collect_mount_plan(dir.path(), &opts).unwrap();

        let file_names: Vec<&str> = plan
            .files
            .iter()
            .filter_map(|f| f.host_path.file_name().and_then(|n| n.to_str()))
            .collect();
        assert_eq!(file_names, vec!["tiny.txt"]);

        let skipped_huge = plan.skipped.iter().find(|s| {
            s.host_path
                .file_name()
                .and_then(|n| n.to_str())
                .map_or(false, |n| n == "huge.bin")
        });
        assert!(matches!(
            skipped_huge.map(|s| &s.reason),
            Some(SkipReason::TooLarge { size: 1024 })
        ));
    }

    #[test]
    fn collect_mount_plan_errors_on_missing_root() {
        let missing = std::env::temp_dir().join(format!(
            "cuartel-mount-missing-{}",
            uuid::Uuid::new_v4()
        ));
        let opts = MountOptions::default();
        assert!(collect_mount_plan(&missing, &opts).is_err());
    }

    #[test]
    fn collect_mount_plan_errors_on_file_root() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("single.txt");
        touch(&file, b"x");
        let opts = MountOptions::default();
        assert!(collect_mount_plan(&file, &opts).is_err());
    }

    #[test]
    fn collect_mount_plan_errors_on_invalid_mount_point() {
        let dir = tempfile::tempdir().unwrap();
        let opts = MountOptions::default().with_mount_point("relative");
        assert!(collect_mount_plan(dir.path(), &opts).is_err());
    }

    #[test]
    fn custom_exclude_replaces_defaults() {
        let dir = fixture_project();
        let opts = MountOptions::default().with_exclude(["README.md"]);
        let plan = collect_mount_plan(dir.path(), &opts).unwrap();
        // README.md is now excluded, but .git etc. are NOT, since the
        // exclude list was replaced, not extended.
        assert!(plan
            .files
            .iter()
            .any(|f| f.vm_path == "/workspace/.git/HEAD"));
        assert!(!plan.files.iter().any(|f| f.vm_path.ends_with("README.md")));
    }

    #[test]
    fn mount_plan_total_size_sums_files() {
        let dir = fixture_project();
        let plan = collect_mount_plan(dir.path(), &MountOptions::default()).unwrap();
        let expected: u64 = plan.files.iter().map(|f| f.size).sum();
        assert_eq!(plan.total_size(), expected);
        assert!(expected > 0);
    }

    #[test]
    fn mount_point_customization_changes_vm_paths() {
        let dir = fixture_project();
        let opts = MountOptions::default().with_mount_point("/projects/demo");
        let plan = collect_mount_plan(dir.path(), &opts).unwrap();
        assert!(plan
            .files
            .iter()
            .all(|f| f.vm_path.starts_with("/projects/demo/")));
        assert!(plan
            .directories
            .iter()
            .all(|d| d.starts_with("/projects/demo/")));
    }

    #[cfg(unix)]
    #[test]
    fn collect_mount_plan_skips_symlinks() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("real.txt"), b"real");
        symlink(dir.path().join("real.txt"), dir.path().join("link.txt")).unwrap();
        let plan = collect_mount_plan(dir.path(), &MountOptions::default()).unwrap();

        let file_names: Vec<&str> = plan
            .files
            .iter()
            .filter_map(|f| f.host_path.file_name().and_then(|n| n.to_str()))
            .collect();
        assert_eq!(file_names, vec!["real.txt"]);
        assert!(plan.skipped.iter().any(|s| {
            matches!(s.reason, SkipReason::NotRegularFile)
                && s.host_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map_or(false, |n| n == "link.txt")
        }));
    }
}
