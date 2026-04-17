use anyhow::Result;
use rusqlite::Connection;

pub fn run_migrations(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS schema_version (
            version INTEGER PRIMARY KEY
        );

        CREATE TABLE IF NOT EXISTS workspaces (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            path TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS credentials (
            id TEXT PRIMARY KEY,
            provider TEXT NOT NULL,
            env_key TEXT NOT NULL,
            encrypted_value BLOB NOT NULL,
            nonce BLOB NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(provider, env_key)
        );

        CREATE INDEX IF NOT EXISTS credentials_provider_idx
            ON credentials(provider);

        CREATE TABLE IF NOT EXISTS servers (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            address TEXT NOT NULL,
            tailscale_ip TEXT,
            is_local INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS sessions (
            id TEXT PRIMARY KEY,
            workspace_id TEXT NOT NULL REFERENCES workspaces(id),
            server_id TEXT NOT NULL REFERENCES servers(id),
            agent_type TEXT NOT NULL,
            rivet_session_id TEXT,
            status TEXT NOT NULL DEFAULT 'created',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS checkpoints (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL REFERENCES sessions(id),
            rivet_checkpoint_id TEXT,
            parent_checkpoint_id TEXT REFERENCES checkpoints(id),
            label TEXT,
            metadata TEXT NOT NULL DEFAULT '{}',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS checkpoints_session_idx
            ON checkpoints(session_id);
        CREATE INDEX IF NOT EXISTS checkpoints_created_idx
            ON checkpoints(session_id, created_at);

        CREATE TABLE IF NOT EXISTS pipeline_definitions (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            definition_json TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS pipeline_runs (
            id TEXT PRIMARY KEY,
            pipeline_id TEXT NOT NULL REFERENCES pipeline_definitions(id),
            state TEXT NOT NULL DEFAULT 'pending',
            stages_json TEXT NOT NULL DEFAULT '{}',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS pipeline_runs_pipeline_idx
            ON pipeline_runs(pipeline_id);
        CREATE INDEX IF NOT EXISTS pipeline_runs_state_idx
            ON pipeline_runs(state);

        CREATE TABLE IF NOT EXISTS scheduled_jobs (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            cron_expr TEXT NOT NULL,
            agent_type TEXT NOT NULL,
            prompt TEXT NOT NULL,
            workspace_id TEXT NOT NULL REFERENCES workspaces(id),
            enabled INTEGER NOT NULL DEFAULT 1,
            last_run_at TEXT,
            next_run_at TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS scheduled_jobs_next_run_idx
            ON scheduled_jobs(enabled, next_run_at);

        CREATE TABLE IF NOT EXISTS workflow_definitions (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            workspace_id TEXT NOT NULL REFERENCES workspaces(id),
            definition_json TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS workflow_executions (
            id TEXT PRIMARY KEY,
            definition_id TEXT NOT NULL REFERENCES workflow_definitions(id),
            rivet_workflow_id TEXT,
            state TEXT NOT NULL DEFAULT 'pending',
            current_step_index INTEGER NOT NULL DEFAULT 0,
            steps_json TEXT NOT NULL DEFAULT '[]',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS workflow_executions_def_idx
            ON workflow_executions(definition_id);
        CREATE INDEX IF NOT EXISTS workflow_executions_state_idx
            ON workflow_executions(state);

        CREATE TABLE IF NOT EXISTS audit_events (
            id TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            timestamp TEXT NOT NULL,
            hostname TEXT NOT NULL,
            provider_id TEXT,
            env_key TEXT,
            method TEXT,
            path TEXT,
            status INTEGER,
            client_ip TEXT,
            reason TEXT,
            error TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE INDEX IF NOT EXISTS audit_events_timestamp_idx
            ON audit_events(timestamp DESC);
        CREATE INDEX IF NOT EXISTS audit_events_kind_idx
            ON audit_events(kind);
        CREATE INDEX IF NOT EXISTS audit_events_hostname_idx
            ON audit_events(hostname);
        ",
    )?;
    Ok(())
}
