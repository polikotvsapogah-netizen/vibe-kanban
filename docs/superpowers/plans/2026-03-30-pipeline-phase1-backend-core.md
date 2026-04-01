# Multi-Agent Pipeline — Phase 1: Backend Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the pipeline state machine core — DB model, PipelineController, verdict parsing, and exit_monitor integration — so that pipeline transitions can be tested end-to-end without UI or API endpoints.

**Architecture:** New `pipeline_states` SQLite table stores pipeline runtime state per workspace. A thin `PipelineController` module reads verdicts from `CodingAgentTurn.summary`, updates state, and determines the next stage transition. The controller is called from `spawn_exit_monitor()` after execution completion.

**Tech Stack:** Rust, SQLx (SQLite), serde_json, tokio, uuid, chrono

**Spec:** `docs/superpowers/specs/2026-03-30-multi-agent-pipeline-design.md`

---

## File Structure

| File | Responsibility |
|------|---------------|
| **Create:** `crates/db/migrations/20260330120000_create_pipeline_states.sql` | DB migration for pipeline_states table |
| **Create:** `crates/db/src/models/pipeline_state.rs` | PipelineState model with CRUD operations |
| **Modify:** `crates/db/src/models/mod.rs` | Register pipeline_state module |
| **Create:** `crates/services/src/services/pipeline_controller.rs` | PipelineController state machine — verdict parsing, transitions, retry logic |
| **Modify:** `crates/services/src/services/mod.rs` | Register pipeline_controller module |
| **Create:** `crates/services/src/services/pipeline_types.rs` | Shared pipeline types: PipelineConfig, StageConfig, Verdict, HandoffArtifact |
| **Modify:** `crates/local-deployment/src/container.rs` | Call pipeline_controller from spawn_exit_monitor() |

---

### Task 1: DB Migration — Create pipeline_states Table

**Files:**
- Create: `crates/db/migrations/20260330120000_create_pipeline_states.sql`

- [ ] **Step 1: Create migration file**

```sql
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
```

- [ ] **Step 2: Verify migration compiles**

Run: `cd /Users/aipolikot/myai/Vibe-coding && cargo build --package db 2>&1 | tail -5`
Expected: Successful build (migrations are embedded at compile time via sqlx)

- [ ] **Step 3: Commit**

```bash
git add crates/db/migrations/20260330120000_create_pipeline_states.sql
git commit -m "feat(db): add pipeline_states table migration"
```

---

### Task 2: Pipeline Types — Shared Structs

**Files:**
- Create: `crates/services/src/services/pipeline_types.rs`
- Modify: `crates/services/src/services/mod.rs`

- [ ] **Step 1: Create pipeline_types.rs with all shared types**

```rust
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Pipeline configuration — snapshot stored at creation time
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct PipelineConfig {
    pub name: String,
    pub enable_second_agent: bool,
    pub default_max_retries: u32,
    pub auto_create_pr: bool,
    pub stages: Vec<StageConfig>,
}

/// Configuration for a single pipeline stage
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct StageConfig {
    pub id: String,
    pub role: String,
    pub agent: String,
    #[serde(default = "default_approval")]
    pub approval: String,
    pub on_success: String,
    pub on_fail: String,
    #[serde(default)]
    pub max_retries: Option<u32>,
    #[serde(default)]
    pub escalate_agent: Option<String>,
    #[serde(default)]
    pub escalate_after_retries: Option<u32>,
    #[serde(default)]
    pub workflow_profile: Option<String>,
    #[serde(default)]
    pub workflow_mode: Option<String>,
    #[serde(default)]
    pub policies: Vec<String>,
}

fn default_approval() -> String {
    "auto".to_string()
}

/// Verdict returned by an agent at the end of a stage
#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum VerdictStatus {
    Approved,
    NeedsChanges,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct Verdict {
    pub verdict: VerdictStatus,
    pub summary: String,
    #[serde(default)]
    pub issues: Vec<VerdictIssue>,
    #[serde(default)]
    pub revised_plan: Option<String>,
    #[serde(default)]
    pub what_changed: Vec<String>,
    #[serde(default)]
    pub why_changed: Vec<String>,
    #[serde(default)]
    pub unresolved_issues: Vec<String>,
    #[serde(default)]
    pub blockers: Vec<String>,
    #[serde(default)]
    pub missing_steps: Vec<String>,
    #[serde(default)]
    pub risks: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct VerdictIssue {
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub line: Option<u32>,
    pub description: String,
}

/// Structured handoff artifact passed between stages
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct HandoffArtifact {
    pub from_stage: String,
    pub to_stage: String,
    #[serde(default)]
    pub final_plan: Option<String>,
    #[serde(default)]
    pub review_summary: Option<String>,
    #[serde(default)]
    pub constraints: Vec<String>,
    #[serde(default)]
    pub risks: Vec<String>,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub issues: Vec<VerdictIssue>,
    #[serde(default)]
    pub test_report: Option<String>,
}

/// Entry in stage_history
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct StageHistoryEntry {
    pub stage_id: String,
    pub execution_process_id: String,
    pub result: String,
    pub verdict: Option<Verdict>,
    pub started_at: String,
    pub completed_at: Option<String>,
}

/// Pipeline status enum
#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PipelineStatus {
    Running,
    Paused,
    Completed,
    Failed,
    ReadyForPr,
}

impl PipelineStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::ReadyForPr => "ready_for_pr",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "running" => Some(Self::Running),
            "paused" => Some(Self::Paused),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "ready_for_pr" => Some(Self::ReadyForPr),
            _ => None,
        }
    }
}
```

- [ ] **Step 2: Register module in mod.rs**

Add to `crates/services/src/services/mod.rs`:

```rust
pub mod pipeline_types;
```

- [ ] **Step 3: Verify it compiles**

Run: `cd /Users/aipolikot/myai/Vibe-coding && cargo build --package services 2>&1 | tail -5`
Expected: Successful build

