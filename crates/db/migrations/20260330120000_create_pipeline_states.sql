COMMIT;

PRAGMA foreign_keys = OFF;

BEGIN TRANSACTION;

CREATE TABLE pipeline_states (
    id                  BLOB PRIMARY KEY,
    workspace_id        BLOB NOT NULL UNIQUE,
    pipeline_config     TEXT NOT NULL,
    current_stage_id    TEXT NOT NULL,
    status              TEXT NOT NULL DEFAULT 'running',
    retry_counts        TEXT NOT NULL DEFAULT '{}',
    stage_history       TEXT NOT NULL DEFAULT '[]',
    handoff_artifacts   TEXT NOT NULL DEFAULT '{}',
    role_sessions       TEXT NOT NULL DEFAULT '{}',
    awaiting_approval   INTEGER NOT NULL DEFAULT 0,
    approval_stage_id   TEXT,
    approval_payload    TEXT,
    created_at          TEXT NOT NULL DEFAULT (datetime('now', 'subsec')),
    updated_at          TEXT NOT NULL DEFAULT (datetime('now', 'subsec'))
);

CREATE INDEX idx_pipeline_states_workspace_id ON pipeline_states(workspace_id);
CREATE INDEX idx_pipeline_states_status ON pipeline_states(status);

PRAGMA foreign_key_check;

COMMIT;

PRAGMA foreign_keys = ON;

BEGIN TRANSACTION;
