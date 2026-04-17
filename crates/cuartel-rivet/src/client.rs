//! HTTP client for the rivetkit manager API.
//!
//! Wraps the REST surface exposed by a running `rivetkit` server (the
//! Node-side "manager" router), plus actor-level actions invoked through
//! the manager's gateway at `POST /gateway/{actor_id}/action/{name}`.

use anyhow::{anyhow, Context, Result};
use reqwest::Client;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

#[derive(Clone)]
pub struct RivetClient {
    base_url: String,
    http: Client,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Health {
    pub status: String,
    pub runtime: String,
    pub version: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ActorName {
    #[serde(default)]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ActorNames {
    pub names: HashMap<String, ActorName>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Actor {
    pub actor_id: String,
    pub name: String,
    #[serde(default)]
    pub key: Option<String>,
    pub namespace_id: String,
    pub runner_name_selector: String,
    pub create_ts: i64,
    #[serde(default)]
    pub connectable_ts: Option<i64>,
    #[serde(default)]
    pub destroy_ts: Option<i64>,
    #[serde(default)]
    pub sleep_ts: Option<i64>,
    #[serde(default)]
    pub start_ts: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ActorList {
    pub actors: Vec<Actor>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GetOrCreateResult {
    pub actor: Actor,
    pub created: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct GetOrCreateRequest<'a> {
    pub name: &'a str,
    pub key: &'a str,
    pub runner_name_selector: &'a str,
    pub crash_policy: &'a str,
}

impl Default for GetOrCreateRequest<'static> {
    fn default() -> Self {
        Self {
            name: "vm",
            key: "default",
            runner_name_selector: "default",
            crash_policy: "kill",
        }
    }
}

impl RivetClient {
    /// Build a client against any rivet HTTP endpoint.
    ///
    /// `base_url` is stored as-is with a trailing slash trimmed; it may point
    /// at the sidecar on this Mac (`http://localhost:6420`) or at a remote
    /// rivet instance reached over Tailscale (`http://100.67.106.62:6420`).
    /// Callers that already hold a registered server should prefer
    /// `cuartel_remote::registry::rivet_client_for` which pulls the URL off
    /// the registry row and keeps the rivet/remote boundary clean.
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http: Client::new(),
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub async fn health(&self) -> Result<Health> {
        let resp = self
            .http
            .get(format!("{}/health", self.base_url))
            .send()
            .await
            .context("GET /health")?
            .error_for_status()?;
        resp.json::<Health>()
            .await
            .context("decode /health response")
    }

    pub async fn list_actor_names(&self, namespace: &str) -> Result<ActorNames> {
        let resp = self
            .http
            .get(format!("{}/actors/names", self.base_url))
            .query(&[("namespace", namespace)])
            .send()
            .await
            .context("GET /actors/names")?
            .error_for_status()?;
        resp.json::<ActorNames>()
            .await
            .context("decode /actors/names response")
    }

    pub async fn list_actors(&self, name: &str, key: Option<&str>) -> Result<Vec<Actor>> {
        let mut query: Vec<(&str, &str)> = vec![("name", name)];
        if let Some(k) = key {
            query.push(("key", k));
        }
        let resp = self
            .http
            .get(format!("{}/actors", self.base_url))
            .query(&query)
            .send()
            .await
            .context("GET /actors")?
            .error_for_status()?;
        Ok(resp
            .json::<ActorList>()
            .await
            .context("decode /actors response")?
            .actors)
    }

    pub async fn get_or_create_actor(
        &self,
        req: &GetOrCreateRequest<'_>,
    ) -> Result<GetOrCreateResult> {
        let resp = self
            .http
            .put(format!("{}/actors", self.base_url))
            .json(req)
            .send()
            .await
            .context("PUT /actors")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("PUT /actors returned {status}: {body}"));
        }
        resp.json::<GetOrCreateResult>()
            .await
            .context("decode PUT /actors response")
    }

    /// Invoke an actor action via the manager gateway and decode the JSON
    /// `output` field into `T`.
    ///
    /// Corresponds to `POST /gateway/{actor_id}/action/{action}` with a JSON
    /// body `{"args": [...]}`, as handled by rivetkit's
    /// `handleAction`/`HttpActionRequestSchema`.
    pub async fn call_action<T: DeserializeOwned>(
        &self,
        actor_id: &str,
        action: &str,
        args: Vec<Value>,
    ) -> Result<T> {
        let url = action_url(&self.base_url, actor_id, action);
        let body = action_body(args);
        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("POST {url} returned {status}: {body}"));
        }
        let envelope: ActionResponse<T> = resp
            .json()
            .await
            .with_context(|| format!("decode action {action} response"))?;
        Ok(envelope.output)
    }

