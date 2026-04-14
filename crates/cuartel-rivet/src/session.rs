use crate::client::{RivetClient, SessionEvent, SessionInfo};
use anyhow::Result;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSessionOptions {
    pub agent: String,
    pub env: HashMap<String, String>,
}

pub struct SessionManager {
    client: RivetClient,
}

impl SessionManager {
    pub fn new(client: RivetClient) -> Self {
        Self { client }
    }

    pub async fn create(
        &self,
        vm_id: &str,
        opts: &CreateSessionOptions,
    ) -> Result<SessionInfo> {
        self.client
            .create_session(vm_id, &opts.agent, &opts.env)
            .await
    }

    pub async fn send_prompt(
        &self,
        vm_id: &str,
        session_id: &str,
        prompt: &str,
    ) -> Result<serde_json::Value> {
        self.client.send_prompt(vm_id, session_id, prompt).await
    }

    pub fn subscribe_events(
        &self,
        vm_id: &str,
    ) -> Result<mpsc::Receiver<SessionEvent>> {
        let (tx, rx) = mpsc::channel(256);
        let base_url = self.client.base_url().to_string();
        let vm_id = vm_id.to_string();

        tokio::spawn(async move {
            let ws_url = format!(
                "{}/vm/{}/events",
                base_url.replace("http://", "ws://").replace("https://", "wss://"),
                vm_id
            );
            match tokio_tungstenite::connect_async(&ws_url).await {
                Ok((ws_stream, _)) => {
                    let (_write, mut read) = ws_stream.split();
                    while let Some(msg) = read.next().await {
                        match msg {
                            Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                                if let Ok(event) =
                                    serde_json::from_str::<SessionEvent>(&text)
                                {
                                    if tx.send(event).await.is_err() {
                                        break;
                                    }
                                }
                            }
                            Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => break,
                            Err(e) => {
                                log::error!("websocket error: {}", e);
                                break;
                            }
                            _ => {}
                        }
                    }
                }
                Err(e) => {
                    log::error!("failed to connect to event stream: {}", e);
                }
            }
        });

        Ok(rx)
    }
}
