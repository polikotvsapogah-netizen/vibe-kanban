use std::collections::HashMap;

use axum::{Extension, Router, extract::State, response::Json as ResponseJson, routing::{get, post}};
use db::models::{pipeline_state::PipelineState, workspace::Workspace};
use deployment::Deployment;
use serde::{Deserialize, Serialize};
use services::services::{pipeline_executor, pipeline_types::PipelineConfig};
use ts_rs::TS;
use utils::response::ApiResponse;
use uuid::Uuid;

use crate::{DeploymentImpl, error::ApiError};

// ── Response type ──────────────────────────────────────────────────

#[derive(Debug, Serialize, TS)]
pub struct PipelineStatusResponse {
    pub current_stage: String,
    pub status: String,
    pub awaiting_approval: bool,
    pub stage_count: usize,
    pub current_stage_index: usize,
}

// ── Request types ──────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct RejectRequest {
    pub feedback: Option<String>,
}

// ── Router ─────────────────────────────────────────────────────────

pub fn router() -> Router<DeploymentImpl> {
    Router::new()
        .route("/status", get(get_pipeline_status))
        .route("/approve", post(approve_pipeline))
        .route("/reject", post(reject_pipeline))
        .route("/pause", post(pause_pipeline))
}

// ── Helpers ────────────────────────────────────────────────────────

fn build_status_response(
    state: &PipelineState,
    config: &PipelineConfig,
) -> PipelineStatusResponse {
    let current_stage_index = config
        .stages
        .iter()
        .position(|s| s.id == state.current_stage_id)
        .unwrap_or(0);

    PipelineStatusResponse {
        current_stage: state.current_stage_id.clone(),
        status: state.status.clone(),
        awaiting_approval: state.awaiting_approval,
        stage_count: config.stages.len(),
        current_stage_index,
    }
}

// ── GET /status ────────────────────────────────────────────────────

pub async fn get_pipeline_status(
    Extension(workspace): Extension<Workspace>,
    State(deployment): State<DeploymentImpl>,
) -> Result<ResponseJson<ApiResponse<PipelineStatusResponse>>, ApiError> {
    let pool = &deployment.db().pool;

    let state = PipelineState::find_by_workspace_id(pool, workspace.id)
        .await?
        .ok_or_else(|| {
            ApiError::BadRequest("No pipeline configured for this workspace".to_string())
        })?;

    let config: PipelineConfig = serde_json::from_str(&state.pipeline_config).map_err(|e| {
        tracing::error!("Failed to parse pipeline config: {}", e);
        ApiError::BadRequest("Invalid pipeline configuration".to_string())
    })?;

    Ok(ResponseJson(ApiResponse::success(build_status_response(
        &state, &config,
    ))))
}

// ── POST /approve ──────────────────────────────────────────────────

