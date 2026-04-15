pub mod checkpoints;
pub mod client;
pub mod event_decode;
pub mod events;
pub mod sidecar;

pub use checkpoints::{CheckpointRecord, CreateCheckpointOptions, RestoreCheckpointOptions};
pub use client::{
    Actor, GetOrCreateRequest, GetOrCreateResult, Health, PromptResult, RivetClient,
    SessionInfo, SessionRecord,
};
pub use event_decode::{
    decode_bytes_envelope, extract_session_update_text, summarize_permission,
    PermissionSummary,
};
pub use events::{
    subscribe as subscribe_events, EventStream, JsonRpcNotification, PermissionRequestPayload,
    ProcessExitPayload, RivetEvent, SessionEventPayload, VmShutdownPayload, DEFAULT_CHANNELS,
    EVENT_CRON_EVENT, EVENT_PERMISSION_REQUEST, EVENT_PROCESS_EXIT, EVENT_PROCESS_OUTPUT,
    EVENT_SESSION_EVENT, EVENT_SHELL_DATA, EVENT_VM_BOOTED, EVENT_VM_SHUTDOWN,
};
pub use sidecar::Sidecar;
