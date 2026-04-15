//! Rivet AgentOS filesystem action client.
//!
//! Phase 4d of `SPEC.md`: thin typed wrappers around the filesystem actions
//! exposed by the rivetkit agent-os actor
//! (`rivetkit/src/agent-os/actor/filesystem.ts`). These sit in front of the
//! same `POST /gateway/{actor_id}/action/{name}` surface used by
//! `createSession`, `sendPrompt`, etc. (see [`crate::client`]); this module
//! just adds typed wrappers so phase 4e (mount project at `/workspace`) and
//! later overlay-snapshotting code can read/write VM files without pushing
//! `serde_json::Value` through the core crate.
//!
//! Binary file contents use the `["$Uint8Array", "<base64>"]` tagged array
//! encoding that rivetkit's `jsonStringifyCompat` / `jsonParseCompat`
//! serialize `Uint8Array` with (see
//! `rivetkit/src/actor/protocol/serde.ts`). The [`FileBytes`] newtype
//! round-trips that form transparently on both the request and response
//! sides, so callers can deal in plain `Vec<u8>`.

use anyhow::Result;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{
    de::{self, SeqAccess, Visitor},
    ser::SerializeTuple,
    Deserialize, Deserializer, Serialize, Serializer,
};
use serde_json::Value;
use std::fmt;

use crate::client::RivetClient;

/// Binary file contents, serialized as the rivetkit `["$Uint8Array", base64]`
/// tagged array.
///
/// rivetkit's `jsonStringifyCompat` encodes `Uint8Array` values this way so
/// binary payloads survive the JSON transport without being truncated at
/// non-UTF-8 bytes. `jsonParseCompat` decodes the same shape back into a
/// `Uint8Array` server-side, so the tagged form is symmetric across reads
/// and writes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileBytes(pub Vec<u8>);

impl FileBytes {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    pub fn into_inner(self) -> Vec<u8> {
        self.0
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

impl From<Vec<u8>> for FileBytes {
    fn from(v: Vec<u8>) -> Self {
        Self(v)
    }
}

impl From<&[u8]> for FileBytes {
    fn from(v: &[u8]) -> Self {
        Self(v.to_vec())
    }
}

impl From<&str> for FileBytes {
    fn from(v: &str) -> Self {
        Self(v.as_bytes().to_vec())
    }
}

impl From<String> for FileBytes {
    fn from(v: String) -> Self {
        Self(v.into_bytes())
    }
}

impl Serialize for FileBytes {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let encoded = STANDARD.encode(&self.0);
        let mut tup = serializer.serialize_tuple(2)?;
        tup.serialize_element("$Uint8Array")?;
        tup.serialize_element(&encoded)?;
        tup.end()
    }
}

impl<'de> Deserialize<'de> for FileBytes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct FileBytesVisitor;

        impl<'de> Visitor<'de> for FileBytesVisitor {
            type Value = FileBytes;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a [\"$Uint8Array\", base64] or [\"$ArrayBuffer\", base64] tagged array")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<FileBytes, A::Error> {
                let tag: String = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::invalid_length(0, &self))?;
                if tag != "$Uint8Array" && tag != "$ArrayBuffer" {
                    return Err(de::Error::custom(format!(
                        "expected $Uint8Array/$ArrayBuffer tag, got {tag:?}"
                    )));
                }
                let b64: String = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::invalid_length(1, &self))?;
                let bytes = STANDARD
                    .decode(b64.as_bytes())
                    .map_err(|e| de::Error::custom(format!("invalid base64 in {tag}: {e}")))?;
                Ok(FileBytes(bytes))
            }
        }

        deserializer.deserialize_seq(FileBytesVisitor)
    }
}

/// File type reported by [`DirEntry::entry_type`].
///
/// Matches the `"file" | "directory" | "symlink"` string union emitted by
/// the server-side `DirEntry` interface in `@rivet-dev/agent-os-core`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryType {
    File,
    Directory,
    Symlink,
}

/// One entry returned by [`RivetClient::read_dir_recursive`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirEntry {
    /// Absolute path inside the VM filesystem.
    pub path: String,
    #[serde(rename = "type")]
    pub entry_type: EntryType,
    pub size: i64,
}