pub async fn approve_pipeline(
    Extension(workspace): Extension<Workspace>,
    State(deployment): State<DeploymentImpl>,
) -> Result<ResponseJson<ApiResponse<PipelineStatusResponse>>, ApiError> {
    let pool = &deployment.db().pool;

    let state = PipelineState::find_by_workspace_id(pool, workspace.id)
        .await?
        .ok_or_else(|| {
            ApiError::BadRequest("No pipeline configured for this workspace".to_string())
        })?;

    if !state.awaiting_approval {
        return Err(ApiError::BadRequest(
            "Pipeline is not awaiting approval".to_string(),
        ));
    }

    let config: PipelineConfig = serde_json::from_str(&state.pipeline_config).map_err(|e| {
        tracing::error!("Failed to parse pipeline config: {}", e);
        ApiError::BadRequest("Invalid pipeline configuration".to_string())
    })?;

    // Find the stage that is pending approval.
    let stage_id = state
        .approval_stage_id
        .clone()
        .unwrap_or_else(|| state.current_stage_id.clone());

    let stage_config = config
        .stages
        .iter()
        .find(|s| s.id == stage_id)
        .ok_or_else(|| {
            ApiError::BadRequest(format!("Stage '{}' not found in pipeline config", stage_id))
        })?;

    // Clear the approval flag first.
    PipelineState::clear_approval(pool, workspace.id)
        .await
        .map_err(|e| {
            tracing::error!("Failed to clear approval: {}", e);
            ApiError::BadRequest("Failed to clear approval".to_string())
        })?;

    // Look up existing session for this role.
    let role_sessions: HashMap<String, String> =
        serde_json::from_str(&state.role_sessions).unwrap_or_default();
    let session_id = role_sessions
        .get(&stage_config.role)
        .and_then(|s| Uuid::parse_str(s).ok());
    let is_follow_up = session_id.is_some();

    // Build prompt additions from handoff artifacts.
    let handoff_artifacts: HashMap<String, serde_json::Value> =
        serde_json::from_str(&state.handoff_artifacts).unwrap_or_default();
    let mut prompt_additions = Vec::new();
    if let Some(artifact) = handoff_artifacts.get(&stage_id) {
        if let Some(summary) = artifact.get("review_summary").and_then(|v| v.as_str()) {
            prompt_additions.push(format!("Previous review summary: {}", summary));
        }
    }

    // Start the approved stage.
    let _started = pipeline_executor::start_pipeline_stage(
        deployment.container(),
        pool,
        &workspace,
        &state,
        &stage_id,
        &stage_config.role,
        &stage_config.agent,
        session_id,
        &prompt_additions,
        is_follow_up,
    )
    .await
    .map_err(|e| {
        tracing::error!("Failed to start approved pipeline stage: {}", e);
        ApiError::BadRequest(format!("Failed to start pipeline stage: {}", e))
    })?;

    // Update role_sessions with the new session.
    let mut role_sessions_updated = role_sessions;
    role_sessions_updated.insert(
        stage_config.role.clone(),
        _started.session_id.to_string(),
    );
    let role_sessions_json = serde_json::to_string(&role_sessions_updated).unwrap_or_default();

    // Update the pipeline state to reflect the new stage is running.
    PipelineState::update_stage(
        pool,
        workspace.id,
        &stage_id,
        "running",
        &state.retry_counts,
        &state.stage_history,
        &state.handoff_artifacts,
        &role_sessions_json,
    )
    .await
    .map_err(|e| {
        tracing::error!("Failed to update pipeline state after approval: {}", e);
        ApiError::BadRequest("Failed to update pipeline state".to_string())
    })?;

    // Re-read the updated state.
    let updated_state = PipelineState::find_by_workspace_id(pool, workspace.id)
        .await?
        .ok_or_else(|| {
            ApiError::BadRequest("Pipeline state not found after update".to_string())
        })?;

    Ok(ResponseJson(ApiResponse::success(build_status_response(
        &updated_state, &config,
    ))))
}

// ── POST /reject ───────────────────────────────────────────────────