- [ ] **Step 4: Commit**

```bash
git add crates/services/src/services/pipeline_types.rs crates/services/src/services/mod.rs
git commit -m "feat(pipeline): add shared pipeline types — config, verdict, handoff"
```

---

### Task 3: PipelineState DB Model

**Files:**
- Create: `crates/db/src/models/pipeline_state.rs`
- Modify: `crates/db/src/models/mod.rs`

- [ ] **Step 1: Create pipeline_state.rs with model and CRUD**

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, SqlitePool};
use thiserror::Error;
use ts_rs::TS;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum PipelineStateError {
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error("Pipeline state not found")]
    NotFound,
    #[error("Pipeline already exists for workspace {0}")]
    AlreadyExists(Uuid),
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, TS)]
pub struct PipelineState {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub pipeline_config: String,
    pub current_stage_id: String,
    pub status: String,
    pub retry_counts: String,
    pub stage_history: String,
    pub handoff_artifacts: String,
    pub role_sessions: String,
    pub awaiting_approval: bool,
    pub approval_stage_id: Option<String>,
    pub approval_payload: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePipelineState {
    pub workspace_id: Uuid,
    pub pipeline_config: String,
    pub first_stage_id: String,
}

impl PipelineState {
    pub async fn create(
        pool: &SqlitePool,
        data: &CreatePipelineState,
        id: Uuid,
    ) -> Result<Self, PipelineStateError> {
        let existing = Self::find_by_workspace_id(pool, data.workspace_id).await?;
        if existing.is_some() {
            return Err(PipelineStateError::AlreadyExists(data.workspace_id));
        }

        Ok(sqlx::query_as!(
            PipelineState,
            r#"INSERT INTO pipeline_states (
                id, workspace_id, pipeline_config, current_stage_id, status,
                retry_counts, stage_history, handoff_artifacts, role_sessions,
                awaiting_approval
               )
               VALUES ($1, $2, $3, $4, 'running', '{}', '[]', '{}', '{}', 0)
               RETURNING
                id AS "id!: Uuid",
                workspace_id AS "workspace_id!: Uuid",
                pipeline_config,
                current_stage_id,
                status,
                retry_counts,
                stage_history,
                handoff_artifacts,
                role_sessions,
                awaiting_approval AS "awaiting_approval!: bool",
                approval_stage_id,
                approval_payload,
                created_at AS "created_at!: DateTime<Utc>",
                updated_at AS "updated_at!: DateTime<Utc>""#,
            id,
            data.workspace_id,
            data.pipeline_config,
            data.first_stage_id,
        )
        .fetch_one(pool)
        .await?)
    }

    pub async fn find_by_workspace_id(
        pool: &SqlitePool,
        workspace_id: Uuid,
    ) -> Result<Option<Self>, sqlx::Error> {
        sqlx::query_as!(
            PipelineState,
            r#"SELECT
                id AS "id!: Uuid",
                workspace_id AS "workspace_id!: Uuid",
                pipeline_config,
                current_stage_id,
                status,
                retry_counts,
                stage_history,
                handoff_artifacts,
                role_sessions,
                awaiting_approval AS "awaiting_approval!: bool",
                approval_stage_id,
                approval_payload,
                created_at AS "created_at!: DateTime<Utc>",
                updated_at AS "updated_at!: DateTime<Utc>"
               FROM pipeline_states
               WHERE workspace_id = $1"#,
            workspace_id
        )
        .fetch_optional(pool)
        .await
    }

    pub async fn update_stage(
        pool: &SqlitePool,
        workspace_id: Uuid,
        current_stage_id: &str,
        status: &str,
        retry_counts: &str,
        stage_history: &str,
        handoff_artifacts: &str,
        role_sessions: &str,
    ) -> Result<(), PipelineStateError> {
        let rows = sqlx::query!(
            r#"UPDATE pipeline_states SET
                current_stage_id = $1,
                status = $2,
                retry_counts = $3,
                stage_history = $4,
                handoff_artifacts = $5,
                role_sessions = $6,
                awaiting_approval = 0,
                approval_stage_id = NULL,
                approval_payload = NULL,
                updated_at = datetime('now', 'subsec')
               WHERE workspace_id = $7"#,
            current_stage_id,
            status,
            retry_counts,
            stage_history,
            handoff_artifacts,
            role_sessions,
            workspace_id,
        )
        .execute(pool)
        .await?
        .rows_affected();

        if rows == 0 {
            return Err(PipelineStateError::NotFound);
        }
        Ok(())
    }

    pub async fn set_approval(
        pool: &SqlitePool,
        workspace_id: Uuid,
        stage_id: &str,
        payload: &str,
    ) -> Result<(), PipelineStateError> {
        let rows = sqlx::query!(
            r#"UPDATE pipeline_states SET
                awaiting_approval = 1,
                approval_stage_id = $1,
                approval_payload = $2,
                status = 'paused',
                updated_at = datetime('now', 'subsec')
               WHERE workspace_id = $3"#,
            stage_id,
            payload,
            workspace_id,
        )
        .execute(pool)
        .await?
        .rows_affected();

        if rows == 0 {
            return Err(PipelineStateError::NotFound);
        }
        Ok(())
    }

    pub async fn clear_approval(
        pool: &SqlitePool,
        workspace_id: Uuid,
    ) -> Result<(), PipelineStateError> {
        let rows = sqlx::query!(
            r#"UPDATE pipeline_states SET
                awaiting_approval = 0,
                approval_stage_id = NULL,
                approval_payload = NULL,
                status = 'running',
                updated_at = datetime('now', 'subsec')
               WHERE workspace_id = $1"#,
            workspace_id,
        )
        .execute(pool)
        .await?
        .rows_affected();

        if rows == 0 {
            return Err(PipelineStateError::NotFound);
        }
        Ok(())
    }