/// Options accepted by [`RivetClient::read_dir_recursive`].
///
/// `default()` produces an "include everything" query; the client elides the
/// options arg from the wire payload entirely when no fields are set, so the
/// server sees a plain `[path]` arg list — matching the optional-arg
/// convention used elsewhere in the agent-os actor.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReaddirRecursiveOptions {
    #[serde(rename = "maxDepth", skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclude: Option<Vec<String>>,
}

impl ReaddirRecursiveOptions {
    fn is_empty(&self) -> bool {
        self.max_depth.is_none()
            && self
                .exclude
                .as_ref()
                .map_or(true, |v| v.is_empty())
    }
}

/// Entry for a batch write submitted via [`RivetClient::write_files`].
#[derive(Debug, Clone, Serialize)]
pub struct BatchWriteEntry {
    pub path: String,
    pub content: FileBytes,
}

impl BatchWriteEntry {
    pub fn new(path: impl Into<String>, content: impl Into<FileBytes>) -> Self {
        Self {
            path: path.into(),
            content: content.into(),
        }
    }
}

/// Per-file outcome returned by [`RivetClient::write_files`].
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct BatchWriteResult {
    pub path: String,
    pub success: bool,
    #[serde(default)]
    pub error: Option<String>,
}

/// Per-file outcome returned by [`RivetClient::read_files`].
///
/// `content` is `None` when the server reported a read error for that path;
/// the accompanying `error` field carries the reason string.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct BatchReadResult {
    pub path: String,
    #[serde(default)]
    pub content: Option<FileBytes>,
    #[serde(default)]
    pub error: Option<String>,
}

/// POSIX-flavored stat block returned by [`RivetClient::stat`].
///
/// Field names mirror the camelCase `VirtualStat` interface exported by
/// `@secure-exec/core` and re-exported through `@rivet-dev/agent-os-core`.
/// Time fields are kept as `f64` because Node emits JS numbers (which are
/// IEEE-754 doubles) for `mtimeMs` and friends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VirtualStat {
    pub mode: i64,
    pub size: i64,
    #[serde(rename = "isDirectory")]
    pub is_directory: bool,
    #[serde(rename = "isSymbolicLink")]
    pub is_symbolic_link: bool,
    #[serde(rename = "atimeMs")]
    pub atime_ms: f64,
    #[serde(rename = "mtimeMs")]
    pub mtime_ms: f64,
    #[serde(rename = "ctimeMs")]
    pub ctime_ms: f64,
    #[serde(rename = "birthtimeMs")]
    pub birthtime_ms: f64,
    pub ino: i64,
    pub nlink: i64,
    pub uid: i64,
    pub gid: i64,
}

/// Options accepted by [`RivetClient::mkdir`].
#[derive(Debug, Clone, Default, Serialize)]
pub struct MkdirOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recursive: Option<bool>,
}

impl MkdirOptions {
    pub fn recursive() -> Self {
        Self {
            recursive: Some(true),
        }
    }

    fn is_empty(&self) -> bool {
        self.recursive.is_none()
    }
}

/// Options accepted by [`RivetClient::delete_file`].
#[derive(Debug, Clone, Default, Serialize)]
pub struct DeleteOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recursive: Option<bool>,
}

impl DeleteOptions {
    pub fn recursive() -> Self {
        Self {
            recursive: Some(true),
        }
    }

    fn is_empty(&self) -> bool {
        self.recursive.is_none()
    }
}

impl RivetClient {
    /// Read a single file from the VM filesystem. Maps to the `readFile(path)`
    /// action in `buildFilesystemActions`.
    pub async fn read_file(&self, actor_id: &str, path: &str) -> Result<FileBytes> {
        let args = vec![Value::String(path.to_string())];
        self.call_action(actor_id, "readFile", args).await
    }

    /// Write a single file into the VM filesystem. Maps to the
    /// `writeFile(path, content)` action.
    pub async fn write_file(
        &self,
        actor_id: &str,
        path: &str,
        content: impl Into<FileBytes>,
    ) -> Result<()> {
        let bytes: FileBytes = content.into();
        let args = vec![
            Value::String(path.to_string()),
            serde_json::to_value(&bytes).expect("FileBytes is serializable"),
        ];
        let _: Option<Value> = self.call_action(actor_id, "writeFile", args).await?;
        Ok(())
    }

