//! Rivet actor event stream.
//!
//! Opens a WebSocket to `/gateway/{actor_id}/connect` (rivetkit's stateful
//! actor protocol, JSON encoding), sends `SubscriptionRequest` messages for
//! the agent-os broadcast channels we care about, and forwards typed
//! [`RivetEvent`] values over an unbounded channel.
//!
//! The JSON wire format is defined in rivetkit's `client-protocol-zod`
//! module: server → client messages are `{ body: { tag, val } }` envelopes
//! where `tag` is one of `Init` / `Event` / `Error` / `ActionResponse`.
//! We only care about `Init` (connection acknowledgement) and `Event`
//! (server broadcast via `c.broadcast(name, payload)`).

use std::fmt;

use anyhow::{anyhow, Context, Result};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::{connect_async, tungstenite::Message};

/// Canonical agent-os broadcast channel names (see rivetkit
/// `AgentOsEvents`). Used when constructing subscription requests.
pub const EVENT_SESSION_EVENT: &str = "sessionEvent";
pub const EVENT_PERMISSION_REQUEST: &str = "permissionRequest";
pub const EVENT_VM_BOOTED: &str = "vmBooted";
pub const EVENT_VM_SHUTDOWN: &str = "vmShutdown";
pub const EVENT_PROCESS_OUTPUT: &str = "processOutput";
pub const EVENT_PROCESS_EXIT: &str = "processExit";
pub const EVENT_SHELL_DATA: &str = "shellData";
pub const EVENT_CRON_EVENT: &str = "cronEvent";

/// The default set of channels subscribed when no explicit list is passed
/// to [`RivetClient::subscribe_events`](crate::RivetClient::subscribe_events).
pub const DEFAULT_CHANNELS: &[&str] = &[
    EVENT_SESSION_EVENT,
    EVENT_PERMISSION_REQUEST,
    EVENT_VM_BOOTED,
    EVENT_VM_SHUTDOWN,
    EVENT_PROCESS_OUTPUT,
    EVENT_PROCESS_EXIT,
];

/// A JSON-RPC notification emitted by an ACP agent adapter (pi, claude-code,
/// ...). Payload of `sessionEvent` broadcasts.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JsonRpcNotification {
    #[serde(default = "default_jsonrpc")]
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

fn default_jsonrpc() -> String {
    "2.0".to_string()
}

/// Payload of a `sessionEvent` broadcast.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionEventPayload {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub event: JsonRpcNotification,
}

/// Payload of a `permissionRequest` broadcast.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRequestPayload {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub request: Value,
}

/// Payload of a `vmShutdown` broadcast. `reason` is one of `"sleep"`,
/// `"destroy"`, or `"error"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmShutdownPayload {
    pub reason: String,
}

/// Payload of a `processExit` broadcast.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessExitPayload {
    pub pid: u64,
    #[serde(rename = "exitCode")]
    pub exit_code: i32,
}

/// Typed view of a server-side broadcast delivered over the event stream.
///
/// Payloads that contain binary fields (`processOutput`, `shellData`,
/// `cronEvent`) are kept as raw [`serde_json::Value`] so callers can decide
/// how to decode the `["$Uint8Array", "base64..."]` envelopes that
/// `jsonStringifyCompat` produces.
#[derive(Debug, Clone)]
pub enum RivetEvent {
    SessionEvent(SessionEventPayload),
    PermissionRequest(PermissionRequestPayload),
    VmBooted,
    VmShutdown(VmShutdownPayload),
    ProcessOutput(Value),
    ProcessExit(ProcessExitPayload),
    ShellData(Value),
    CronEvent(Value),
    /// A broadcast channel we don't have a typed variant for yet.
    Other { name: String, args: Value },
    /// A server-side error frame. The stream stays open but the caller may
    /// want to surface these to the UI.
    Error {
        group: String,
        code: String,
        message: String,
    },
}

