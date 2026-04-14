//! HTTP client for the rivetkit manager API.
//!
//! Wraps the REST surface exposed by a running `rivetkit` server (the
//! Node-side "manager" router). Actor-level RPC (session/prompt etc.) goes
//! over WebSocket JSON-RPC and is not implemented here yet — that lands in
//! Phase 3.

use anyhow::{anyhow, Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
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
