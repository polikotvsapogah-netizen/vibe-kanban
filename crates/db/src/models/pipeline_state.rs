use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, QueryBuilder, Sqlite, SqlitePool};
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

        let mut query_builder = QueryBuilder::<Sqlite>::new(
            r#"SELECT
                id,
                workspace_id,
                pipeline_config,
                current_stage_id,
                status,
                retry_counts,
                stage_history,
                handoff_artifacts,
                role_sessions,
                awaiting_approval,
                approval_stage_id,
                approval_payload,
                created_at,
                updated_at
               FROM pipeline_states
               WHERE workspace_id IN ("#,
        );

        let mut separated = query_builder.separated(", ");
        for workspace_id in workspace_ids {
            separated.push_bind(workspace_id);
        }
        separated.push_unseparated(")");

        query_builder
            .build_query_as::<PipelineState>()
            .fetch_all(pool)
            .await
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

#[cfg(test)]
mod tests {
    use sqlx::SqlitePool;
    use uuid::Uuid;

    use super::{CreatePipelineState, PipelineState};

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query(
            r#"
            CREATE TABLE pipeline_states (
                id BLOB PRIMARY KEY,
                workspace_id BLOB NOT NULL UNIQUE,
                pipeline_config TEXT NOT NULL,
                current_stage_id TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'running',
                retry_counts TEXT NOT NULL DEFAULT '{}',
                stage_history TEXT NOT NULL DEFAULT '[]',
                handoff_artifacts TEXT NOT NULL DEFAULT '{}',
                role_sessions TEXT NOT NULL DEFAULT '{}',
                awaiting_approval INTEGER NOT NULL DEFAULT 0,
                approval_stage_id TEXT,
                approval_payload TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now', 'subsec')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now', 'subsec'))
            )
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    #[tokio::test]
    async fn find_by_workspace_ids_returns_all_matching_states_in_one_call() {
        let pool = test_pool().await;

        let first_workspace_id = Uuid::new_v4();
        let second_workspace_id = Uuid::new_v4();
        let missing_workspace_id = Uuid::new_v4();

        PipelineState::create(
            &pool,
            &CreatePipelineState {
                workspace_id: first_workspace_id,
                pipeline_config: r#"{"name":"a","stages":[]}"#.to_string(),
                first_stage_id: "planner".to_string(),
            },
            Uuid::new_v4(),
        )
        .await
        .unwrap();

        PipelineState::create(
            &pool,
            &CreatePipelineState {
                workspace_id: second_workspace_id,
                pipeline_config: r#"{"name":"b","stages":[]}"#.to_string(),
                first_stage_id: "reviewer".to_string(),
            },
            Uuid::new_v4(),
        )
        .await
        .unwrap();

        let states = PipelineState::find_by_workspace_ids(
            &pool,
            &[
                second_workspace_id,
                missing_workspace_id,
                first_workspace_id,
            ],
        )
        .await
        .unwrap();

        assert_eq!(states.len(), 2);
        assert!(
            states
                .iter()
                .any(|state| state.workspace_id == first_workspace_id)
        );
        assert!(
            states
                .iter()
                .any(|state| state.workspace_id == second_workspace_id)
        );
        assert!(
            !states
                .iter()
                .any(|state| state.workspace_id == missing_workspace_id)
        );
    }
}