    /// Read several files in one round trip. Per-path errors surface as
    /// `BatchReadResult { content: None, error: Some(..) }` instead of
    /// aborting the whole call. Maps to `readFiles(paths)`.
    pub async fn read_files(
        &self,
        actor_id: &str,
        paths: &[&str],
    ) -> Result<Vec<BatchReadResult>> {
        let paths_json: Vec<Value> = paths
            .iter()
            .map(|p| Value::String((*p).to_string()))
            .collect();
        let args = vec![Value::Array(paths_json)];
        self.call_action(actor_id, "readFiles", args).await
    }

    /// Write several files in one round trip. Maps to `writeFiles(entries)`.
    pub async fn write_files(
        &self,
        actor_id: &str,
        entries: &[BatchWriteEntry],
    ) -> Result<Vec<BatchWriteResult>> {
        let entries_json: Vec<Value> = entries
            .iter()
            .map(|e| serde_json::to_value(e).expect("BatchWriteEntry is serializable"))
            .collect();
        let args = vec![Value::Array(entries_json)];
        self.call_action(actor_id, "writeFiles", args).await
    }

    /// List immediate children of a directory. Maps to `readdir(path)`.
    pub async fn read_dir(&self, actor_id: &str, path: &str) -> Result<Vec<String>> {
        let args = vec![Value::String(path.to_string())];
        self.call_action(actor_id, "readdir", args).await
    }

    /// Walk a directory recursively. The options envelope is omitted from
    /// the wire payload when empty. Maps to `readdirRecursive(path, options?)`.
    pub async fn read_dir_recursive(
        &self,
        actor_id: &str,
        path: &str,
        options: ReaddirRecursiveOptions,
    ) -> Result<Vec<DirEntry>> {
        let args = build_readdir_recursive_args(path, &options);
        self.call_action(actor_id, "readdirRecursive", args).await
    }

    /// Fetch stat metadata for a single path. Maps to `stat(path)`.
    pub async fn stat(&self, actor_id: &str, path: &str) -> Result<VirtualStat> {
        let args = vec![Value::String(path.to_string())];
        self.call_action(actor_id, "stat", args).await
    }

    /// Check whether a path exists. Maps to `exists(path)`.
    pub async fn exists(&self, actor_id: &str, path: &str) -> Result<bool> {
        let args = vec![Value::String(path.to_string())];
        self.call_action(actor_id, "exists", args).await
    }

    /// Create a directory. Pass [`MkdirOptions::recursive`] for `mkdir -p`
    /// semantics. Maps to `mkdir(path, options?)`.
    pub async fn mkdir(
        &self,
        actor_id: &str,
        path: &str,
        options: MkdirOptions,
    ) -> Result<()> {
        let args = build_mkdir_args(path, &options);
        let _: Option<Value> = self.call_action(actor_id, "mkdir", args).await?;
        Ok(())
    }

    /// Delete a file or directory. Pass [`DeleteOptions::recursive`] to
    /// remove a non-empty directory tree. Maps to `deleteFile(path, options?)`
    /// (the action is named `deleteFile` server-side even though it handles
    /// directories too — `delete` is a reserved word in some action-dispatch
    /// contexts).
    pub async fn delete_file(
        &self,
        actor_id: &str,
        path: &str,
        options: DeleteOptions,
    ) -> Result<()> {
        let args = build_delete_args(path, &options);
        let _: Option<Value> = self.call_action(actor_id, "deleteFile", args).await?;
        Ok(())
    }

    /// Rename or move a path within the VM filesystem. Maps to `move(from, to)`.
    pub async fn move_path(&self, actor_id: &str, from: &str, to: &str) -> Result<()> {
        let args = vec![
            Value::String(from.to_string()),
            Value::String(to.to_string()),
        ];
        let _: Option<Value> = self.call_action(actor_id, "move", args).await?;
        Ok(())
    }
}

// --- Pure helpers (easy to unit test) ------------------------------------