    /// Create an agent-os session on the given VM actor.
    ///
    /// Maps to the `createSession(agentType, options?)` action from
    /// `@rivet-dev/rivetkit` agent-os `buildSessionActions`.
    pub async fn create_session(
        &self,
        actor_id: &str,
        agent_type: &str,
        options: Option<Value>,
    ) -> Result<SessionRecord> {
        let args = match options {
            Some(opts) => vec![Value::String(agent_type.into()), opts],
            None => vec![Value::String(agent_type.into())],
        };
        self.call_action(actor_id, "createSession", args).await
    }

    /// Send a prompt to an existing session and wait for the turn to end.
    ///
    /// Maps to the `sendPrompt(sessionId, text)` action. Blocks until the
    /// ACP adapter returns the final `JsonRpcResponse`; intermediate agent
    /// notifications are delivered via the event stream client (3c).
    pub async fn send_prompt(
        &self,
        actor_id: &str,
        session_id: &str,
        text: &str,
    ) -> Result<PromptResult> {
        let args = vec![Value::String(session_id.into()), Value::String(text.into())];
        self.call_action(actor_id, "sendPrompt", args).await
    }

    /// Destroy a session, freeing agent-os resources and removing persisted
    /// state. Maps to the `destroySession(sessionId)` action.
    pub async fn destroy_session(&self, actor_id: &str, session_id: &str) -> Result<()> {
        let args = vec![Value::String(session_id.into())];
        let _: Option<Value> = self.call_action(actor_id, "destroySession", args).await?;
        Ok(())
    }

    /// List active (non-persisted) sessions on the given VM actor.
    pub async fn list_sessions(&self, actor_id: &str) -> Result<Vec<SessionInfo>> {
        self.call_action(actor_id, "listSessions", vec![]).await
    }

    /// Cancel an in-flight prompt turn for a session.
    pub async fn cancel_prompt(&self, actor_id: &str, session_id: &str) -> Result<Value> {
        let args = vec![Value::String(session_id.into())];
        self.call_action(actor_id, "cancelPrompt", args).await
    }

    /// Open a WebSocket event subscription against the given actor. See
    /// [`crate::events`] for the full set of broadcast channels.
    pub async fn subscribe_events(
        &self,
        actor_id: &str,
        channels: &[&str],
    ) -> Result<crate::events::EventStream> {
        crate::events::subscribe(&self.base_url, actor_id, channels).await
    }

    pub async fn read_kv(&self, actor_id: &str, key: &str) -> Result<serde_json::Value> {
        let resp = self
            .http
            .get(format!(
                "{}/actors/{}/kv/keys/{}",
                self.base_url, actor_id, key
            ))
            .send()
            .await
            .context("GET /actors/{id}/kv/keys/{key}")?
            .error_for_status()?;
        let body: serde_json::Value = resp.json().await?;
        Ok(body.get("value").cloned().unwrap_or(serde_json::Value::Null))
    }
}

// --- Action envelope + session types -------------------------------------

#[derive(Debug, Deserialize)]
struct ActionResponse<T> {
    output: T,
}

/// Result of a `sendPrompt` action: the raw JSON-RPC response from the ACP
/// adapter plus the accumulated agent text for the turn.
#[derive(Debug, Clone, Deserialize)]
pub struct PromptResult {
    pub response: Value,
    #[serde(default)]
    pub text: String,
}