impl RivetEvent {
    /// Dispatch a raw `{ name, args }` `Event` payload into a typed variant.
    pub fn from_broadcast(name: &str, args: Value) -> Self {
        match name {
            EVENT_SESSION_EVENT => match serde_json::from_value::<SessionEventPayload>(args.clone())
            {
                Ok(p) => RivetEvent::SessionEvent(p),
                Err(_) => RivetEvent::Other {
                    name: name.to_string(),
                    args,
                },
            },
            EVENT_PERMISSION_REQUEST => {
                match serde_json::from_value::<PermissionRequestPayload>(args.clone()) {
                    Ok(p) => RivetEvent::PermissionRequest(p),
                    Err(_) => RivetEvent::Other {
                        name: name.to_string(),
                        args,
                    },
                }
            }
            EVENT_VM_BOOTED => RivetEvent::VmBooted,
            EVENT_VM_SHUTDOWN => {
                match serde_json::from_value::<VmShutdownPayload>(args.clone()) {
                    Ok(p) => RivetEvent::VmShutdown(p),
                    Err(_) => RivetEvent::Other {
                        name: name.to_string(),
                        args,
                    },
                }
            }
            EVENT_PROCESS_OUTPUT => RivetEvent::ProcessOutput(args),
            EVENT_PROCESS_EXIT => match serde_json::from_value::<ProcessExitPayload>(args.clone())
            {
                Ok(p) => RivetEvent::ProcessExit(p),
                Err(_) => RivetEvent::Other {
                    name: name.to_string(),
                    args,
                },
            },
            EVENT_SHELL_DATA => RivetEvent::ShellData(args),
            EVENT_CRON_EVENT => RivetEvent::CronEvent(args),
            other => RivetEvent::Other {
                name: other.to_string(),
                args,
            },
        }
    }
}

impl fmt::Display for RivetEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RivetEvent::SessionEvent(p) => {
                write!(f, "sessionEvent({}, {})", p.session_id, p.event.method)
            }
            RivetEvent::PermissionRequest(p) => {
                write!(f, "permissionRequest({})", p.session_id)
            }
            RivetEvent::VmBooted => write!(f, "vmBooted"),
            RivetEvent::VmShutdown(p) => write!(f, "vmShutdown({})", p.reason),
            RivetEvent::ProcessOutput(_) => write!(f, "processOutput"),
            RivetEvent::ProcessExit(p) => {
                write!(f, "processExit(pid={}, code={})", p.pid, p.exit_code)
            }
            RivetEvent::ShellData(_) => write!(f, "shellData"),
            RivetEvent::CronEvent(_) => write!(f, "cronEvent"),
            RivetEvent::Other { name, .. } => write!(f, "other({name})"),
            RivetEvent::Error {
                group,
                code,
                message,
            } => write!(f, "error({group}.{code}: {message})"),
        }
    }
}

/// Outcome of parsing a single server-to-client frame. `Init` is returned
/// separately because callers may want to synchronise on the first Init
/// before forwarding events.
#[derive(Debug, Clone)]
pub(crate) enum ParsedFrame {
    Init {
        #[allow(dead_code)]
        actor_id: String,
        #[allow(dead_code)]
        connection_id: String,
    },
    Event(RivetEvent),
    /// An `ActionResponse` or other frame we ignore in a subscribe-only
    /// client. Preserved as a variant so tests can assert on it.
    Ignored,
}

/// Parse a JSON `ToClient` envelope into a [`ParsedFrame`].
pub(crate) fn parse_frame(text: &str) -> Result<ParsedFrame> {
    let value: Value = serde_json::from_str(text).context("parse ToClient JSON")?;
    let body = value
        .get("body")
        .ok_or_else(|| anyhow!("ToClient frame missing body: {text}"))?;
    let tag = body
        .get("tag")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("ToClient body missing tag: {text}"))?;
    let val = body
        .get("val")
        .cloned()
        .ok_or_else(|| anyhow!("ToClient body missing val: {text}"))?;

    match tag {
        "Init" => {
            let actor_id = val
                .get("actorId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let connection_id = val
                .get("connectionId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            Ok(ParsedFrame::Init {
                actor_id,
                connection_id,
            })
        }
        "Event" => {
            let name = val
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("Event missing name"))?
                .to_string();
            let args = val.get("args").cloned().unwrap_or(Value::Null);
            Ok(ParsedFrame::Event(RivetEvent::from_broadcast(&name, args)))
        }
        "Error" => {
            let group = val
                .get("group")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let code = val
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let message = val
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            Ok(ParsedFrame::Event(RivetEvent::Error {
                group,
                code,
                message,
            }))
        }
        "ActionResponse" => Ok(ParsedFrame::Ignored),
        other => Err(anyhow!("unknown ToClient tag: {other}")),
    }
}

