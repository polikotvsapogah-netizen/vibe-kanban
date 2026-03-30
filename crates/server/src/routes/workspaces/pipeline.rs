use std::collections::HashMap;

use axum::{
    Extension, Router,
    extract::State,
    response::Json as ResponseJson,
    routing::{get, post},
};
use db::models::{
    execution_process::{ExecutionProcess, ExecutionProcessStatus},
    pipeline_state::PipelineState,
    workspace::Workspace,
};
use deployment::Deployment;
use serde::{Deserialize, Serialize};
use services::services::{
    container::ContainerService,
    pipeline_executor,
    pipeline_types::{HandoffArtifact, PipelineConfig, StageConfig},
};
use sqlx::SqlitePool;
use ts_rs::TS;
use utils::response::ApiResponse;
use uuid::Uuid;

use crate::{DeploymentImpl, error::ApiError};

// ── Error type for pipeline-specific errors ───────────────────────

#[derive(Debug, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(tag = "type", rename_all = "snake_case")]
pub enum PipelineApiError {
    InvalidState { message: String },
}

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

#[derive(Debug, Clone)]
enum ApprovalRejectionTarget {
    Pause,
    Stage(StageConfig),
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

fn resolve_rejection_target(
    config: &PipelineConfig,
    approval_stage_id: &str,
) -> Result<ApprovalRejectionTarget, ApiError> {
    let producer_stage = config
        .stages
        .iter()
        .find(|s| s.on_success == approval_stage_id);

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

    if fail_stage_id == "pause" {
        return Ok(ApprovalRejectionTarget::Pause);
    }

    let fail_stage = config
        .stages
        .iter()
        .find(|s| s.id == *fail_stage_id)
        .cloned()
        .ok_or_else(|| {
            ApiError::BadRequest(format!(
                "on_fail stage '{}' not found in pipeline config",
                fail_stage_id
            ))
        })?;

    Ok(ApprovalRejectionTarget::Stage(fail_stage))
}

async fn rollback_started_stage(
    container: &(impl ContainerService + ?Sized),
    pool: &SqlitePool,
    workspace: &Workspace,
    execution_process_id: Uuid,
    approval_stage_id: &str,
    approval_payload: &str,
) {
    if let Ok(Some(process)) = ExecutionProcess::find_by_id(pool, execution_process_id).await
        && let Err(e) = container
            .stop_execution(&process, ExecutionProcessStatus::Killed)
            .await
    {
        tracing::error!(
            "Failed to stop started pipeline execution {} during rollback: {}",
            execution_process_id,
            e
        );
    }

    if let Err(e) =
        PipelineState::restore_approval(pool, workspace.id, approval_stage_id, approval_payload)
            .await
    {
        tracing::error!(
            "Failed to restore pipeline approval for workspace {} during rollback: {}",
            workspace.id,
            e
        );
    }
}

fn append_handoff_prompt_additions(
    prompt_additions: &mut Vec<String>,
    artifact: &HandoffArtifact,
    plan_label: &str,
    summary_label: &str,
) {
    if let Some(plan) = artifact.final_plan.as_deref() {
        prompt_additions.push(format!("{plan_label}:\n{plan}"));
    }
    if let Some(summary) = artifact.review_summary.as_deref() {
        prompt_additions.push(format!("{summary_label}: {summary}"));
    }
    if !artifact.constraints.is_empty() {
        prompt_additions.push(format!(
            "Constraints:\n- {}",
            artifact.constraints.join("\n- ")
        ));
    }
    if !artifact.acceptance_criteria.is_empty() {
        prompt_additions.push(format!(
            "Acceptance criteria:\n- {}",
            artifact.acceptance_criteria.join("\n- ")
        ));
    }
    if !artifact.risks.is_empty() {
        prompt_additions.push(format!("Known risks:\n- {}", artifact.risks.join("\n- ")));
    }
    if !artifact.issues.is_empty() {
        let formatted: Vec<String> = artifact
            .issues
            .iter()
            .map(|issue| match (&issue.file, issue.line) {
                (Some(file), Some(line)) => format!("- {file}:{line}: {}", issue.description),
                (Some(file), None) => format!("- {file}: {}", issue.description),
                _ => format!("- {}", issue.description),
            })
            .collect();
        prompt_additions.push(format!("Outstanding issues:\n{}", formatted.join("\n")));
    }
    if let Some(report) = artifact.test_report.as_deref() {
        prompt_additions.push(format!("Test report:\n{report}"));
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

    // Save approval info before clearing, so we can restore on failure.
    let saved_approval_stage_id = state
        .approval_stage_id
        .clone()
        .unwrap_or_else(|| stage_id.clone());
    let saved_approval_payload = state
        .approval_payload
        .clone()
        .unwrap_or_else(|| serde_json::json!({ "stage_id": stage_id }).to_string());

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
    let handoff_artifacts: HashMap<String, HandoffArtifact> =
        serde_json::from_str(&state.handoff_artifacts).unwrap_or_default();
    let mut prompt_additions = Vec::new();
    if let Some(artifact) = handoff_artifacts.get(&stage_id) {
        append_handoff_prompt_additions(
            &mut prompt_additions,
            artifact,
            "Approved plan",
            "Review summary",
        );
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
            // Rollback: restore approval state so user can retry.
            let _ = PipelineState::restore_approval(
                pool,
                workspace.id,
                &saved_approval_stage_id,
                &saved_approval_payload,
            )
            .await;
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
    if let Err(e) = PipelineState::update_stage(
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
    {
        tracing::error!("Failed to update pipeline state after approval: {}", e);
        rollback_started_stage(
            deployment.container(),
            pool,
            &workspace,
            _started.execution_process_id,
            &saved_approval_stage_id,
            &saved_approval_payload,
        )
        .await;
        return Err(ApiError::BadRequest(
            "Failed to update pipeline state".to_string(),
        ));
    }

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

    // Save approval info before clearing, so we can restore on failure.
    let saved_approval_stage_id = state
        .approval_stage_id
        .clone()
        .unwrap_or_else(|| approval_stage_id.clone());
    let saved_approval_payload = state
        .approval_payload
        .clone()
        .unwrap_or_else(|| serde_json::json!({ "stage_id": approval_stage_id }).to_string());

    let rejection_target = resolve_rejection_target(&config, &approval_stage_id)?;

    // Clear approval.
    PipelineState::clear_approval(pool, workspace.id)
        .await
        .map_err(|e| {
            tracing::error!("Failed to clear approval: {}", e);
            ApiError::BadRequest("Failed to clear approval".to_string())
        })?;

    if matches!(rejection_target, ApprovalRejectionTarget::Pause) {
        if let Err(e) = PipelineState::set_status(pool, workspace.id, "paused").await {
            tracing::error!("Failed to pause rejected pipeline: {}", e);
            let _ = PipelineState::restore_approval(
                pool,
                workspace.id,
                &saved_approval_stage_id,
                &saved_approval_payload,
            )
            .await;
            return Err(ApiError::BadRequest(
                "Failed to pause pipeline after rejection".to_string(),
            ));
        }

        let updated_state = PipelineState::find_by_workspace_id(pool, workspace.id)
            .await?
            .ok_or_else(|| {
                ApiError::BadRequest("Pipeline state not found after update".to_string())
            })?;

        return Ok(ResponseJson(ApiResponse::success(build_status_response(
            &updated_state,
            &config,
        ))));
    }

    let ApprovalRejectionTarget::Stage(fail_stage) = rejection_target else {
        unreachable!("pause handled above");
    };

    // Build prompt additions including rejection feedback AND existing handoff context.
    let mut prompt_additions = Vec::new();
    if let Some(ref feedback) = payload.feedback {
        prompt_additions.push(format!("Human reviewer feedback (rejection): {}", feedback));
    }

    // Include any existing handoff context for the target stage.
    let handoff_artifacts: HashMap<String, HandoffArtifact> =
        serde_json::from_str(&state.handoff_artifacts).unwrap_or_default();
    if let Some(artifact) = handoff_artifacts.get(&fail_stage.id) {
        append_handoff_prompt_additions(
            &mut prompt_additions,
            artifact,
            "Previous plan",
            "Previous review summary",
        );
    }

    // Look up existing session for the fail stage's role.
    let role_sessions: HashMap<String, String> =
        serde_json::from_str(&state.role_sessions).unwrap_or_default();
    let session_id = role_sessions
        .get(&fail_stage.role)
        .and_then(|s| Uuid::parse_str(s).ok());
    let is_follow_up = session_id.is_some();

    // Start the on_fail stage.
    let started = match pipeline_executor::start_pipeline_stage(
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
    {
        Ok(started) => started,
        Err(e) => {
            tracing::error!("Failed to start rejected pipeline stage: {}", e);
            // Rollback: restore approval state so user can retry.
            let _ = PipelineState::restore_approval(
                pool,
                workspace.id,
                &saved_approval_stage_id,
                &saved_approval_payload,
            )
            .await;
            return Err(ApiError::BadRequest(format!(
                "Failed to start pipeline stage: {}",
                e
            )));
        }
    };

    // Update role_sessions.
    let mut role_sessions_updated = role_sessions;
    role_sessions_updated.insert(fail_stage.role.clone(), started.session_id.to_string());
    let role_sessions_json = serde_json::to_string(&role_sessions_updated).unwrap_or_default();

    // Persist updated state.
    if let Err(e) = PipelineState::update_stage(
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
    {
        tracing::error!("Failed to update pipeline state after rejection: {}", e);
        rollback_started_stage(
            deployment.container(),
            pool,
            &workspace,
            started.execution_process_id,
            &saved_approval_stage_id,
            &saved_approval_payload,
        )
        .await;
        return Err(ApiError::BadRequest(
            "Failed to update pipeline state".to_string(),
        ));
    }

    let updated_state = PipelineState::find_by_workspace_id(pool, workspace.id)
        .await?
        .ok_or_else(|| ApiError::BadRequest("Pipeline state not found after update".to_string()))?;

    Ok(ResponseJson(ApiResponse::success(build_status_response(
        &updated_state,
        &config,
    ))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_stage(id: &str, on_success: &str, on_fail: &str) -> StageConfig {
        StageConfig {
            id: id.to_string(),
            role: id.to_string(),
            agent: "claude_code".to_string(),
            approval: "auto".to_string(),
            on_success: on_success.to_string(),
            on_fail: on_fail.to_string(),
            max_retries: None,
            escalate_agent: None,
            escalate_after_retries: None,
            workflow_profile: None,
            workflow_mode: None,
            policies: vec![],
        }
    }

    fn make_config(stages: Vec<StageConfig>) -> PipelineConfig {
        PipelineConfig {
            name: "test".to_string(),
            enable_second_agent: false,
            default_max_retries: 3,
            auto_create_pr: false,
            stages,
        }
    }

    #[test]
    fn test_resolve_rejection_target_returns_pause() {
        let planner = make_stage("planner", "reviewer", "pause");
        let mut reviewer = make_stage("reviewer", "builder", "planner");
        reviewer.approval = "approval".to_string();
        let builder = make_stage("builder", "complete", "planner");
        let config = make_config(vec![planner, reviewer, builder]);

        let result = resolve_rejection_target(&config, "reviewer").unwrap();

        assert!(matches!(result, ApprovalRejectionTarget::Pause));
    }

    #[test]
    fn test_resolve_rejection_target_returns_stage() {
        let planner = make_stage("planner", "reviewer", "planner");
        let mut reviewer = make_stage("reviewer", "builder", "planner");
        reviewer.approval = "approval".to_string();
        let builder = make_stage("builder", "complete", "planner");
        let config = make_config(vec![planner, reviewer, builder]);

        let result = resolve_rejection_target(&config, "reviewer").unwrap();

        match result {
            ApprovalRejectionTarget::Stage(stage) => {
                assert_eq!(stage.id, "planner");
                assert_eq!(stage.on_success, "reviewer");
                assert_eq!(stage.on_fail, "planner");
            }
            ApprovalRejectionTarget::Pause => panic!("expected stage target"),
        }
    }

    #[test]
    fn test_append_handoff_prompt_additions_includes_all_fields() {
        let artifact = HandoffArtifact {
            from_stage: "reviewer".to_string(),
            to_stage: "builder".to_string(),
            final_plan: Some("implement feature".to_string()),
            review_summary: Some("looks good".to_string()),
            constraints: vec!["keep api stable".to_string()],
            risks: vec!["migration may be slow".to_string()],
            acceptance_criteria: vec!["tests pass".to_string()],
            issues: vec![services::services::pipeline_types::VerdictIssue {
                file: Some("src/lib.rs".to_string()),
                line: Some(10),
                description: "handle timeout".to_string(),
            }],
            test_report: Some("2 passed".to_string()),
        };

        let mut additions = Vec::new();
        append_handoff_prompt_additions(
            &mut additions,
            &artifact,
            "Approved plan",
            "Review summary",
        );

        let combined = additions.join("\n");
        assert!(combined.contains("Approved plan:\nimplement feature"));
        assert!(combined.contains("Review summary: looks good"));
        assert!(combined.contains("Constraints:\n- keep api stable"));
        assert!(combined.contains("Acceptance criteria:\n- tests pass"));
        assert!(combined.contains("Known risks:\n- migration may be slow"));
        assert!(combined.contains("Outstanding issues:\n- src/lib.rs:10: handle timeout"));
        assert!(combined.contains("Test report:\n2 passed"));
    }
}

// ── POST /pause ────────────────────────────────────────────────────

pub async fn pause_pipeline(
    Extension(workspace): Extension<Workspace>,
    State(deployment): State<DeploymentImpl>,
) -> Result<ResponseJson<ApiResponse<PipelineStatusResponse, PipelineApiError>>, ApiError> {
    let pool = &deployment.db().pool;

    let state = PipelineState::find_by_workspace_id(pool, workspace.id)
        .await?
        .ok_or_else(|| {
            ApiError::BadRequest("No pipeline configured for this workspace".to_string())
        })?;

    if state.status == "completed"
        || state.status == "failed"
        || state.status == "ready_for_pr"
        || state.status == "paused"
    {
        return Ok(ResponseJson(ApiResponse::error_with_data(
            PipelineApiError::InvalidState {
                message: "Cannot pause pipeline in current state".to_string(),
            },
        )));
    }

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
