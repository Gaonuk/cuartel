use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use cuartel_db::workspaces::{WorkspaceRepo, WorkspaceRow};
use cuartel_db::Database;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    pub created_at: String,
    pub updated_at: String,
}

impl Workspace {
    fn from_row(row: WorkspaceRow) -> Self {
        Self {
            id: row.id,
            name: row.name,
            path: PathBuf::from(row.path),
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

/// Resolve a user-provided directory into an absolute, existing, canonical path.
///
/// Returns an error if the path does not exist or is not a directory. This is the
/// single validation point for mapping a workspace to a host project directory.
pub fn resolve_project_dir(path: impl AsRef<Path>) -> Result<PathBuf> {
    let path = path.as_ref();
    if !path.exists() {
        return Err(anyhow!("path does not exist: {}", path.display()));
    }
    if !path.is_dir() {
        return Err(anyhow!("path is not a directory: {}", path.display()));
    }
    Ok(path.canonicalize()?)
}

fn derive_name(path: &Path) -> String {
    path.file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| path.display().to_string())
}

pub struct WorkspaceService<'a> {
    repo: WorkspaceRepo<'a>,
}

impl<'a> WorkspaceService<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self {
            repo: WorkspaceRepo::new(db),
        }
    }

    /// Create a workspace pointing at `path`. If `name` is `None`, derive it from the
    /// directory basename. Fails if a workspace with the same canonical path already
    /// exists — use [`WorkspaceService::upsert_for_path`] for idempotent registration.
    pub fn create(&self, name: Option<&str>, path: impl AsRef<Path>) -> Result<Workspace> {
        let canonical = resolve_project_dir(path)?;
        let canonical_str = canonical.to_string_lossy().into_owned();
        if self.repo.find_by_path(&canonical_str)?.is_some() {
            return Err(anyhow!(
                "workspace already registered for path {}",
                canonical.display()
            ));
        }
        let name = name
            .map(|s| s.to_string())
            .unwrap_or_else(|| derive_name(&canonical));
        let id = Uuid::new_v4().to_string();
        let row = self.repo.insert(&id, &name, &canonical_str)?;
        Ok(Workspace::from_row(row))
    }

    pub fn upsert_for_path(&self, name: Option<&str>, path: impl AsRef<Path>) -> Result<Workspace> {
        let canonical = resolve_project_dir(path)?;
        let canonical_str = canonical.to_string_lossy().into_owned();
        if let Some(existing) = self.repo.find_by_path(&canonical_str)? {
            return Ok(Workspace::from_row(existing));
        }
        let name = name
            .map(|s| s.to_string())
            .unwrap_or_else(|| derive_name(&canonical));
        let id = Uuid::new_v4().to_string();
        let row = self.repo.insert(&id, &name, &canonical_str)?;
        Ok(Workspace::from_row(row))
    }

    pub fn get(&self, id: &str) -> Result<Option<Workspace>> {
        Ok(self.repo.get(id)?.map(Workspace::from_row))
    }

    pub fn find_by_path(&self, path: impl AsRef<Path>) -> Result<Option<Workspace>> {
        let canonical = resolve_project_dir(path)?;
        let canonical_str = canonical.to_string_lossy().into_owned();
        Ok(self
            .repo
            .find_by_path(&canonical_str)?
            .map(Workspace::from_row))
    }

    pub fn list(&self) -> Result<Vec<Workspace>> {
        Ok(self
            .repo
            .list()?
            .into_iter()
            .map(Workspace::from_row)
            .collect())
    }

    pub fn rename(&self, id: &str, new_name: &str) -> Result<Workspace> {
        let existing = self
            .repo
            .get(id)?
            .ok_or_else(|| anyhow!("workspace {id} not found"))?;
        let row = self.repo.update(id, new_name, &existing.path)?;
        Ok(Workspace::from_row(row))
    }

    pub fn remap(&self, id: &str, new_path: impl AsRef<Path>) -> Result<Workspace> {
        let existing = self
            .repo
            .get(id)?
            .ok_or_else(|| anyhow!("workspace {id} not found"))?;
        let canonical = resolve_project_dir(new_path)?;
        let canonical_str = canonical.to_string_lossy().into_owned();
        let row = self.repo.update(id, &existing.name, &canonical_str)?;
        Ok(Workspace::from_row(row))
    }

    pub fn delete(&self, id: &str) -> Result<bool> {
        self.repo.delete(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn db() -> Database {
        Database::open_in_memory().unwrap()
    }

    fn tmp_subdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cuartel-ws-test-{}-{}",
            name,
            Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn resolve_rejects_missing_path() {
        let p = std::env::temp_dir().join(format!("cuartel-missing-{}", Uuid::new_v4()));
        assert!(resolve_project_dir(&p).is_err());
    }

    #[test]
    fn resolve_rejects_file() {
        let dir = tmp_subdir("file");
        let file = dir.join("f.txt");
        fs::write(&file, b"").unwrap();
        assert!(resolve_project_dir(&file).is_err());
    }

    #[test]
    fn create_derives_name_from_basename() {
        let db = db();
        let svc = WorkspaceService::new(&db);
        let dir = tmp_subdir("basename");
        let ws = svc.create(None, &dir).unwrap();
        assert_eq!(ws.name, dir.file_name().unwrap().to_str().unwrap());
        assert_eq!(ws.path, dir.canonicalize().unwrap());
    }

    #[test]
    fn create_rejects_duplicate_path() {
        let db = db();
        let svc = WorkspaceService::new(&db);
        let dir = tmp_subdir("dup");
        svc.create(Some("a"), &dir).unwrap();
        assert!(svc.create(Some("b"), &dir).is_err());
    }

    #[test]
    fn upsert_returns_existing_for_same_path() {
        let db = db();
        let svc = WorkspaceService::new(&db);
        let dir = tmp_subdir("upsert");
        let first = svc.upsert_for_path(Some("a"), &dir).unwrap();
        let second = svc.upsert_for_path(Some("b"), &dir).unwrap();
        assert_eq!(first.id, second.id);
        assert_eq!(second.name, "a");
    }

    #[test]
    fn list_returns_created_workspaces() {
        let db = db();
        let svc = WorkspaceService::new(&db);
        let a = tmp_subdir("list-a");
        let b = tmp_subdir("list-b");
        svc.create(Some("a"), &a).unwrap();
        svc.create(Some("b"), &b).unwrap();
        let rows = svc.list().unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn rename_updates_name_only() {
        let db = db();
        let svc = WorkspaceService::new(&db);
        let dir = tmp_subdir("rename");
        let ws = svc.create(Some("old"), &dir).unwrap();
        let renamed = svc.rename(&ws.id, "new").unwrap();
        assert_eq!(renamed.name, "new");
        assert_eq!(renamed.path, ws.path);
    }

    #[test]
    fn remap_updates_path() {
        let db = db();
        let svc = WorkspaceService::new(&db);
        let a = tmp_subdir("remap-a");
        let b = tmp_subdir("remap-b");
        let ws = svc.create(Some("w"), &a).unwrap();
        let moved = svc.remap(&ws.id, &b).unwrap();
        assert_eq!(moved.path, b.canonicalize().unwrap());
    }

    #[test]
    fn delete_removes_workspace() {
        let db = db();
        let svc = WorkspaceService::new(&db);
        let dir = tmp_subdir("delete");
        let ws = svc.create(None, &dir).unwrap();
        assert!(svc.delete(&ws.id).unwrap());
        assert!(svc.get(&ws.id).unwrap().is_none());
    }
}