/// Build a `SubscriptionRequest` JSON string for the given event channel.
pub(crate) fn subscription_message(event_name: &str, subscribe: bool) -> String {
    let msg = json!({
        "body": {
            "tag": "SubscriptionRequest",
            "val": {
                "eventName": event_name,
                "subscribe": subscribe,
            }
        }
    });
    msg.to_string()
}

/// Convert an http(s) base URL to the corresponding ws(s) URL for the
/// actor connect endpoint.
pub(crate) fn connect_ws_url(base_url: &str, actor_id: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let ws_base = if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else {
        base.to_string()
    };
    format!("{ws_base}/gateway/{actor_id}/connect")
}

/// Handle for an active event subscription. Dropping the handle aborts the
/// background reader task; the receiver can be polled directly as long as
/// the handle is kept alive.
pub struct EventStream {
    rx: mpsc::UnboundedReceiver<RivetEvent>,
    task: JoinHandle<()>,
}

impl EventStream {
    /// Receive the next event. Returns `None` if the WebSocket closed.
    pub async fn recv(&mut self) -> Option<RivetEvent> {
        self.rx.recv().await
    }

    /// Borrow the underlying receiver for use with `tokio::select!` or
    /// similar constructs.
    pub fn receiver(&mut self) -> &mut mpsc::UnboundedReceiver<RivetEvent> {
        &mut self.rx
    }
}

