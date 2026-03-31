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
    pipeline_handoff::{
        HandoffPromptFormat, HandoffPromptLabels, IssuePresentation, PlanPresentation,
        build_handoff_prompt_additions,
    },
    pipeline_types::{HandoffArtifact, PipelineConfig, PipelineStatus, StageConfig},
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct ApprovalContext {
    stage_id: String,
    saved_stage_id: String,
    saved_payload: String,
    initial_prompt: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApprovalPayload {
    #[serde(default)]
    stage_id: Option<String>,
    #[serde(default)]
    initial_prompt: Option<String>,
}

struct LoadedPipelineContext {
    state: PipelineState,
    config: PipelineConfig,
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

fn success_status_response<E>(
    state: &PipelineState,
    config: &PipelineConfig,
) -> ResponseJson<ApiResponse<PipelineStatusResponse, E>> {
    ResponseJson(ApiResponse::success(build_status_response(state, config)))
}

async fn load_pipeline_context(
    pool: &SqlitePool,
    workspace_id: Uuid,
) -> Result<LoadedPipelineContext, ApiError> {
    let state = PipelineState::find_by_workspace_id(pool, workspace_id)
        .await?
        .ok_or_else(|| {
            ApiError::BadRequest("No pipeline configured for this workspace".to_string())
        })?;

    let config: PipelineConfig = serde_json::from_str(&state.pipeline_config).map_err(|e| {
        tracing::error!("Failed to parse pipeline config: {}", e);
        ApiError::BadRequest("Invalid pipeline configuration".to_string())
    })?;

    Ok(LoadedPipelineContext { state, config })
}

fn require_awaiting_approval(state: &PipelineState) -> Result<(), ApiError> {
    if state.awaiting_approval {
        Ok(())
    } else {
        Err(ApiError::BadRequest(
            "Pipeline is not awaiting approval".to_string(),
        ))
    }
}

fn build_approval_context(state: &PipelineState) -> ApprovalContext {
    let parsed_payload = state
        .approval_payload
        .as_deref()
        .and_then(|payload| serde_json::from_str::<ApprovalPayload>(payload).ok());
    let stage_id = state
        .approval_stage_id
        .clone()
        .or_else(|| {
            parsed_payload
                .as_ref()
                .and_then(|payload| payload.stage_id.clone())
        })
        .unwrap_or_else(|| state.current_stage_id.clone());

    ApprovalContext {
        saved_stage_id: state
            .approval_stage_id
            .clone()
            .unwrap_or_else(|| stage_id.clone()),
        saved_payload: state
            .approval_payload
            .clone()
            .unwrap_or_else(|| serde_json::json!({ "stage_id": stage_id }).to_string()),
        initial_prompt: parsed_payload
            .and_then(|payload| payload.initial_prompt)
            .map(|prompt| prompt.trim().to_string())
            .filter(|prompt| !prompt.is_empty()),
        stage_id,
    }
}

fn find_stage_config<'a>(
    config: &'a PipelineConfig,
    stage_id: &str,
) -> Result<&'a StageConfig, ApiError> {
    config
        .stages
        .iter()
        .find(|s| s.id == stage_id)
        .ok_or_else(|| {
            ApiError::BadRequest(format!("Stage '{}' not found in pipeline config", stage_id))
        })
}

async fn clear_approval_flag(pool: &SqlitePool, workspace_id: Uuid) -> Result<(), ApiError> {
    PipelineState::clear_approval(pool, workspace_id)
        .await
        .map_err(|e| {
            tracing::error!("Failed to clear approval: {}", e);
            ApiError::BadRequest("Failed to clear approval".to_string())
        })
}

async fn set_pipeline_status(
    pool: &SqlitePool,
    workspace_id: Uuid,
    status: PipelineStatus,
    error_message: &'static str,
) -> Result<(), ApiError> {
    PipelineState::set_status(pool, workspace_id, status.as_str())
        .await
        .map_err(|e| {
            tracing::error!("Failed to set pipeline status '{}': {}", status.as_str(), e);
            ApiError::BadRequest(error_message.to_string())
        })
}

async fn fetch_updated_status_response(
    pool: &SqlitePool,
    workspace_id: Uuid,
    config: &PipelineConfig,
) -> Result<ResponseJson<ApiResponse<PipelineStatusResponse>>, ApiError> {
    let updated_state = PipelineState::find_by_workspace_id(pool, workspace_id)
        .await?
        .ok_or_else(|| ApiError::BadRequest("Pipeline state not found after update".to_string()))?;

    Ok(success_status_response::<PipelineStatusResponse>(
        &updated_state,
        config,
    ))
}

