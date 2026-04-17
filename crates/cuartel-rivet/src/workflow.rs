use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::client::RivetClient;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowRecord {
    #[serde(rename = "workflowId")]
    pub workflow_id: String,
    #[serde(rename = "workflowName")]
    pub workflow_name: String,
    pub status: String,
    #[serde(default)]
    pub output: Option<Value>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(rename = "createdAt", default)]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowSignal {
    pub name: String,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct StartWorkflowRequest {
    pub workflow_name: String,
    pub input: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Value>,
}

impl RivetClient {
    pub async fn start_workflow(
        &self,
        actor_id: &str,
        req: &StartWorkflowRequest,
    ) -> Result<WorkflowRecord> {
        let args = vec![
            Value::String(req.workflow_name.clone()),
            req.input.clone(),
        ];
        let mut full_args = args;
        if let Some(ref tags) = req.tags {
            full_args.push(tags.clone());
        }
        self.call_action(actor_id, "startWorkflow", full_args).await
    }

    pub async fn get_workflow(
        &self,
        actor_id: &str,
        workflow_id: &str,
    ) -> Result<WorkflowRecord> {
        let args = vec![Value::String(workflow_id.to_string())];
        self.call_action(actor_id, "getWorkflow", args).await
    }

    pub async fn signal_workflow(
        &self,
        actor_id: &str,
        workflow_id: &str,
        signal: &WorkflowSignal,
    ) -> Result<()> {
        let args = vec![
            Value::String(workflow_id.to_string()),
            Value::String(signal.name.clone()),
            signal.payload.clone(),
        ];
        let _: Option<Value> = self.call_action(actor_id, "signalWorkflow", args).await?;
        Ok(())
    }

    pub async fn list_workflows(
        &self,
        actor_id: &str,
        workflow_name: Option<&str>,
    ) -> Result<Vec<WorkflowRecord>> {
        let args = match workflow_name {
            Some(name) => vec![json!({ "workflowName": name })],
            None => vec![],
        };
        self.call_action(actor_id, "listWorkflows", args).await
    }

    pub async fn cancel_workflow(
        &self,
        actor_id: &str,
        workflow_id: &str,
    ) -> Result<()> {
        let args = vec![Value::String(workflow_id.to_string())];
        let _: Option<Value> = self.call_action(actor_id, "cancelWorkflow", args).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn workflow_record_deserializes_from_camel_case() {
        let value = json!({
            "workflowId": "wf-1",
            "workflowName": "code-review",
            "status": "running",
            "output": null,
            "createdAt": "2026-04-16T12:00:00Z"
        });
        let rec: WorkflowRecord = serde_json::from_value(value).unwrap();
        assert_eq!(rec.workflow_id, "wf-1");
        assert_eq!(rec.workflow_name, "code-review");
        assert_eq!(rec.status, "running");
        assert!(rec.output.is_none());
        assert_eq!(rec.created_at.as_deref(), Some("2026-04-16T12:00:00Z"));
    }

    #[test]
    fn workflow_record_completed_with_output() {
        let value = json!({
            "workflowId": "wf-2",
            "workflowName": "test-suite",
            "status": "completed",
            "output": { "passed": 42, "failed": 0 },
            "error": null
        });
        let rec: WorkflowRecord = serde_json::from_value(value).unwrap();
        assert_eq!(rec.status, "completed");
        assert_eq!(rec.output.unwrap()["passed"], 42);
        assert!(rec.error.is_none());
    }

    #[test]
    fn workflow_record_failed_with_error() {
        let value = json!({
            "workflowId": "wf-3",
            "workflowName": "deploy",
            "status": "failed",
            "error": "timeout after 300s"
        });
        let rec: WorkflowRecord = serde_json::from_value(value).unwrap();
        assert_eq!(rec.status, "failed");
        assert_eq!(rec.error.as_deref(), Some("timeout after 300s"));
    }

    #[test]
    fn start_workflow_request_serializes_without_tags() {
        let req = StartWorkflowRequest {
            workflow_name: "deploy".into(),
            input: json!({ "branch": "main" }),
            tags: None,
        };
        let value = serde_json::to_value(&req).unwrap();
        assert!(value.get("tags").is_none());
        assert_eq!(value["workflow_name"], "deploy");
    }

    #[test]
    fn workflow_signal_serializes() {
        let sig = WorkflowSignal {
            name: "approve".into(),
            payload: json!({ "reviewer": "alice" }),
        };
        let value = serde_json::to_value(&sig).unwrap();
        assert_eq!(value["name"], "approve");
        assert_eq!(value["payload"]["reviewer"], "alice");
    }
}
