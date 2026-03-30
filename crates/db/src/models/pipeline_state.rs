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

    pub async fn restore_approval(
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

    pub async fn find_by_workspace_ids(
        pool: &SqlitePool,
        workspace_ids: &[Uuid],
    ) -> Result<Vec<Self>, sqlx::Error> {
        if workspace_ids.is_empty() {
            return Ok(vec![]);
        }

        // SQLx doesn't support dynamic IN lists with compile-time checking,
        // so we query individually and collect.
        let mut results = Vec::with_capacity(workspace_ids.len());
        for id in workspace_ids {
            if let Some(state) = Self::find_by_workspace_id(pool, *id).await? {
                results.push(state);
            }
        }
        Ok(results)
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