    pub async fn set_status(
        pool: &SqlitePool,
        workspace_id: Uuid,
        status: &str,
    ) -> Result<(), PipelineStateError> {
        let rows = sqlx::query!(
            r#"UPDATE pipeline_states SET
                status = $1,
                updated_at = datetime('now', 'subsec')
               WHERE workspace_id = $2"#,
            status,
            workspace_id,
        )
        .execute(pool)
        .await?
        .rows_affected();

        if rows == 0 {
            return Err(PipelineStateError::NotFound);
        }
        Ok(())
    }

    pub async fn delete_by_workspace_id(
        pool: &SqlitePool,
        workspace_id: Uuid,
    ) -> Result<(), sqlx::Error> {
        sqlx::query!(
            "DELETE FROM pipeline_states WHERE workspace_id = $1",
            workspace_id,
        )
        .execute(pool)
        .await?;
        Ok(())
    }
}
```

- [ ] **Step 2: Register module in mod.rs**

Add to `crates/db/src/models/mod.rs`:

```rust
pub mod pipeline_state;
```

- [ ] **Step 3: Verify it compiles**

Run: `cd /Users/aipolikot/myai/Vibe-coding && cargo build --package db 2>&1 | tail -5`
Expected: Successful build

- [ ] **Step 4: Commit**

```bash
git add crates/db/src/models/pipeline_state.rs crates/db/src/models/mod.rs
git commit -m "feat(db): add PipelineState model with CRUD operations"
```

---

### Task 4: Verdict Parser

**Files:**
- Create: `crates/services/src/services/verdict_parser.rs`
- Modify: `crates/services/src/services/mod.rs`

- [ ] **Step 1: Create verdict_parser.rs**

```rust
use crate::services::pipeline_types::{Verdict, VerdictStatus};

/// Parse a verdict JSON block from the end of a CodingAgentTurn summary.
///
/// Strategy: scan the last 1024 characters of the summary for a JSON block
/// matching the verdict schema. The agent is instructed to end its response
/// with a ```json verdict block.
///
/// Returns None if no valid verdict is found (will be treated as failed/paused).
pub fn parse_verdict(summary: &str) -> Option<Verdict> {
    // Take the last 1024 characters to handle truncation
    let search_range = if summary.len() > 1024 {
        &summary[summary.len() - 1024..]
    } else {
        summary
    };

    // Find the last JSON block that contains "verdict"
    let mut best_verdict: Option<Verdict> = None;

    // Try to find ```json blocks first (most reliable)
    for block in extract_json_code_blocks(search_range) {
        if let Some(v) = try_parse_verdict_json(block) {
            best_verdict = Some(v);
        }
    }

    // If no code block found, try to find raw JSON with "verdict" key
    if best_verdict.is_none() {
        if let Some(v) = try_find_raw_verdict_json(search_range) {
            best_verdict = Some(v);
        }
    }

    best_verdict
}

/// Extract content from ```json ... ``` code blocks
fn extract_json_code_blocks(text: &str) -> Vec<&str> {
    let mut blocks = Vec::new();
    let mut search_from = 0;

    while let Some(start_marker) = text[search_from..].find("```json") {
        let content_start = search_from + start_marker + 7; // len("```json")
        // Skip whitespace/newline after ```json
        let content_start = text[content_start..]
            .find(|c: char| !c.is_whitespace())
            .map(|i| content_start + i)
            .unwrap_or(content_start);

        if let Some(end_marker) = text[content_start..].find("```") {
            let content_end = content_start + end_marker;
            let block = text[content_start..content_end].trim();
            if !block.is_empty() {
                blocks.push(block);
            }
            search_from = content_end + 3;
        } else {
            break;
        }
    }

    blocks
}

/// Try to parse a string as a Verdict JSON
fn try_parse_verdict_json(json_str: &str) -> Option<Verdict> {
    let verdict: Verdict = serde_json::from_str(json_str).ok()?;
    // Validate it has a verdict field (serde would have parsed it)
    Some(verdict)
}

/// Try to find a raw JSON object containing "verdict" key
fn try_find_raw_verdict_json(text: &str) -> Option<Verdict> {
    // Find positions of { that might start a verdict object
    let mut pos = text.len();
    while let Some(brace_pos) = text[..pos].rfind('{') {
        let candidate = &text[brace_pos..];
        // Find the matching closing brace
        if let Some(end) = find_matching_brace(candidate) {
            let json_str = &candidate[..=end];
            if json_str.contains("\"verdict\"") {
                if let Some(v) = try_parse_verdict_json(json_str) {
                    return Some(v);
                }
            }
        }
        pos = brace_pos;
    }
    None
}