/// Snapshot of a session as returned by `createSession`/`getSession`.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionRecord {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "agentType")]
    pub agent_type: String,
    #[serde(default)]
    pub capabilities: Value,
    #[serde(rename = "agentInfo", default)]
    pub agent_info: Option<Value>,
}

/// Lightweight listing entry from `listSessions`.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionInfo {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "agentType")]
    pub agent_type: String,
}

// --- Pure helpers (easy to unit test) ------------------------------------

fn action_url(base: &str, actor_id: &str, action: &str) -> String {
    format!("{}/gateway/{}/action/{}", base.trim_end_matches('/'), actor_id, action)
}

fn action_body(args: Vec<Value>) -> Value {
    json!({ "args": args })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_url_uses_gateway_prefix() {
        assert_eq!(
            action_url("http://127.0.0.1:6420", "actor-123", "createSession"),
            "http://127.0.0.1:6420/gateway/actor-123/action/createSession"
        );
    }

    #[test]
    fn action_url_trims_trailing_slash() {
        assert_eq!(
            action_url("http://localhost:6420/", "abc", "sendPrompt"),
            "http://localhost:6420/gateway/abc/action/sendPrompt"
        );
    }

    #[test]
    fn action_url_works_with_remote_tailscale_ip() {
        // Phase 7d: the client must be equally happy against a remote
        // Tailscale-reachable rivet instance.
        assert_eq!(
            action_url("http://100.67.106.62:6420", "actor-xyz", "createSession"),
            "http://100.67.106.62:6420/gateway/actor-xyz/action/createSession"
        );
    }

    #[test]
    fn client_preserves_base_url() {
        let c = RivetClient::new("http://100.67.106.62:6420/");
        assert_eq!(c.base_url(), "http://100.67.106.62:6420");
    }

    #[test]
    fn action_body_wraps_args_array() {
        let body = action_body(vec![
            Value::String("pi".into()),
            json!({ "cwd": "/workspace" }),
        ]);
        assert_eq!(body, json!({ "args": ["pi", { "cwd": "/workspace" }] }));
    }

    #[test]
    fn action_body_empty_args_is_empty_array() {
        assert_eq!(action_body(vec![]), json!({ "args": [] }));
    }

    #[test]
    fn session_record_deserializes_from_camel_case_envelope() {
        let resp: ActionResponse<SessionRecord> = serde_json::from_value(json!({
            "output": {
                "sessionId": "sess_abc",
                "agentType": "pi",
                "capabilities": { "prompts": true },
                "agentInfo": { "name": "pi", "version": "0.1.0" }
            }
        }))
        .unwrap();
        assert_eq!(resp.output.session_id, "sess_abc");
        assert_eq!(resp.output.agent_type, "pi");
        assert_eq!(resp.output.capabilities, json!({ "prompts": true }));
        assert_eq!(
            resp.output.agent_info,
            Some(json!({ "name": "pi", "version": "0.1.0" }))
        );
    }

    #[test]
    fn session_record_accepts_null_agent_info() {
        let rec: SessionRecord = serde_json::from_value(json!({
            "sessionId": "s1",
            "agentType": "pi",
            "capabilities": {},
            "agentInfo": null
        }))
        .unwrap();
        assert!(rec.agent_info.is_none());
    }

    #[test]
    fn prompt_result_deserializes_with_default_text() {
        let pr: PromptResult = serde_json::from_value(json!({
            "response": { "jsonrpc": "2.0", "id": 1, "result": {} }
        }))
        .unwrap();
        assert_eq!(pr.text, "");
        assert_eq!(pr.response["jsonrpc"], "2.0");
    }

    #[test]
    fn session_info_list_round_trips() {
        let envelope: ActionResponse<Vec<SessionInfo>> = serde_json::from_value(json!({
            "output": [
                { "sessionId": "a", "agentType": "pi" },
                { "sessionId": "b", "agentType": "pi" }
            ]
        }))
        .unwrap();
        assert_eq!(envelope.output.len(), 2);
        assert_eq!(envelope.output[0].session_id, "a");
        assert_eq!(envelope.output[1].agent_type, "pi");
    }
}
