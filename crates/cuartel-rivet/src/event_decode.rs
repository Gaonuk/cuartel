//! Pure decoding helpers for rivet event payloads.
//!
//! Lives in `cuartel-rivet` (not in the UI crate) so the helpers can be
//! unit-tested without pulling in gpui's proc-macros — testing inside a
//! `bin` crate that depends on `gpui_macros` currently triggers a nightly
//! rustc SIGBUS during macro expansion.

use base64::prelude::*;
use serde_json::Value;

/// A neutral summary of a permission request, produced from the raw ACP /
/// agent-os JSON. The UI crate maps this into its own richer type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionSummary {
    pub id: String,
    pub tool_name: String,
    pub input: Value,
}

/// Decode a rivetkit `jsonStringifyCompat` byte envelope into raw bytes.
///
/// rivetkit wraps `Uint8Array` values as `["$Uint8Array", "<base64>"]`;
/// we also handle a couple of fallback shapes that sometimes appear while
/// agent-os is still settling on the wire format.
pub fn decode_bytes_envelope(v: &Value) -> Option<Vec<u8>> {
    if let Value::Array(arr) = v {
        if arr.len() == 2 {
            if let (Some(Value::String(tag)), Some(Value::String(b64))) =
                (arr.first(), arr.get(1))
            {
                if tag == "$Uint8Array" {
                    if let Ok(bytes) = BASE64_STANDARD.decode(b64) {
                        return Some(bytes);
                    }
                }
            }
        }
    }
    if let Value::String(s) = v {
        return Some(s.as_bytes().to_vec());
    }
    if let Some(obj) = v.as_object() {
        if let Some(Value::String(b64)) = obj.get("data").or_else(|| obj.get("bytes")) {
            if let Ok(bytes) = BASE64_STANDARD.decode(b64) {
                return Some(bytes);
            }
        }
        if let Some(Value::String(text)) = obj.get("text") {
            return Some(text.as_bytes().to_vec());
        }
    }
    None
}

/// Best-effort extraction of agent-rendered text from an ACP `session/*`
/// notification. Falls back to a bracketed method tag when nothing
/// extractable is present so the caller can still surface activity.
///
/// Always returns a trailing newline so the caller can concatenate
/// results without worrying about line boundaries.
pub fn extract_session_update_text(method: &str, params: &Value) -> String {
    if let Some(text) = params.get("text").and_then(Value::as_str) {
        return format!("{text}\n");
    }
    let update = params.get("update").unwrap_or(params);
    if let Some(content) = update.get("content") {
        if let Some(text) = content.get("text").and_then(Value::as_str) {
            return format!("{text}\n");
        }
        if let Some(arr) = content.as_array() {
            let mut out = String::new();
            for block in arr {
                if let Some(t) = block.get("text").and_then(Value::as_str) {
                    out.push_str(t);
                }
            }
            if !out.is_empty() {
                out.push('\n');
                return out;
            }
        }
    }
    if let Some(title) = update.get("title").and_then(Value::as_str) {
        return format!("[tool] {title}\n");
    }
    format!("[{method}]\n")
}