pub async fn reject_pipeline(
    Extension(workspace): Extension<Workspace>,
    State(deployment): State<DeploymentImpl>,
    ResponseJson(payload): ResponseJson<RejectRequest>,
) -> Result<ResponseJson<ApiResponse<PipelineStatusResponse>>, ApiError> {
    let pool = &deployment.db().pool;

    let state = PipelineState::find_by_workspace_id(pool, workspace.id)
        .await?
        .ok_or_else(|| {
            ApiError::BadRequest("No pipeline configured for this workspace".to_string())
        })?;

    if !state.awaiting_approval {
        return Err(ApiError::BadRequest(
            "Pipeline is not awaiting approval".to_string(),
        ));
    }

    let config: PipelineConfig = serde_json::from_str(&state.pipeline_config).map_err(|e| {
        tracing::error!("Failed to parse pipeline config: {}", e);
        ApiError::BadRequest("Invalid pipeline configuration".to_string())
    })?;

    // Find the current stage to determine on_fail target.
    let current_stage = config
        .stages
        .iter()
        .find(|s| s.id == state.current_stage_id)
        .ok_or_else(|| {
            ApiError::BadRequest(format!(
                "Current stage '{}' not found in pipeline config",
                state.current_stage_id
            ))
        })?;

    let fail_stage_id = &current_stage.on_fail;
    let fail_stage = config
        .stages
        .iter()
        .find(|s| s.id == *fail_stage_id)
        .ok_or_else(|| {
            ApiError::BadRequest(format!(
                "on_fail stage '{}' not found in pipeline config",
                fail_stage_id
            ))
        })?;

    // Clear approval.
    PipelineState::clear_approval(pool, workspace.id)
        .await
        .map_err(|e| {
            tracing::error!("Failed to clear approval: {}", e);
            ApiError::BadRequest("Failed to clear approval".to_string())
        })?;

    // Build prompt additions including rejection feedback.
    let mut prompt_additions = Vec::new();
    if let Some(ref feedback) = payload.feedback {
        prompt_additions.push(format!("Human reviewer feedback: {}", feedback));
    }

    // Look up existing session for the fail stage's role.
    let role_sessions: HashMap<String, String> =
        serde_json::from_str(&state.role_sessions).unwrap_or_default();
    let session_id = role_sessions
        .get(&fail_stage.role)
        .and_then(|s| Uuid::parse_str(s).ok());
    let is_follow_up = session_id.is_some();

    // Start the on_fail stage.
    let started = pipeline_executor::start_pipeline_stage(
        deployment.container(),
        pool,
        &workspace,
        &state,
        &fail_stage.id,
        &fail_stage.role,
        &fail_stage.agent,
        session_id,
        &prompt_additions,
        is_follow_up,
    )
    .await
    .map_err(|e| {
        tracing::error!("Failed to start rejected pipeline stage: {}", e);
        ApiError::BadRequest(format!("Failed to start pipeline stage: {}", e))
    })?;

    // Update role_sessions.
    let mut role_sessions_updated = role_sessions;
    role_sessions_updated.insert(fail_stage.role.clone(), started.session_id.to_string());
    let role_sessions_json = serde_json::to_string(&role_sessions_updated).unwrap_or_default();

    // Persist updated state.
    PipelineState::update_stage(
        pool,
        workspace.id,
        &fail_stage.id,
        "running",
        &state.retry_counts,
        &state.stage_history,
        &state.handoff_artifacts,
        &role_sessions_json,
    )
    .await
    .map_err(|e| {
        tracing::error!("Failed to update pipeline state after rejection: {}", e);
        ApiError::BadRequest("Failed to update pipeline state".to_string())
    })?;

    let updated_state = PipelineState::find_by_workspace_id(pool, workspace.id)
        .await?
        .ok_or_else(|| {
            ApiError::BadRequest("Pipeline state not found after update".to_string())
        })?;

    Ok(ResponseJson(ApiResponse::success(build_status_response(
        &updated_state, &config,
    ))))
}

// ── POST /pause ────────────────────────────────────────────────────

pub async fn pause_pipeline(
    Extension(workspace): Extension<Workspace>,
    State(deployment): State<DeploymentImpl>,
) -> Result<ResponseJson<ApiResponse<PipelineStatusResponse>>, ApiError> {
    let pool = &deployment.db().pool;

    let state = PipelineState::find_by_workspace_id(pool, workspace.id)
        .await?
        .ok_or_else(|| {
            ApiError::BadRequest("No pipeline configured for this workspace".to_string())
        })?;

    let config: PipelineConfig = serde_json::from_str(&state.pipeline_config).map_err(|e| {
        tracing::error!("Failed to parse pipeline config: {}", e);
        ApiError::BadRequest("Invalid pipeline configuration".to_string())
    })?;

    PipelineState::set_status(pool, workspace.id, "paused")
        .await
        .map_err(|e| {
            tracing::error!("Failed to pause pipeline: {}", e);
            ApiError::BadRequest("Failed to pause pipeline".to_string())
        })?;

    let updated_state = PipelineState::find_by_workspace_id(pool, workspace.id)
        .await?
        .ok_or_else(|| {
            ApiError::BadRequest("Pipeline state not found after update".to_string())
        })?;

    Ok(ResponseJson(ApiResponse::success(build_status_response(
        &updated_state, &config,
    ))))
}
