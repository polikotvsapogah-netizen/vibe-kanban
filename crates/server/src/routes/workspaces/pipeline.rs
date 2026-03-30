use std::collections::HashMap;

use axum::{
    Extension, Router,
    extract::State,
    response::Json as ResponseJson,
    routing::{get, post},
};
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

fn build_status_response(state: &PipelineState, config: &PipelineConfig) -> PipelineStatusResponse {
    // 1-based index for display consistency
    let current_stage_index = config
        .stages
        .iter()
        .position(|s| s.id == state.current_stage_id)
        .map(|i| i + 1)
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

    // Build prompt additions from handoff artifacts — pass ALL fields, not just review_summary.
    let handoff_artifacts: HashMap<String, serde_json::Value> =
        serde_json::from_str(&state.handoff_artifacts).unwrap_or_default();
    let mut prompt_additions = Vec::new();
    if let Some(artifact) = handoff_artifacts.get(&stage_id) {
        if let Some(plan) = artifact.get("final_plan").and_then(|v| v.as_str()) {
            prompt_additions.push(format!("Approved plan:\n{}", plan));
        }
        if let Some(summary) = artifact.get("review_summary").and_then(|v| v.as_str()) {
            prompt_additions.push(format!("Review summary: {}", summary));
        }
        if let Some(constraints) = artifact.get("constraints").and_then(|v| v.as_array()) {
            let items: Vec<&str> = constraints.iter().filter_map(|c| c.as_str()).collect();
            if !items.is_empty() {
                prompt_additions.push(format!("Constraints:\n- {}", items.join("\n- ")));
            }
        }
        if let Some(criteria) = artifact
            .get("acceptance_criteria")
            .and_then(|v| v.as_array())
        {
            let items: Vec<&str> = criteria.iter().filter_map(|c| c.as_str()).collect();
            if !items.is_empty() {
                prompt_additions.push(format!("Acceptance criteria:\n- {}", items.join("\n- ")));
            }
        }
        if let Some(risks) = artifact.get("risks").and_then(|v| v.as_array()) {
            let items: Vec<&str> = risks.iter().filter_map(|c| c.as_str()).collect();
            if !items.is_empty() {
                prompt_additions.push(format!("Known risks:\n- {}", items.join("\n- ")));
            }
        }
        if let Some(issues) = artifact.get("issues").and_then(|v| v.as_array()) {
            if !issues.is_empty() {
                let formatted: Vec<String> = issues
                    .iter()
                    .filter_map(|i| {
                        let desc = i.get("description").and_then(|d| d.as_str())?;
                        let file = i.get("file").and_then(|f| f.as_str()).unwrap_or("");
                        let line = i.get("line").and_then(|l| l.as_u64());
                        if file.is_empty() {
                            Some(format!("- {}", desc))
                        } else if let Some(l) = line {
                            Some(format!("- {}:{}: {}", file, l, desc))
                        } else {
                            Some(format!("- {}: {}", file, desc))
                        }
                    })
                    .collect();
                if !formatted.is_empty() {
                    prompt_additions.push(format!("Outstanding issues:\n{}", formatted.join("\n")));
                }
            }
        }
        if let Some(report) = artifact.get("test_report").and_then(|v| v.as_str()) {
            prompt_additions.push(format!("Test report:\n{}", report));
        }
    }

    // Start the approved stage.
    let _started = match pipeline_executor::start_pipeline_stage(
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
    {
        Ok(started) => started,
        Err(e) => {
            tracing::error!("Failed to start approved pipeline stage: {}", e);
            // Rollback: set pipeline back to paused since clear_approval set it to running
            let _ = PipelineState::set_status(pool, workspace.id, "paused").await;
            return Err(ApiError::BadRequest(format!(
                "Failed to start pipeline stage: {}",
                e
            )));
        }
    };

    // Update role_sessions with the new session.
    let mut role_sessions_updated = role_sessions;
    role_sessions_updated.insert(stage_config.role.clone(), _started.session_id.to_string());
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
        .ok_or_else(|| ApiError::BadRequest("Pipeline state not found after update".to_string()))?;

    Ok(ResponseJson(ApiResponse::success(build_status_response(
        &updated_state,
        &config,
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

    // Use approval_stage_id to find the stage that was about to be approved.
    // The rejection should go to the stage that PRODUCED the output being rejected,
    // which is the stage before approval_stage_id. Find it by looking at which stage
    // has on_success == approval_stage_id, then use that stage's on_fail for routing.
    let approval_stage_id = state
        .approval_stage_id
        .clone()
        .unwrap_or_else(|| state.current_stage_id.clone());

    // Find the producer stage: the one whose on_success points to the approval stage.
    let producer_stage = config
        .stages
        .iter()
        .find(|s| s.on_success == approval_stage_id);

    // Use the producer's on_fail if found, otherwise fall back to the approval stage's on_fail.
    let fail_stage_id = if let Some(producer) = producer_stage {
        &producer.on_fail
    } else {
        let approval_stage = config
            .stages
            .iter()
            .find(|s| s.id == approval_stage_id)
            .ok_or_else(|| {
                ApiError::BadRequest(format!(
                    "Approval stage '{}' not found in pipeline config",
                    approval_stage_id
                ))
            })?;
        &approval_stage.on_fail
    };

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

    // Build prompt additions including rejection feedback AND existing handoff context.
    let mut prompt_additions = Vec::new();
    if let Some(ref feedback) = payload.feedback {
        prompt_additions.push(format!("Human reviewer feedback (rejection): {}", feedback));
    }

    // Include any existing handoff context for the target stage.
    let handoff_artifacts: HashMap<String, serde_json::Value> =
        serde_json::from_str(&state.handoff_artifacts).unwrap_or_default();
    if let Some(artifact) = handoff_artifacts.get(&fail_stage.id) {
        if let Some(plan) = artifact.get("final_plan").and_then(|v| v.as_str()) {
            prompt_additions.push(format!("Previous plan:\n{}", plan));
        }
        if let Some(summary) = artifact.get("review_summary").and_then(|v| v.as_str()) {
            prompt_additions.push(format!("Previous review summary: {}", summary));
        }
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
        .ok_or_else(|| ApiError::BadRequest("Pipeline state not found after update".to_string()))?;

    Ok(ResponseJson(ApiResponse::success(build_status_response(
        &updated_state,
        &config,
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
        .ok_or_else(|| ApiError::BadRequest("Pipeline state not found after update".to_string()))?;

    Ok(ResponseJson(ApiResponse::success(build_status_response(
        &updated_state,
        &config,
    ))))
}