/// Pull out a permission request id + tool name + input blob from the raw
/// agent-os `permissionRequest` payload. Never returns `None` for
/// well-formed payloads; a missing id is synthesized by the caller.
pub fn summarize_permission(request: &Value) -> PermissionSummary {
    let id = request
        .get("id")
        .or_else(|| request.get("requestId"))
        .or_else(|| request.get("permissionId"))
        .and_then(Value::as_str)
        .map(String::from)
        .unwrap_or_default();

    let (tool_name, input) = if let Some(tc) = request.get("toolCall") {
        let name = tc
            .get("title")
            .or_else(|| tc.get("kind"))
            .and_then(Value::as_str)
            .unwrap_or("tool")
            .to_string();
        (name, tc.clone())
    } else if let Some(tool) = request.get("tool") {
        let name = tool
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("tool")
            .to_string();
        let input = tool.get("input").cloned().unwrap_or_else(|| tool.clone());
        (name, input)
    } else {
        ("permission".to_string(), request.clone())
    };

    PermissionSummary {
        id,
        tool_name,
        input,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn decodes_uint8_array_envelope() {
        let v = json!(["$Uint8Array", "aGVsbG8="]); // "hello"
        assert_eq!(decode_bytes_envelope(&v).unwrap(), b"hello");
    }

    #[test]
    fn decodes_plain_string_as_bytes() {
        let v = json!("hi there");
        assert_eq!(decode_bytes_envelope(&v).unwrap(), b"hi there");
    }

    #[test]
    fn decodes_object_with_data_base64() {
        let v = json!({ "data": "aGVsbG8=" });
        assert_eq!(decode_bytes_envelope(&v).unwrap(), b"hello");
    }

    #[test]
    fn decodes_object_with_bytes_base64_alias() {
        let v = json!({ "bytes": "aGVsbG8=" });
        assert_eq!(decode_bytes_envelope(&v).unwrap(), b"hello");
    }

    #[test]
    fn decodes_object_with_text_field() {
        let v = json!({ "text": "hi" });
        assert_eq!(decode_bytes_envelope(&v).unwrap(), b"hi");
    }

    #[test]
    fn rejects_unknown_shape() {
        let v = json!({ "foo": "bar" });
        assert!(decode_bytes_envelope(&v).is_none());
    }

    #[test]
    fn rejects_wrong_tag_in_two_element_array() {
        let v = json!(["$Other", "aGVsbG8="]);
        assert!(decode_bytes_envelope(&v).is_none());
    }

    #[test]
    fn extracts_text_from_acp_agent_message_chunk() {
        let params = json!({
            "update": { "content": { "type": "text", "text": "hello world" } }
        });
        assert_eq!(
            extract_session_update_text("session/update", &params),
            "hello world\n"
        );
    }

    #[test]
    fn extracts_text_from_content_block_array() {
        let params = json!({
            "update": {
                "content": [
                    { "type": "text", "text": "part 1 " },
                    { "type": "text", "text": "part 2" }
                ]
            }
        });
        assert_eq!(
            extract_session_update_text("session/update", &params),
            "part 1 part 2\n"
        );
    }

    #[test]
    fn extracts_direct_text_field_at_params_root() {
        let params = json!({ "text": "direct" });
        assert_eq!(
            extract_session_update_text("session/message", &params),
            "direct\n"
        );
    }

    #[test]
    fn extracts_tool_title_when_no_text_content() {
        let params = json!({ "update": { "title": "Run tests" } });
        assert_eq!(
            extract_session_update_text("session/update", &params),
            "[tool] Run tests\n"
        );
    }

    #[test]
    fn falls_back_to_bracketed_method_when_no_text() {
        let params = json!({ "update": { "kind": "thinking" } });
        assert_eq!(
            extract_session_update_text("session/update", &params),
            "[session/update]\n"
        );
    }

    #[test]
    fn summarizes_tool_call_shape() {
        let request = json!({
            "id": "req-1",
            "toolCall": {
                "title": "Run shell command",
                "kind": "execute",
                "toolCallId": "tc-1"
            }
        });
        let s = summarize_permission(&request);
        assert_eq!(s.id, "req-1");
        assert_eq!(s.tool_name, "Run shell command");
    }

    #[test]
    fn summarizes_tool_input_shape() {
        let request = json!({
            "requestId": "r2",
            "tool": { "name": "bash", "input": { "command": "ls" } }
        });
        let s = summarize_permission(&request);
        assert_eq!(s.id, "r2");
        assert_eq!(s.tool_name, "bash");
        assert_eq!(s.input, json!({ "command": "ls" }));
    }

    #[test]
    fn summarizes_missing_id_as_empty_so_caller_synthesizes() {
        let request = json!({ "toolCall": { "title": "x" } });
        let s = summarize_permission(&request);
        assert_eq!(s.id, "");
        assert_eq!(s.tool_name, "x");
    }

    #[test]
    fn summarizes_completely_unknown_payload_as_permission_fallback() {
        let request = json!({ "foo": 1 });
        let s = summarize_permission(&request);
        assert_eq!(s.tool_name, "permission");
        assert_eq!(s.input, json!({ "foo": 1 }));
    }
}