/// Find the position of the matching closing brace
fn find_matching_brace(s: &str) -> Option<usize> {
    let mut depth = 0;
    let mut in_string = false;
    let mut escape_next = false;

    for (i, ch) in s.char_indices() {
        if escape_next {
            escape_next = false;
            continue;
        }
        match ch {
            '\\' if in_string => escape_next = true,
            '"' => in_string = !in_string,
            '{' if !in_string => depth += 1,
            '}' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_verdict_from_code_block() {
        let summary = r#"I reviewed the plan carefully.

```json
{"verdict": "approved", "summary": "Plan looks good", "issues": []}
```"#;
        let verdict = parse_verdict(summary).unwrap();
        assert_eq!(verdict.verdict, VerdictStatus::Approved);
        assert_eq!(verdict.summary, "Plan looks good");
    }

    #[test]
    fn test_parse_verdict_needs_changes() {
        let summary = r#"Found some issues.

```json
{"verdict": "needs_changes", "summary": "Missing error handling", "issues": [{"description": "No rollback on failure"}]}
```"#;
        let verdict = parse_verdict(summary).unwrap();
        assert_eq!(verdict.verdict, VerdictStatus::NeedsChanges);
        assert_eq!(verdict.issues.len(), 1);
        assert_eq!(verdict.issues[0].description, "No rollback on failure");
    }

    #[test]
    fn test_parse_verdict_from_raw_json() {
        let summary = r#"Review complete. {"verdict": "approved", "summary": "All good", "issues": []}"#;
        let verdict = parse_verdict(summary).unwrap();
        assert_eq!(verdict.verdict, VerdictStatus::Approved);
    }

    #[test]
    fn test_parse_verdict_not_found() {
        let summary = "I finished the work. Everything looks fine.";
        assert!(parse_verdict(summary).is_none());
    }

    #[test]
    fn test_parse_verdict_truncated_summary() {
        // Simulate a long summary where verdict is at the end
        let padding = "x".repeat(5000);
        let summary = format!(
            r#"{}

```json
{{"verdict": "approved", "summary": "Done", "issues": []}}
```"#,
            padding
        );
        let verdict = parse_verdict(&summary).unwrap();
        assert_eq!(verdict.verdict, VerdictStatus::Approved);
    }

    #[test]
    fn test_parse_verdict_with_consensus_fields() {
        let summary = r#"```json
{"verdict": "needs_changes", "summary": "Improved plan", "issues": [], "revised_plan": "new plan text", "what_changed": ["added step"], "why_changed": ["missing rollback"]}
```"#;
        let verdict = parse_verdict(summary).unwrap();
        assert_eq!(verdict.verdict, VerdictStatus::NeedsChanges);
        assert_eq!(verdict.revised_plan, Some("new plan text".to_string()));
        assert_eq!(verdict.what_changed, vec!["added step"]);
    }
}
```

- [ ] **Step 2: Register module in mod.rs**

Add to `crates/services/src/services/mod.rs`:

```rust
pub mod verdict_parser;
```

- [ ] **Step 3: Run tests**

Run: `cd /Users/aipolikot/myai/Vibe-coding && cargo test --package services verdict_parser 2>&1 | tail -15`
Expected: All 5 tests pass

- [ ] **Step 4: Commit**

```bash
git add crates/services/src/services/verdict_parser.rs crates/services/src/services/mod.rs
git commit -m "feat(pipeline): add verdict parser with JSON extraction from summary"
```

---

### Task 5: PipelineController — Core State Machine

**Files:**
- Create: `crates/services/src/services/pipeline_controller.rs`
- Modify: `crates/services/src/services/mod.rs`

- [ ] **Step 1: Create pipeline_controller.rs with transition logic**

```rust
use std::collections::HashMap;

use db::{
    models::{
        coding_agent_turn::CodingAgentTurn,
        execution_process::ExecutionContext,
        pipeline_state::{PipelineState, PipelineStateError},
    },
    DBService,
};
use sqlx::SqlitePool;
use thiserror::Error;
use tracing;
use uuid::Uuid;

use crate::services::{
    pipeline_types::{
        HandoffArtifact, PipelineConfig, PipelineStatus, StageConfig, StageHistoryEntry,
        Verdict, VerdictStatus,
    },
    verdict_parser,
};

#[derive(Debug, Error)]
pub enum PipelineControllerError {
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    PipelineState(#[from] PipelineStateError),
    #[error("Pipeline not found for workspace {0}")]
    NotFound(Uuid),
    #[error("Pipeline config parse error: {0}")]
    ConfigParseError(String),
    #[error("Stage not found in config: {0}")]
    StageNotFound(String),
    #[error("Execution process mismatch: expected current stage execution")]
    ExecutionMismatch,
}

/// Result of a pipeline transition — tells the caller what to do next
#[derive(Debug)]
pub enum TransitionAction {
    /// Start the next stage with this prompt and config
    StartStage {
        stage_id: String,
        role: String,
        agent: String,
        session_id: Option<String>,
        prompt_additions: String,
        is_follow_up: bool,
    },
    /// Pipeline is paused — waiting for user approval
    AwaitApproval {
        stage_id: String,
    },
    /// Pipeline is complete
    Completed,
    /// Pipeline is ready for PR creation
    ReadyForPr,
    /// Pipeline is paused due to error or max retries
    Paused {
        reason: String,
    },
    /// No pipeline for this workspace — skip
    NoPipeline,
}

pub struct PipelineController;

impl PipelineController {
    /// Called from spawn_exit_monitor() after an execution completes.
    /// Returns what action the caller should take next.
    pub async fn handle_stage_completed(
        pool: &SqlitePool,
        ctx: &ExecutionContext,
    ) -> Result<TransitionAction, PipelineControllerError> {
        // 1. Load pipeline state for this workspace
        let pipeline_state =
            match PipelineState::find_by_workspace_id(pool, ctx.workspace.id).await? {
                Some(state) => state,
                None => return Ok(TransitionAction::NoPipeline),
            };

        // Skip if pipeline is not running
        if pipeline_state.status != "running" {
            tracing::debug!(
                "Pipeline for workspace {} is not running (status: {}), skipping",
                ctx.workspace.id,
                pipeline_state.status
            );
            return Ok(TransitionAction::NoPipeline);
        }

        // 2. Parse pipeline config
        let config: PipelineConfig = serde_json::from_str(&pipeline_state.pipeline_config)
            .map_err(|e| PipelineControllerError::ConfigParseError(e.to_string()))?;

        // 3. Find current stage config
        let current_stage = config
            .stages
            .iter()
            .find(|s| s.id == pipeline_state.current_stage_id)
            .ok_or_else(|| {
                PipelineControllerError::StageNotFound(
                    pipeline_state.current_stage_id.clone(),
                )
            })?;

        // 4. Dedupe check — verify this execution belongs to current stage
        let stage_history: Vec<StageHistoryEntry> =
            serde_json::from_str(&pipeline_state.stage_history).unwrap_or_default();

        if let Some(last_entry) = stage_history.last() {
            if last_entry.execution_process_id == ctx.execution_process.id.to_string()
                && last_entry.completed_at.is_some()
            {
                tracing::debug!("Execution {} already processed, skipping", ctx.execution_process.id);
                return Ok(TransitionAction::NoPipeline);
            }
        }

        // 5. Parse verdict from CodingAgentTurn summary
        let verdict = Self::extract_verdict(pool, ctx).await;

        // 6. Update stage history
        let mut stage_history = stage_history;
        Self::update_stage_history(
            &mut stage_history,
            current_stage,
            ctx,
            &verdict,
        );

        // 7. Determine transition based on verdict
        let mut retry_counts: HashMap<String, u32> =
            serde_json::from_str(&pipeline_state.retry_counts).unwrap_or_default();
        let mut handoff_artifacts: HashMap<String, HandoffArtifact> =
            serde_json::from_str(&pipeline_state.handoff_artifacts).unwrap_or_default();
        let role_sessions: HashMap<String, String> =
            serde_json::from_str(&pipeline_state.role_sessions).unwrap_or_default();

        let transition = Self::determine_transition(
            &config,
            current_stage,
            &verdict,
            &mut retry_counts,
            &mut handoff_artifacts,
            &role_sessions,
        );

        // 8. Persist updated state
        let (next_stage_id, next_status) = match &transition {
            TransitionAction::StartStage { stage_id, .. } => {
                (stage_id.clone(), PipelineStatus::Running)
            }
            TransitionAction::AwaitApproval { stage_id } => {
                (stage_id.clone(), PipelineStatus::Paused)
            }
            TransitionAction::Completed => {
                (pipeline_state.current_stage_id.clone(), PipelineStatus::Completed)
            }
            TransitionAction::ReadyForPr => {
                (pipeline_state.current_stage_id.clone(), PipelineStatus::ReadyForPr)
            }
            TransitionAction::Paused { .. } => {
                (pipeline_state.current_stage_id.clone(), PipelineStatus::Paused)
            }
            TransitionAction::NoPipeline => {
                return Ok(TransitionAction::NoPipeline);
            }
        };

        PipelineState::update_stage(
            pool,
            ctx.workspace.id,
            &next_stage_id,
            next_status.as_str(),
            &serde_json::to_string(&retry_counts).unwrap_or_default(),
            &serde_json::to_string(&stage_history).unwrap_or_default(),
            &serde_json::to_string(&handoff_artifacts).unwrap_or_default(),
            &serde_json::to_string(&role_sessions).unwrap_or_default(),
        )
        .await?;

        // Handle approval pause
        if let TransitionAction::AwaitApproval { ref stage_id } = transition {
            let payload = serde_json::to_string(
                &handoff_artifacts.get(&current_stage.id),
            )
            .unwrap_or_default();
            PipelineState::set_approval(pool, ctx.workspace.id, stage_id, &payload).await?;
        }

        Ok(transition)
    }

    /// Extract verdict from the latest CodingAgentTurn summary
    async fn extract_verdict(
        pool: &SqlitePool,
        ctx: &ExecutionContext,
    ) -> Option<Verdict> {
        let turn = CodingAgentTurn::find_by_execution_process_id(
            pool,
            ctx.execution_process.id,
        )
        .await
        .ok()
        .flatten();

        let summary = turn.and_then(|t| t.summary)?;
        verdict_parser::parse_verdict(&summary)
    }

    /// Update stage history with the completed stage
    fn update_stage_history(
        history: &mut Vec<StageHistoryEntry>,
        stage: &StageConfig,
        ctx: &ExecutionContext,
        verdict: &Option<Verdict>,
    ) {
        let result = match verdict {
            Some(v) => match v.verdict {
                VerdictStatus::Approved => "approved",
                VerdictStatus::NeedsChanges => "needs_changes",
                VerdictStatus::Failed => "failed",
            },
            None => "no_verdict",
        };

        // Check if there's an in-progress entry for this stage
        if let Some(entry) = history.iter_mut().rev().find(|e| {
            e.stage_id == stage.id && e.completed_at.is_none()
        }) {
            entry.result = result.to_string();
            entry.verdict = verdict.clone();
            entry.completed_at = Some(chrono::Utc::now().to_rfc3339());
        } else {
            history.push(StageHistoryEntry {
                stage_id: stage.id.clone(),
                execution_process_id: ctx.execution_process.id.to_string(),
                result: result.to_string(),
                verdict: verdict.clone(),
                started_at: chrono::Utc::now().to_rfc3339(),
                completed_at: Some(chrono::Utc::now().to_rfc3339()),
            });
        }
    }

    /// Core transition logic — determines what happens next based on verdict
    fn determine_transition(
        config: &PipelineConfig,
        current_stage: &StageConfig,
        verdict: &Option<Verdict>,
        retry_counts: &mut HashMap<String, u32>,
        handoff_artifacts: &mut HashMap<String, HandoffArtifact>,
        role_sessions: &HashMap<String, String>,
    ) -> TransitionAction {
        match verdict {
            Some(v) if v.verdict == VerdictStatus::Approved => {
                // Build handoff artifact
                let handoff = HandoffArtifact {
                    from_stage: current_stage.id.clone(),
                    to_stage: current_stage.on_success.clone(),
                    final_plan: v.revised_plan.clone(),
                    review_summary: Some(v.summary.clone()),
                    constraints: Vec::new(),
                    risks: v.risks.clone(),
                    acceptance_criteria: Vec::new(),
                    issues: Vec::new(),
                    test_report: None,
                };
                handoff_artifacts.insert(current_stage.id.clone(), handoff);

                // Check if pipeline is complete
                if current_stage.on_success == "complete" {
                    if config.auto_create_pr {
                        return TransitionAction::ReadyForPr;
                    }
                    return TransitionAction::Completed;
                }

                // Find next stage
                let next_stage = config.stages.iter().find(|s| s.id == current_stage.on_success);
                match next_stage {
                    Some(next) => {
                        // Check if next stage requires approval (first entry only)
                        let retry_key = format!("{}→{}", current_stage.id, next.id);
                        let is_first_entry = retry_counts.get(&retry_key).copied().unwrap_or(0) == 0;

                        if next.approval == "approval" && is_first_entry {
                            return TransitionAction::AwaitApproval {
                                stage_id: next.id.clone(),
                            };
                        }

                        let session_id = role_sessions.get(&next.role).cloned();

                        TransitionAction::StartStage {
                            stage_id: next.id.clone(),
                            role: next.role.clone(),
                            agent: next.agent.clone(),
                            session_id,
                            prompt_additions: String::new(),
                            is_follow_up: session_id.is_some(),
                        }
                    }
                    None => TransitionAction::Paused {
                        reason: format!("Next stage '{}' not found in config", current_stage.on_success),
                    },
                }
            }
            Some(v) if v.verdict == VerdictStatus::NeedsChanges => {
                let retry_key = format!(
                    "{}→{}",
                    current_stage.id, current_stage.on_fail
                );
                let current_retries = retry_counts.get(&retry_key).copied().unwrap_or(0);
                let max_retries = current_stage
                    .max_retries
                    .unwrap_or(config.default_max_retries);

                if current_retries >= max_retries {
                    // Check for escalation
                    if let Some(ref escalate_agent) = current_stage.escalate_agent {
                        let escalate_after = current_stage.escalate_after_retries.unwrap_or(max_retries);
                        if current_retries >= escalate_after && config.enable_second_agent {
                            return TransitionAction::StartStage {
                                stage_id: current_stage.id.clone(),
                                role: current_stage.role.clone(),
                                agent: escalate_agent.clone(),
                                session_id: None,
                                prompt_additions: format!(
                                    "ESCALATION: Primary agent failed after {} attempts. Review the entire implementation.",
                                    current_retries
                                ),
                                is_follow_up: false,
                            };
                        }
                    }

                    return TransitionAction::Paused {
                        reason: format!(
                            "Stage '{}' exceeded max retries ({}/{})",
                            current_stage.id, current_retries, max_retries
                        ),
                    };
                }

                // Increment retry count
                retry_counts.insert(retry_key, current_retries + 1);

                // Store feedback as handoff
                let handoff = HandoffArtifact {
                    from_stage: current_stage.id.clone(),
                    to_stage: current_stage.on_fail.clone(),
                    final_plan: v.revised_plan.clone(),
                    review_summary: Some(v.summary.clone()),
                    constraints: Vec::new(),
                    risks: v.risks.clone(),
                    acceptance_criteria: Vec::new(),
                    issues: v.issues.clone(),
                    test_report: None,
                };
                handoff_artifacts.insert(
                    format!("{}_feedback", current_stage.id),
                    handoff,
                );

                // Go to on_fail stage
                if current_stage.on_fail == "pause" {
                    return TransitionAction::Paused {
                        reason: format!("Stage '{}' needs changes", current_stage.id),
                    };
                }

                let fail_stage = config
                    .stages
                    .iter()
                    .find(|s| s.id == current_stage.on_fail);

                match fail_stage {
                    Some(target) => {
                        let session_id = role_sessions.get(&target.role).cloned();
                        TransitionAction::StartStage {
                            stage_id: target.id.clone(),
                            role: target.role.clone(),
                            agent: target.agent.clone(),
                            session_id,
                            prompt_additions: format!(
                                "Feedback from {}: {}",
                                current_stage.role, v.summary
                            ),
                            is_follow_up: session_id.is_some(),
                        }
                    }
                    None => TransitionAction::Paused {
                        reason: format!(
                            "On-fail stage '{}' not found in config",
                            current_stage.on_fail
                        ),
                    },
                }
            }
            _ => {
                // No verdict or failed — pause pipeline
                TransitionAction::Paused {
                    reason: format!(
                        "Stage '{}' completed without valid verdict",
                        current_stage.id
                    ),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::pipeline_types::*;

    fn make_config() -> PipelineConfig {
        PipelineConfig {
            name: "Test Pipeline".to_string(),
            enable_second_agent: false,
            default_max_retries: 3,
            auto_create_pr: false,
            stages: vec![
                StageConfig {
                    id: "planner".to_string(),
                    role: "planner".to_string(),
                    agent: "claude_code".to_string(),
                    approval: "auto".to_string(),
                    on_success: "reviewer".to_string(),
                    on_fail: "pause".to_string(),
                    max_retries: None,
                    escalate_agent: None,
                    escalate_after_retries: None,
                    workflow_profile: None,
                    workflow_mode: None,
                    policies: vec![],
                },
                StageConfig {
                    id: "reviewer".to_string(),
                    role: "reviewer".to_string(),
                    agent: "claude_code".to_string(),
                    approval: "approval".to_string(),
                    on_success: "builder".to_string(),
                    on_fail: "planner".to_string(),
                    max_retries: Some(7),
                    escalate_agent: None,
                    escalate_after_retries: None,
                    workflow_profile: None,
                    workflow_mode: Some("consensus".to_string()),
                    policies: vec![],
                },
                StageConfig {
                    id: "builder".to_string(),
                    role: "builder".to_string(),
                    agent: "claude_code".to_string(),
                    approval: "auto".to_string(),
                    on_success: "complete".to_string(),
                    on_fail: "pause".to_string(),
                    max_retries: None,
                    escalate_agent: None,
                    escalate_after_retries: None,
                    workflow_profile: None,
                    workflow_mode: None,
                    policies: vec![],
                },
            ],
        }
    }

    #[test]
    fn test_transition_approved_goes_to_next_stage() {
        let config = make_config();
        let planner = &config.stages[0];
        let verdict = Some(Verdict {
            verdict: VerdictStatus::Approved,
            summary: "Plan is good".to_string(),
            issues: vec![],
            revised_plan: None,
            what_changed: vec![],
            why_changed: vec![],
            unresolved_issues: vec![],
            blockers: vec![],
            missing_steps: vec![],
            risks: vec![],
        });

        let mut retry_counts = HashMap::new();
        let mut handoff_artifacts = HashMap::new();
        let role_sessions = HashMap::new();

        let action = PipelineController::determine_transition(
            &config,
            planner,
            &verdict,
            &mut retry_counts,
            &mut handoff_artifacts,
            &role_sessions,
        );

        // Reviewer has approval=approval, first entry → AwaitApproval
        match action {
            TransitionAction::AwaitApproval { stage_id } => {
                assert_eq!(stage_id, "reviewer");
            }
            other => panic!("Expected AwaitApproval, got {:?}", other),
        }
    }

    #[test]
    fn test_transition_needs_changes_retries() {
        let config = make_config();
        let reviewer = &config.stages[1];
        let verdict = Some(Verdict {
            verdict: VerdictStatus::NeedsChanges,
            summary: "Plan needs work".to_string(),
            issues: vec![],
            revised_plan: Some("improved plan".to_string()),
            what_changed: vec!["added step".to_string()],
            why_changed: vec!["missing rollback".to_string()],
            unresolved_issues: vec![],
            blockers: vec![],
            missing_steps: vec![],
            risks: vec![],
        });

        let mut retry_counts = HashMap::new();
        let mut handoff_artifacts = HashMap::new();
        let role_sessions = HashMap::new();

        let action = PipelineController::determine_transition(
            &config,
            reviewer,
            &verdict,
            &mut retry_counts,
            &mut handoff_artifacts,
            &role_sessions,
        );

        match action {
            TransitionAction::StartStage { stage_id, .. } => {
                assert_eq!(stage_id, "planner");
            }
            other => panic!("Expected StartStage(planner), got {:?}", other),
        }

        // Verify retry count incremented
        assert_eq!(retry_counts.get("reviewer→planner"), Some(&1));
    }

    #[test]
    fn test_transition_max_retries_pauses() {
        let config = make_config();
        let reviewer = &config.stages[1];
        let verdict = Some(Verdict {
            verdict: VerdictStatus::NeedsChanges,
            summary: "Still bad".to_string(),
            issues: vec![],
            revised_plan: None,
            what_changed: vec![],
            why_changed: vec![],
            unresolved_issues: vec![],
            blockers: vec![],
            missing_steps: vec![],
            risks: vec![],
        });

        let mut retry_counts = HashMap::new();
        retry_counts.insert("reviewer→planner".to_string(), 7); // Already at max
        let mut handoff_artifacts = HashMap::new();
        let role_sessions = HashMap::new();

        let action = PipelineController::determine_transition(
            &config,
            reviewer,
            &verdict,
            &mut retry_counts,
            &mut handoff_artifacts,
            &role_sessions,
        );

        match action {
            TransitionAction::Paused { reason } => {
                assert!(reason.contains("exceeded max retries"));
            }
            other => panic!("Expected Paused, got {:?}", other),
        }
    }

    #[test]
    fn test_transition_no_verdict_pauses() {
        let config = make_config();
        let planner = &config.stages[0];

        let mut retry_counts = HashMap::new();
        let mut handoff_artifacts = HashMap::new();
        let role_sessions = HashMap::new();

        let action = PipelineController::determine_transition(
            &config,
            planner,
            &None,
            &mut retry_counts,
            &mut handoff_artifacts,
            &role_sessions,
        );

        match action {
            TransitionAction::Paused { reason } => {
                assert!(reason.contains("without valid verdict"));
            }
            other => panic!("Expected Paused, got {:?}", other),
        }
    }

    #[test]
    fn test_transition_complete_stage() {
        let config = make_config();
        let builder = &config.stages[2]; // on_success = "complete"
        let verdict = Some(Verdict {
            verdict: VerdictStatus::Approved,
            summary: "Build complete".to_string(),
            issues: vec![],
            revised_plan: None,
            what_changed: vec![],
            why_changed: vec![],
            unresolved_issues: vec![],
            blockers: vec![],
            missing_steps: vec![],
            risks: vec![],
        });

        let mut retry_counts = HashMap::new();
        let mut handoff_artifacts = HashMap::new();
        let role_sessions = HashMap::new();

        let action = PipelineController::determine_transition(
            &config,
            builder,
            &verdict,
            &mut retry_counts,
            &mut handoff_artifacts,
            &role_sessions,
        );

        match action {
            TransitionAction::Completed => {}
            other => panic!("Expected Completed, got {:?}", other),
        }
    }
}
```

- [ ] **Step 2: Register module**

Add to `crates/services/src/services/mod.rs`:

```rust
pub mod pipeline_controller;
```

- [ ] **Step 3: Check if CodingAgentTurn has find_by_execution_process_id method**

Run: `cd /Users/aipolikot/myai/Vibe-coding && grep -n "find_by_execution_process_id\|find_latest_by_execution" crates/db/src/models/coding_agent_turn.rs`

If it doesn't exist, add it to `crates/db/src/models/coding_agent_turn.rs`:

```rust
pub async fn find_by_execution_process_id(
    pool: &SqlitePool,
    execution_process_id: Uuid,
) -> Result<Option<Self>, sqlx::Error> {
    sqlx::query_as!(
        CodingAgentTurn,
        r#"SELECT
            id AS "id!: Uuid",
            execution_process_id AS "execution_process_id!: Uuid",
            agent_session_id,
            agent_message_id,
            prompt,
            summary,
            seen AS "seen!: bool",
            created_at AS "created_at!: DateTime<Utc>",
            updated_at AS "updated_at!: DateTime<Utc>"
           FROM coding_agent_turns
           WHERE execution_process_id = $1
           ORDER BY created_at DESC
           LIMIT 1"#,
        execution_process_id
    )
    .fetch_optional(pool)
    .await
}
```

- [ ] **Step 4: Run tests**

Run: `cd /Users/aipolikot/myai/Vibe-coding && cargo test --package services pipeline_controller 2>&1 | tail -15`
Expected: All 5 tests pass

- [ ] **Step 5: Commit**

```bash
git add crates/services/src/services/pipeline_controller.rs crates/services/src/services/mod.rs crates/db/src/models/coding_agent_turn.rs
git commit -m "feat(pipeline): add PipelineController state machine with transition logic"
```

---

### Task 6: Exit Monitor Integration

**Files:**
- Modify: `crates/local-deployment/src/container.rs` (~line 597)

- [ ] **Step 1: Add import for PipelineController**

At the top of `crates/local-deployment/src/container.rs`, add to the existing `use services::` block:

```rust
use services::services::pipeline_controller::{PipelineController, TransitionAction};
```

- [ ] **Step 2: Add pipeline controller call in spawn_exit_monitor**

In `crates/local-deployment/src/container.rs`, find the block around line 597 inside `if should_start_next {`:

**Before (existing code):**
```rust
if should_start_next {
    if let Err(e) = container.try_start_next_action(&ctx).await {
        tracing::error!("Failed to start next action after completion: {}", e);
    }
}
```

**After (modified code):**
```rust
if should_start_next {
    // Check if this workspace has an active pipeline
    match PipelineController::handle_stage_completed(
        &container.db().pool,
        &ctx,
    ).await {
        Ok(TransitionAction::NoPipeline) => {
            // No pipeline — fall through to existing behavior
            if let Err(e) = container.try_start_next_action(&ctx).await {
                tracing::error!("Failed to start next action after completion: {}", e);
            }
        }
        Ok(TransitionAction::StartStage { stage_id, role, agent, .. }) => {
            tracing::info!(
                "Pipeline transition: {} -> {} (agent: {})",
                ctx.execution_process.id,
                stage_id,
                agent
            );
            // TODO Phase 2: Start the next stage execution
            // For now, just log the transition
        }
        Ok(TransitionAction::AwaitApproval { stage_id }) => {
            tracing::info!(
                "Pipeline awaiting approval for stage: {}",
                stage_id
            );
            // TODO Phase 2: Send notification
        }
        Ok(TransitionAction::Completed) => {
            tracing::info!("Pipeline completed for workspace {}", ctx.workspace.id);
        }
        Ok(TransitionAction::ReadyForPr) => {
            tracing::info!("Pipeline ready for PR for workspace {}", ctx.workspace.id);
            // TODO Phase 2: Auto-create PR if conditions met
        }
        Ok(TransitionAction::Paused { reason }) => {
            tracing::warn!(
                "Pipeline paused for workspace {}: {}",
                ctx.workspace.id,
                reason
            );
            // TODO Phase 2: Send notification
        }
        Err(e) => {
            tracing::error!(
                "Pipeline controller error for workspace {}: {}",
                ctx.workspace.id,
                e
            );
            // Fall through to existing behavior on error
            if let Err(e) = container.try_start_next_action(&ctx).await {
                tracing::error!("Failed to start next action after completion: {}", e);
            }
        }
    }
}
```

- [ ] **Step 3: Verify it compiles**

Run: `cd /Users/aipolikot/myai/Vibe-coding && cargo build --package local-deployment 2>&1 | tail -10`
Expected: Successful build (may need to add `services` to local-deployment's Cargo.toml dependencies if not already there)

- [ ] **Step 4: If build fails due to missing dependency, add it**

Check `crates/local-deployment/Cargo.toml` for `services` dependency. If missing:

```toml
[dependencies]
services = { path = "../services" }
```

- [ ] **Step 5: Verify full build**

Run: `cd /Users/aipolikot/myai/Vibe-coding && cargo build 2>&1 | tail -10`
Expected: Successful build

- [ ] **Step 6: Commit**

```bash
git add crates/local-deployment/src/container.rs
git commit -m "feat(pipeline): integrate PipelineController into spawn_exit_monitor"
```

---

### Task 7: SQLx Offline Preparation

**Files:**
- Regenerate: SQLx offline query data

- [ ] **Step 1: Prepare SQLx offline data**

The project uses SQLx compile-time query checking. After adding new queries, regenerate:

Run: `cd /Users/aipolikot/myai/Vibe-coding && pnpm run prepare-db 2>&1 | tail -5`
Expected: SQLx query data regenerated successfully

- [ ] **Step 2: Run full test suite**

Run: `cd /Users/aipolikot/myai/Vibe-coding && cargo test --workspace 2>&1 | tail -20`
Expected: All existing tests pass, plus new verdict_parser and pipeline_controller tests

- [ ] **Step 3: Commit SQLx data**

```bash
git add -A
git commit -m "chore: regenerate SQLx offline data for pipeline_states"
```

---

## Self-Review Checklist

**Spec coverage:**
- [x] pipeline_states table — Task 1
- [x] PipelineState model with CRUD — Task 3
- [x] PipelineConfig, StageConfig, Verdict types — Task 2
- [x] Verdict parsing from summary (last 1024 chars) — Task 4
- [x] PipelineController state machine — Task 5
- [x] Transition table (approved/needs_changes/failed) — Task 5
- [x] Retry logic with max_retries — Task 5
- [x] Escalation logic — Task 5
- [x] Approval on first entry only — Task 5
- [x] Exit monitor integration — Task 6
- [x] Idempotency (dedupe check) — Task 5
- [x] Workflow profile fields in types — Task 2
- [ ] Stage execution (actually starting next agent) — **Phase 2**
- [ ] API endpoints (approve/reject/pause) — **Phase 2**
- [ ] Notifications — **Phase 2**
- [ ] Frontend UI — **Phase 3**

**Placeholder scan:** No TBD/TODO in implementation code. Phase 2 TODOs are explicitly marked in exit monitor integration (Task 6) as expected — they log transitions but don't start executions yet.

**Type consistency:** Verified — `PipelineConfig`, `StageConfig`, `Verdict`, `VerdictStatus`, `HandoffArtifact`, `StageHistoryEntry`, `PipelineStatus` used consistently across all tasks.