fn resolve_rejection_target(
    config: &PipelineConfig,
    approval_stage_id: &str,
) -> Result<ApprovalRejectionTarget, ApiError> {
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

    let fail_stage_id = &approval_stage.on_fail;

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
    prompt_additions.extend(build_handoff_prompt_additions(
        artifact,
        HandoffPromptFormat {
            labels: HandoffPromptLabels {
                plan: plan_label,
                summary: summary_label,
            },
            plan_presentation: PlanPresentation::Block,
            issue_presentation: IssuePresentation::BulletList,
        },
    ));
}

fn build_stage_prompt_additions(
    feedback: Option<&str>,
    initial_prompt: Option<&str>,
    artifact: Option<&HandoffArtifact>,
    plan_label: &str,
    summary_label: &str,
) -> Vec<String> {
    let mut prompt_additions = Vec::new();
    if let Some(initial_prompt) = initial_prompt.filter(|prompt| !prompt.trim().is_empty()) {
        prompt_additions.push(initial_prompt.trim().to_string());
    }
    if let Some(feedback) = feedback {
        prompt_additions.push(format!("Human reviewer feedback: {feedback}"));
    }
    if let Some(artifact) = artifact {
        append_handoff_prompt_additions(&mut prompt_additions, artifact, plan_label, summary_label);
    }
    prompt_additions
}

fn resolve_stage_session(
    role_sessions: &HashMap<String, String>,
    role: &str,
) -> (Option<Uuid>, bool) {
    let session_id = role_sessions
        .get(role)
        .and_then(|s| Uuid::parse_str(s).ok());
    let is_follow_up = session_id.is_some();
    (session_id, is_follow_up)
}

async fn start_stage_and_persist_state(
    deployment: &DeploymentImpl,
    pool: &SqlitePool,
    workspace: &Workspace,
    state: &PipelineState,
    config: &PipelineConfig,
    stage_id: &str,
    stage_config: &StageConfig,
    prompt_additions: &[String],
    saved_approval_stage_id: &str,
    saved_approval_payload: &str,
) -> Result<ResponseJson<ApiResponse<PipelineStatusResponse>>, ApiError> {
    let role_sessions: HashMap<String, String> =
        serde_json::from_str(&state.role_sessions).unwrap_or_default();
    let (session_id, is_follow_up) = resolve_stage_session(&role_sessions, &stage_config.role);

    let started = match pipeline_executor::start_pipeline_stage(
        deployment.container(),
        pool,
        workspace,
        state,
        stage_id,
        &stage_config.role,
        &stage_config.agent,
        session_id,
        prompt_additions,
        is_follow_up,
    )
    .await
    {
        Ok(started) => started,
        Err(e) => {
            tracing::error!("Failed to start pipeline stage '{}': {}", stage_id, e);
            let _ = PipelineState::restore_approval(
                pool,
                workspace.id,
                saved_approval_stage_id,
                saved_approval_payload,
            )
            .await;
            return Err(ApiError::BadRequest(format!(
                "Failed to start pipeline stage: {}",
                e
            )));
        }
    };

    let mut role_sessions_updated = role_sessions;
    role_sessions_updated.insert(stage_config.role.clone(), started.session_id.to_string());
    let role_sessions_json = serde_json::to_string(&role_sessions_updated).unwrap_or_default();

    if let Err(e) = PipelineState::update_stage(
        pool,
        workspace.id,
        stage_id,
        PipelineStatus::Running.as_str(),
        &state.retry_counts,
        &state.stage_history,
        &state.handoff_artifacts,
        &role_sessions_json,
    )
    .await
    {
        tracing::error!(
            "Failed to update pipeline state for stage '{}': {}",
            stage_id,
            e
        );
        rollback_started_stage(
            deployment.container(),
            pool,
            workspace,
            started.execution_process_id,
            saved_approval_stage_id,
            saved_approval_payload,
        )
        .await;
        return Err(ApiError::BadRequest(
            "Failed to update pipeline state".to_string(),
        ));
    }

    fetch_updated_status_response(pool, workspace.id, config).await
}

// ── GET /status ────────────────────────────────────────────────────

pub async fn get_pipeline_status(
    Extension(workspace): Extension<Workspace>,
    State(deployment): State<DeploymentImpl>,
) -> Result<ResponseJson<ApiResponse<PipelineStatusResponse>>, ApiError> {
    let pool = &deployment.db().pool;
    let context = load_pipeline_context(pool, workspace.id).await?;
    Ok(success_status_response::<PipelineStatusResponse>(
        &context.state,
        &context.config,
    ))
}

// ── POST /approve ──────────────────────────────────────────────────