fn build_readdir_recursive_args(path: &str, options: &ReaddirRecursiveOptions) -> Vec<Value> {
    let mut args = vec![Value::String(path.to_string())];
    if !options.is_empty() {
        args.push(
            serde_json::to_value(options).expect("ReaddirRecursiveOptions is serializable"),
        );
    }
    args
}

fn build_mkdir_args(path: &str, options: &MkdirOptions) -> Vec<Value> {
    let mut args = vec![Value::String(path.to_string())];
    if !options.is_empty() {
        args.push(serde_json::to_value(options).expect("MkdirOptions is serializable"));
    }
    args
}

fn build_delete_args(path: &str, options: &DeleteOptions) -> Vec<Value> {
    let mut args = vec![Value::String(path.to_string())];
    if !options.is_empty() {
        args.push(serde_json::to_value(options).expect("DeleteOptions is serializable"));
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn file_bytes_serializes_as_uint8_array_tag() {
        let bytes = FileBytes::from(&b"hello"[..]);
        let value = serde_json::to_value(&bytes).unwrap();
        assert_eq!(value, json!(["$Uint8Array", "aGVsbG8="]));
    }

    #[test]
    fn file_bytes_round_trips_through_json() {
        let original = FileBytes::new(vec![0u8, 1, 2, 3, 0xff, 0xfe]);
        let encoded = serde_json::to_string(&original).unwrap();
        let decoded: FileBytes = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn file_bytes_deserializes_empty_payload() {
        let value = json!(["$Uint8Array", ""]);
        let bytes: FileBytes = serde_json::from_value(value).unwrap();
        assert!(bytes.0.is_empty());
    }

    #[test]
    fn file_bytes_accepts_array_buffer_tag() {
        let value = json!(["$ArrayBuffer", "aGVsbG8="]);
        let bytes: FileBytes = serde_json::from_value(value).unwrap();
        assert_eq!(bytes.0, b"hello");
    }

    #[test]
    fn file_bytes_rejects_unknown_tag() {
        let value = json!(["$BigInt", "123"]);
        let err = serde_json::from_value::<FileBytes>(value).unwrap_err();
        assert!(err.to_string().contains("$BigInt"));
    }

    #[test]
    fn file_bytes_rejects_invalid_base64() {
        let value = json!(["$Uint8Array", "not base64!!"]);
        assert!(serde_json::from_value::<FileBytes>(value).is_err());
    }

    #[test]
    fn batch_write_entry_serializes_with_nested_file_bytes() {
        let entry = BatchWriteEntry::new("/workspace/readme.md", "hi");
        let value = serde_json::to_value(&entry).unwrap();
        assert_eq!(
            value,
            json!({
                "path": "/workspace/readme.md",
                "content": ["$Uint8Array", "aGk="],
            })
        );
    }

    #[test]
    fn batch_read_result_deserializes_with_content() {
        let value = json!({
            "path": "/workspace/a.txt",
            "content": ["$Uint8Array", "YQ=="],
        });
        let result: BatchReadResult = serde_json::from_value(value).unwrap();
        assert_eq!(result.path, "/workspace/a.txt");
        assert_eq!(result.content.unwrap().0, b"a");
        assert!(result.error.is_none());
    }

    #[test]
    fn batch_read_result_deserializes_with_null_content() {
        let value = json!({
            "path": "/missing",
            "content": null,
            "error": "ENOENT",
        });
        let result: BatchReadResult = serde_json::from_value(value).unwrap();
        assert!(result.content.is_none());
        assert_eq!(result.error.as_deref(), Some("ENOENT"));
    }

    #[test]
    fn batch_write_result_deserializes_failure() {
        let value = json!({
            "path": "/ro/foo",
            "success": false,
            "error": "EROFS",
        });
        let result: BatchWriteResult = serde_json::from_value(value).unwrap();
        assert!(!result.success);
        assert_eq!(result.error.as_deref(), Some("EROFS"));
    }

    #[test]
    fn dir_entry_round_trips() {
        let entry = DirEntry {
            path: "/workspace/src".into(),
            entry_type: EntryType::Directory,
            size: 4096,
        };
        let value = serde_json::to_value(&entry).unwrap();
        assert_eq!(
            value,
            json!({
                "path": "/workspace/src",
                "type": "directory",
                "size": 4096,
            })
        );
        let back: DirEntry = serde_json::from_value(value).unwrap();
        assert_eq!(back, entry);
    }

    #[test]
    fn entry_type_deserializes_all_variants() {
        assert_eq!(
            serde_json::from_value::<EntryType>(json!("file")).unwrap(),
            EntryType::File
        );
        assert_eq!(
            serde_json::from_value::<EntryType>(json!("directory")).unwrap(),
            EntryType::Directory
        );
        assert_eq!(
            serde_json::from_value::<EntryType>(json!("symlink")).unwrap(),
            EntryType::Symlink
        );
    }

    #[test]
    fn virtual_stat_deserializes_from_camel_case_fixture() {
        let value = json!({
            "mode": 33188,
            "size": 128,
            "isDirectory": false,
            "isSymbolicLink": false,
            "atimeMs": 1_700_000_000_000.0_f64,
            "mtimeMs": 1_700_000_000_500.0_f64,
            "ctimeMs": 1_700_000_000_500.0_f64,
            "birthtimeMs": 1_700_000_000_000.0_f64,
            "ino": 42,
            "nlink": 1,
            "uid": 1000,
            "gid": 1000,
        });
        let stat: VirtualStat = serde_json::from_value(value).unwrap();
        assert_eq!(stat.mode, 33188);
        assert_eq!(stat.size, 128);
        assert!(!stat.is_directory);
        assert!(!stat.is_symbolic_link);
        assert!((stat.mtime_ms - 1_700_000_000_500.0).abs() < f64::EPSILON);
        assert_eq!(stat.ino, 42);
        assert_eq!(stat.uid, 1000);
    }

    #[test]
    fn readdir_recursive_args_omit_options_when_empty() {
        let args =
            build_readdir_recursive_args("/workspace", &ReaddirRecursiveOptions::default());
        assert_eq!(args, vec![Value::String("/workspace".into())]);
    }

    #[test]
    fn readdir_recursive_args_treat_empty_exclude_as_empty() {
        let opts = ReaddirRecursiveOptions {
            max_depth: None,
            exclude: Some(vec![]),
        };
        let args = build_readdir_recursive_args("/workspace", &opts);
        assert_eq!(args.len(), 1);
    }

    #[test]
    fn readdir_recursive_args_include_max_depth_and_exclude() {
        let opts = ReaddirRecursiveOptions {
            max_depth: Some(3),
            exclude: Some(vec!["node_modules".into(), "target".into()]),
        };
        let args = build_readdir_recursive_args("/workspace", &opts);
        assert_eq!(
            args,
            vec![
                Value::String("/workspace".into()),
                json!({
                    "maxDepth": 3,
                    "exclude": ["node_modules", "target"],
                }),
            ]
        );
    }

    #[test]
    fn mkdir_args_omit_options_when_empty() {
        let args = build_mkdir_args("/workspace/new", &MkdirOptions::default());
        assert_eq!(args, vec![Value::String("/workspace/new".into())]);
    }

    #[test]
    fn mkdir_args_include_recursive_flag() {
        let args = build_mkdir_args("/workspace/a/b/c", &MkdirOptions::recursive());
        assert_eq!(
            args,
            vec![
                Value::String("/workspace/a/b/c".into()),
                json!({ "recursive": true }),
            ]
        );
    }

    #[test]
    fn delete_args_omit_options_when_empty() {
        let args = build_delete_args("/workspace/tmp", &DeleteOptions::default());
        assert_eq!(args, vec![Value::String("/workspace/tmp".into())]);
    }

    #[test]
    fn delete_args_include_recursive_flag() {
        let args = build_delete_args("/workspace/tmp", &DeleteOptions::recursive());
        assert_eq!(
            args,
            vec![
                Value::String("/workspace/tmp".into()),
                json!({ "recursive": true }),
            ]
        );
    }

    #[test]
    fn empty_options_are_empty() {
        assert!(ReaddirRecursiveOptions::default().is_empty());
        assert!(MkdirOptions::default().is_empty());
        assert!(DeleteOptions::default().is_empty());
    }
}