impl Drop for EventStream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Open a WebSocket subscription to the given actor and channels.
///
/// Returns once the initial handshake (Init frame + all subscription
/// requests flushed) has completed. Subsequent events arrive via the
/// returned [`EventStream`].
pub async fn subscribe(
    base_url: &str,
    actor_id: &str,
    channels: &[&str],
) -> Result<EventStream> {
    let url = connect_ws_url(base_url, actor_id);
    let (ws, _resp) = connect_async(&url)
        .await
        .with_context(|| format!("connect {url}"))?;
    let (mut sink, mut stream) = ws.split();

    // Wait for the Init frame so we know the server is ready to accept
    // subscription requests and, implicitly, that the actor exists.
    let init_frame = loop {
        let msg = stream
            .next()
            .await
            .ok_or_else(|| anyhow!("ws closed before Init"))?
            .context("ws error waiting for Init")?;
        match msg {
            Message::Text(text) => match parse_frame(&text)? {
                ParsedFrame::Init { .. } => break (),
                ParsedFrame::Ignored => continue,
                ParsedFrame::Event(_) => continue,
            },
            Message::Binary(_) => {
                return Err(anyhow!("unexpected binary frame before Init"));
            }
            Message::Ping(_) | Message::Pong(_) => continue,
            Message::Close(_) => return Err(anyhow!("ws closed before Init")),
            Message::Frame(_) => continue,
        }
    };
    let _ = init_frame;

    // Fire subscription requests for each requested channel.
    for channel in channels {
        let msg = subscription_message(channel, true);
        sink.send(Message::Text(msg.into()))
            .await
            .with_context(|| format!("send SubscriptionRequest for {channel}"))?;
    }

    let (tx, rx) = mpsc::unbounded_channel::<RivetEvent>();
    let task = tokio::spawn(async move {
        while let Some(msg) = stream.next().await {
            match msg {
                Ok(Message::Text(text)) => match parse_frame(&text) {
                    Ok(ParsedFrame::Event(ev)) => {
                        if tx.send(ev).is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        log::warn!("rivet event parse error: {e}");
                    }
                },
                Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => {}
                Ok(Message::Binary(_)) => {
                    log::warn!("unexpected binary frame on rivet event stream");
                }
                Ok(Message::Close(_)) => break,
                Err(e) => {
                    log::warn!("rivet event stream error: {e}");
                    break;
                }
            }
        }
    });

    Ok(EventStream { rx, task })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_ws_url_maps_http_to_ws() {
        assert_eq!(
            connect_ws_url("http://127.0.0.1:6420", "actor-1"),
            "ws://127.0.0.1:6420/gateway/actor-1/connect"
        );
    }

    #[test]
    fn connect_ws_url_maps_https_to_wss() {
        assert_eq!(
            connect_ws_url("https://rivet.example.com/", "abc"),
            "wss://rivet.example.com/gateway/abc/connect"
        );
    }

    #[test]
    fn subscription_message_shape_matches_zod_schema() {
        let msg = subscription_message(EVENT_SESSION_EVENT, true);
        let v: Value = serde_json::from_str(&msg).unwrap();
        assert_eq!(v["body"]["tag"], "SubscriptionRequest");
        assert_eq!(v["body"]["val"]["eventName"], "sessionEvent");
        assert_eq!(v["body"]["val"]["subscribe"], true);
    }

    #[test]
    fn parse_frame_init_returns_actor_and_connection_ids() {
        let raw = r#"{"body":{"tag":"Init","val":{"actorId":"actor-1","connectionId":"conn-9"}}}"#;
        match parse_frame(raw).unwrap() {
            ParsedFrame::Init {
                actor_id,
                connection_id,
            } => {
                assert_eq!(actor_id, "actor-1");
                assert_eq!(connection_id, "conn-9");
            }
            other => panic!("expected Init, got {other:?}"),
        }
    }

    #[test]
    fn parse_frame_session_event_dispatches_to_typed_variant() {
        let raw = r#"{
            "body": {
                "tag": "Event",
                "val": {
                    "name": "sessionEvent",
                    "args": {
                        "sessionId": "sess_1",
                        "event": {
                            "jsonrpc": "2.0",
                            "method": "session/update",
                            "params": { "delta": "hello" }
                        }
                    }
                }
            }
        }"#;
        let ParsedFrame::Event(RivetEvent::SessionEvent(p)) = parse_frame(raw).unwrap() else {
            panic!("expected session event");
        };
        assert_eq!(p.session_id, "sess_1");
        assert_eq!(p.event.method, "session/update");
        assert_eq!(p.event.params, json!({ "delta": "hello" }));
    }

    #[test]
    fn parse_frame_vm_booted_is_unit_variant() {
        let raw = r#"{"body":{"tag":"Event","val":{"name":"vmBooted","args":{}}}}"#;
        let ParsedFrame::Event(RivetEvent::VmBooted) = parse_frame(raw).unwrap() else {
            panic!("expected VmBooted");
        };
    }

    #[test]
    fn parse_frame_vm_shutdown_carries_reason() {
        let raw = r#"{"body":{"tag":"Event","val":{"name":"vmShutdown","args":{"reason":"sleep"}}}}"#;
        let ParsedFrame::Event(RivetEvent::VmShutdown(p)) = parse_frame(raw).unwrap() else {
            panic!("expected VmShutdown");
        };
        assert_eq!(p.reason, "sleep");
    }

    #[test]
    fn parse_frame_process_exit_parses_numeric_fields() {
        let raw = r#"{"body":{"tag":"Event","val":{"name":"processExit","args":{"pid":1234,"exitCode":0}}}}"#;
        let ParsedFrame::Event(RivetEvent::ProcessExit(p)) = parse_frame(raw).unwrap() else {
            panic!("expected ProcessExit");
        };
        assert_eq!(p.pid, 1234);
        assert_eq!(p.exit_code, 0);
    }

    #[test]
    fn parse_frame_unknown_event_name_falls_back_to_other() {
        let raw = r#"{"body":{"tag":"Event","val":{"name":"mystery","args":{"x":1}}}}"#;
        let ParsedFrame::Event(RivetEvent::Other { name, args }) = parse_frame(raw).unwrap() else {
            panic!("expected Other");
        };
        assert_eq!(name, "mystery");
        assert_eq!(args, json!({ "x": 1 }));
    }

    #[test]
    fn parse_frame_error_surfaces_as_error_variant() {
        let raw = r#"{"body":{"tag":"Error","val":{"group":"actor","code":"not_found","message":"nope","actionId":null}}}"#;
        let ParsedFrame::Event(RivetEvent::Error {
            group,
            code,
            message,
        }) = parse_frame(raw).unwrap()
        else {
            panic!("expected Error");
        };
        assert_eq!(group, "actor");
        assert_eq!(code, "not_found");
        assert_eq!(message, "nope");
    }

    #[test]
    fn parse_frame_action_response_is_ignored() {
        let raw = r#"{"body":{"tag":"ActionResponse","val":{"id":["$BigInt","1"],"output":null}}}"#;
        matches!(parse_frame(raw).unwrap(), ParsedFrame::Ignored);
    }

    #[test]
    fn default_channels_cover_agent_os_events() {
        assert!(DEFAULT_CHANNELS.contains(&EVENT_SESSION_EVENT));
        assert!(DEFAULT_CHANNELS.contains(&EVENT_PERMISSION_REQUEST));
        assert!(DEFAULT_CHANNELS.contains(&EVENT_VM_BOOTED));
    }
}