pub async fn approve_pipeline(
    Extension(workspace): Extension<Workspace>,
    State(deployment): State<DeploymentImpl>,
) -> Result<ResponseJson<ApiResponse<PipelineStatusResponse>>, ApiError> {
    let pool = &deployment.db().pool;
    let LoadedPipelineContext { state, config } = load_pipeline_context(pool, workspace.id).await?;
    require_awaiting_approval(&state)?;
    let approval = build_approval_context(&state);
    let stage_config = find_stage_config(&config, &approval.stage_id)?;

    clear_approval_flag(pool, workspace.id).await?;

    let handoff_artifacts: HashMap<String, HandoffArtifact> =
        serde_json::from_str(&state.handoff_artifacts).unwrap_or_default();
    let prompt_additions = build_stage_prompt_additions(
        None,
        approval.initial_prompt.as_deref(),
        handoff_artifacts.get(&approval.stage_id),
        "Approved plan",
        "Review summary",
    );

    start_stage_and_persist_state(
        &deployment,
        pool,
        &workspace,
        &state,
        &config,
        &approval.stage_id,
        stage_config,
        &prompt_additions,
        &approval.saved_stage_id,
        &approval.saved_payload,
    )
    .await
}

// ── POST /reject ───────────────────────────────────────────────────

pub async fn reject_pipeline(
    Extension(workspace): Extension<Workspace>,
    State(deployment): State<DeploymentImpl>,
    ResponseJson(payload): ResponseJson<RejectRequest>,
) -> Result<ResponseJson<ApiResponse<PipelineStatusResponse>>, ApiError> {
    let pool = &deployment.db().pool;
    let LoadedPipelineContext { state, config } = load_pipeline_context(pool, workspace.id).await?;
    require_awaiting_approval(&state)?;
    let approval = build_approval_context(&state);
    let rejection_target = resolve_rejection_target(&config, &approval.stage_id)?;

    clear_approval_flag(pool, workspace.id).await?;

    if matches!(rejection_target, ApprovalRejectionTarget::Pause) {
        if let Err(err) = set_pipeline_status(
            pool,
            workspace.id,
            PipelineStatus::Paused,
            "Failed to pause pipeline after rejection",
        )
        .await
        {
            let _ = PipelineState::restore_approval(
                pool,
                workspace.id,
                &approval.saved_stage_id,
                &approval.saved_payload,
            )
            .await;
            return Err(err);
        }

        let updated_state = PipelineState::find_by_workspace_id(pool, workspace.id)
            .await?
            .ok_or_else(|| {
                ApiError::BadRequest("Pipeline state not found after update".to_string())
            })?;

        return Ok(success_status_response::<PipelineStatusResponse>(
            &updated_state,
            &config,
        ));
    }

    let ApprovalRejectionTarget::Stage(fail_stage) = rejection_target else {
        unreachable!("pause handled above");
    };

    let handoff_artifacts: HashMap<String, HandoffArtifact> =
        serde_json::from_str(&state.handoff_artifacts).unwrap_or_default();
    let prompt_additions = build_stage_prompt_additions(
        payload.feedback.as_deref(),
        None,
        handoff_artifacts.get(&fail_stage.id),
        "Previous plan",
        "Previous review summary",
    );

    start_stage_and_persist_state(
        &deployment,
        pool,
        &workspace,
        &state,
        &config,
        &fail_stage.id,
        &fail_stage,
        &prompt_additions,
        &approval.saved_stage_id,
        &approval.saved_payload,
    )
    .await
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
    fn test_resolve_rejection_target_uses_approval_stage_on_fail() {
        let planner = make_stage("planner", "reviewer", "pause");
        let mut reviewer = make_stage("reviewer", "builder", "planner");
        reviewer.approval = "approval".to_string();
        let builder = make_stage("builder", "complete", "planner");
        let config = make_config(vec![planner, reviewer, builder]);

        let result = resolve_rejection_target(&config, "reviewer").unwrap();

        match result {
            ApprovalRejectionTarget::Stage(stage) => assert_eq!(stage.id, "planner"),
            ApprovalRejectionTarget::Pause => panic!("expected planner retry target"),
        }
    }

    #[test]
    fn test_resolve_rejection_target_returns_pause_when_approval_stage_fails_to_pause() {
        let planner = make_stage("planner", "reviewer", "planner");
        let mut reviewer = make_stage("reviewer", "builder", "pause");
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

    #[test]
    fn test_build_stage_prompt_additions_prepends_feedback() {
        let artifact = HandoffArtifact {
            from_stage: "reviewer".to_string(),
            to_stage: "builder".to_string(),
            final_plan: Some("implement feature".to_string()),
            review_summary: Some("looks good".to_string()),
            constraints: vec!["keep api stable".to_string()],
            risks: vec![],
            acceptance_criteria: vec![],
            issues: vec![],
            test_report: None,
        };

        let additions = build_stage_prompt_additions(
            Some("please tighten the naming"),
            None,
            Some(&artifact),
            "Approved plan",
            "Review summary",
        );

        assert_eq!(
            additions.first().map(String::as_str),
            Some("Human reviewer feedback: please tighten the naming")
        );
        assert!(
            additions
                .iter()
                .any(|line| line.contains("Approved plan:\nimplement feature"))
        );
        assert!(
            additions
                .iter()
                .any(|line| line.contains("Review summary: looks good"))
        );
    }

    #[test]
    fn test_build_approval_context_prefers_saved_state_values() {
        let state = PipelineState {
            id: Uuid::new_v4(),
            workspace_id: Uuid::new_v4(),
            pipeline_config: "{}".to_string(),
            current_stage_id: "builder".to_string(),
            status: "paused".to_string(),
            retry_counts: "{}".to_string(),
            stage_history: "[]".to_string(),
            handoff_artifacts: "{}".to_string(),
            role_sessions: "{}".to_string(),
            awaiting_approval: true,
            approval_stage_id: Some("reviewer".to_string()),
            approval_payload: Some("{\"stage_id\":\"reviewer\"}".to_string()),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        let context = build_approval_context(&state);

        assert_eq!(context.stage_id, "reviewer");
        assert_eq!(context.saved_stage_id, "reviewer");
        assert_eq!(context.saved_payload, "{\"stage_id\":\"reviewer\"}");
        assert_eq!(context.initial_prompt, None);
    }

    #[test]
    fn test_build_approval_context_extracts_initial_prompt() {
        let state = PipelineState {
            id: Uuid::new_v4(),
            workspace_id: Uuid::new_v4(),
            pipeline_config: "{}".to_string(),
            current_stage_id: "planner".to_string(),
            status: "paused".to_string(),
            retry_counts: "{}".to_string(),
            stage_history: "[]".to_string(),
            handoff_artifacts: "{}".to_string(),
            role_sessions: "{}".to_string(),
            awaiting_approval: true,
            approval_stage_id: Some("planner".to_string()),
            approval_payload: Some(
                "{\"stage_id\":\"planner\",\"initial_prompt\":\"  write regression tests  \"}"
                    .to_string(),
            ),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        let context = build_approval_context(&state);

        assert_eq!(context.stage_id, "planner");
        assert_eq!(
            context.initial_prompt.as_deref(),
            Some("write regression tests")
        );
    }

    #[test]
    fn test_build_stage_prompt_additions_includes_initial_prompt_before_feedback() {
        let additions = build_stage_prompt_additions(
            Some("please tighten the naming"),
            Some("implement workspace summary sync"),
            None,
            "Approved plan",
            "Review summary",
        );

        assert_eq!(
            additions.first().map(String::as_str),
            Some("implement workspace summary sync")
        );
        assert_eq!(
            additions.get(1).map(String::as_str),
            Some("Human reviewer feedback: please tighten the naming")
        );
    }
}

// ── POST /pause ────────────────────────────────────────────────────

pub async fn pause_pipeline(
    Extension(workspace): Extension<Workspace>,
    State(deployment): State<DeploymentImpl>,
) -> Result<ResponseJson<ApiResponse<PipelineStatusResponse, PipelineApiError>>, ApiError> {
    let pool = &deployment.db().pool;
    let LoadedPipelineContext { state, config } = load_pipeline_context(pool, workspace.id).await?;

    if matches!(
        state.status.parse::<PipelineStatus>(),
        Ok(PipelineStatus::Completed)
            | Ok(PipelineStatus::Failed)
            | Ok(PipelineStatus::ReadyForPr)
            | Ok(PipelineStatus::Paused)
    ) {
        return Ok(ResponseJson(ApiResponse::error_with_data(
            PipelineApiError::InvalidState {
                message: "Cannot pause pipeline in current state".to_string(),
            },
        )));
    }

    set_pipeline_status(
        pool,
        workspace.id,
        PipelineStatus::Paused,
        "Failed to pause pipeline",
    )
    .await?;

    let updated_state = PipelineState::find_by_workspace_id(pool, workspace.id)
        .await?
        .ok_or_else(|| ApiError::BadRequest("Pipeline state not found after update".to_string()))?;

    Ok(success_status_response::<PipelineApiError>(
        &updated_state,
        &config,
    ))
}
