use std::collections::HashMap;

use axum::{Json, extract::State, response::Json as ResponseJson};
use db::models::{
    coding_agent_turn::CodingAgentTurn,
    execution_process::{ExecutionProcess, ExecutionProcessStatus},
    merge::MergeStatus,
    pipeline_state::PipelineState,
    pull_request::PullRequest,
    workspace::Workspace,
};
use deployment::Deployment;
use serde::{Deserialize, Serialize};
use services::services::pipeline_types::PipelineConfig;
use ts_rs::TS;
use utils::response::ApiResponse;
use uuid::Uuid;

use crate::{DeploymentImpl, error::ApiError};

/// Request for fetching workspace summaries
#[derive(Debug, Deserialize, Serialize, TS)]
pub struct WorkspaceSummaryRequest {
    pub archived: bool,
}

/// Summary info for a single workspace
#[derive(Debug, Serialize, TS)]
pub struct WorkspaceSummary {
    pub workspace_id: Uuid,
    /// Session ID of the latest execution process
    pub latest_session_id: Option<Uuid>,
    /// Is a tool approval currently pending?
    pub has_pending_approval: bool,
    /// Number of files with changes
    pub files_changed: Option<usize>,
    /// Total lines added across all files
    pub lines_added: Option<usize>,
    /// Total lines removed across all files
    pub lines_removed: Option<usize>,
    /// When the latest execution process completed
    #[ts(optional)]
    pub latest_process_completed_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Status of the latest execution process
    pub latest_process_status: Option<ExecutionProcessStatus>,
    /// Is a dev server currently running?
    pub has_running_dev_server: bool,
    /// Does this workspace have unseen coding agent turns?
    pub has_unseen_turns: bool,
    /// PR status for this workspace (if any PR exists)
    pub pr_status: Option<MergeStatus>,
    /// PR number for this workspace (if any PR exists)
    pub pr_number: Option<i64>,
    /// PR URL for this workspace (if any PR exists)
    pub pr_url: Option<String>,
    /// Current pipeline stage id (e.g. "builder"), None if no pipeline
    #[ts(optional)]
    pub pipeline_stage: Option<String>,
    /// Pipeline status ("running", "paused", "completed", etc.), None if no pipeline
    #[ts(optional)]
    pub pipeline_status: Option<String>,
    /// 1-based index of the current stage within the pipeline
    #[ts(optional)]
    pub pipeline_stage_index: Option<u32>,
    /// Total number of stages in the pipeline
    #[ts(optional)]
    pub pipeline_total_stages: Option<u32>,
    /// Retry attempt in "current/max" format (e.g. "1/3"), None if no retries
    #[ts(optional)]
    pub pipeline_attempt: Option<String>,
    /// True if the pipeline is currently waiting for human approval
    #[ts(optional)]
    pub pipeline_awaiting_approval: Option<bool>,
}

/// Response containing summaries for requested workspaces
#[derive(Debug, Serialize, TS)]
pub struct WorkspaceSummaryResponse {
    pub summaries: Vec<WorkspaceSummary>,
}

#[derive(Debug, Clone, Default, Serialize, TS)]
pub struct DiffStats {
    pub files_changed: usize,
    pub lines_added: usize,
    pub lines_removed: usize,
}

/// Fetch summary information for workspaces filtered by archived status.
/// This endpoint returns data that cannot be efficiently included in the streaming endpoint.
#[axum::debug_handler]
pub async fn get_workspace_summaries(
    State(deployment): State<DeploymentImpl>,
    Json(request): Json<WorkspaceSummaryRequest>,
) -> Result<ResponseJson<ApiResponse<WorkspaceSummaryResponse>>, ApiError> {
    let pool = &deployment.db().pool;
    let archived = request.archived;

    // 1. Fetch all workspaces with the given archived status
    let workspaces: Vec<Workspace> = Workspace::find_all_with_status(pool, Some(archived), None)
        .await?
        .into_iter()
        .map(|ws| ws.workspace)
        .collect();

    if workspaces.is_empty() {
        return Ok(ResponseJson(ApiResponse::success(
            WorkspaceSummaryResponse { summaries: vec![] },
        )));
    }

    // 2. Fetch latest process info for workspaces with this archived status
    let latest_processes = ExecutionProcess::find_latest_for_workspaces(pool, archived).await?;

    // 3. Check which workspaces have running dev servers
    let dev_server_workspaces =
        ExecutionProcess::find_workspaces_with_running_dev_servers(pool, archived).await?;

    // 4. Check pending approvals for running processes
    let running_ep_ids: Vec<_> = latest_processes
        .values()
        .filter(|info| info.status == ExecutionProcessStatus::Running)
        .map(|info| info.execution_process_id)
        .collect();
    let pending_approval_eps = deployment
        .approvals()
        .get_pending_execution_process_ids(&running_ep_ids);

    // 5. Check which workspaces have unseen coding agent turns
    let unseen_workspaces = CodingAgentTurn::find_workspaces_with_unseen(pool, archived).await?;

    // 6. Get PR status for each workspace
    let pr_statuses = PullRequest::get_latest_for_workspaces(pool, archived).await?;

    // 7. Get pipeline states for all workspaces
    let workspace_ids: Vec<Uuid> = workspaces.iter().map(|ws| ws.id).collect();
    let pipeline_states_vec = PipelineState::find_by_workspace_ids(pool, &workspace_ids).await?;
    let pipeline_states: HashMap<Uuid, PipelineState> = pipeline_states_vec
        .into_iter()
        .map(|ps| (ps.workspace_id, ps))
        .collect();

    // 9. Compute diff stats for each workspace (in parallel)
    let diff_futures: Vec<_> = workspaces
        .iter()
        .map(|ws| {
            let workspace = ws.clone();
            let deployment = deployment.clone();
            async move {
                if workspace.container_ref.is_some() {
                    compute_workspace_diff_stats(&deployment, &workspace)
                        .await
                        .map(|stats| (workspace.id, stats))
                } else {
                    None
                }
            }
        })
        .collect();

    let diff_results: Vec<Option<(Uuid, DiffStats)>> =
        futures_util::future::join_all(diff_futures).await;
    let diff_stats: HashMap<Uuid, DiffStats> = diff_results.into_iter().flatten().collect();

    // 10. Assemble response
    let summaries: Vec<WorkspaceSummary> = workspaces
        .iter()
        .map(|ws| {
            let id = ws.id;
            let latest = latest_processes.get(&id);
            let has_pending = latest
                .map(|p| pending_approval_eps.contains(&p.execution_process_id))
                .unwrap_or(false);
            let stats = diff_stats.get(&id);

            // Compute pipeline summary fields from the stored pipeline state.
            let (
                pipeline_stage,
                pipeline_status,
                pipeline_stage_index,
                pipeline_total_stages,
                pipeline_attempt,
                pipeline_awaiting_approval,
            ) = if let Some(ps) = pipeline_states.get(&id) {
                let config: Option<PipelineConfig> =
                    serde_json::from_str(&ps.pipeline_config).ok();

                let (stage_index, total_stages) = if let Some(ref cfg) = config {
                    let total = cfg.stages.len() as u32;
                    let index = cfg
                        .stages
                        .iter()
                        .position(|s| s.id == ps.current_stage_id)
                        .map(|i| i as u32 + 1) // 1-based
                        .unwrap_or(0);
                    (Some(index), Some(total))
                } else {
                    (None, None)
                };

                // Build "current/max" attempt string for the current stage.
                let attempt_str = if let Some(ref cfg) = config {
                    let retry_counts: Option<HashMap<String, u32>> =
                        serde_json::from_str(&ps.retry_counts).ok();
                    if let Some(counts) = retry_counts {
                        let current_retry =
                            counts.get(&ps.current_stage_id).copied().unwrap_or(0);
                        if current_retry > 0 {
                            // Find max_retries for this stage.
                            let max_retries = cfg
                                .stages
                                .iter()
                                .find(|s| s.id == ps.current_stage_id)
                                .and_then(|s| s.max_retries)
                                .unwrap_or(cfg.default_max_retries);
                            Some(format!("{}/{}", current_retry + 1, max_retries + 1))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

                (
                    Some(ps.current_stage_id.clone()),
                    Some(ps.status.clone()),
                    stage_index,
                    total_stages,
                    attempt_str,
                    Some(ps.awaiting_approval),
                )
            } else {
                (None, None, None, None, None, None)
            };

            WorkspaceSummary {
                workspace_id: id,
                latest_session_id: latest.map(|p| p.session_id),
                has_pending_approval: has_pending,
                files_changed: stats.map(|s| s.files_changed),
                lines_added: stats.map(|s| s.lines_added),
                lines_removed: stats.map(|s| s.lines_removed),
                latest_process_completed_at: latest.and_then(|p| p.completed_at),
                latest_process_status: latest.map(|p| p.status.clone()),
                has_running_dev_server: dev_server_workspaces.contains(&id),
                has_unseen_turns: unseen_workspaces.contains(&id),
                pr_status: pr_statuses.get(&id).map(|pr| pr.pr_status.clone()),
                pr_number: pr_statuses.get(&id).map(|pr| pr.pr_number),
                pr_url: pr_statuses.get(&id).map(|pr| pr.pr_url.clone()),
                pipeline_stage,
                pipeline_status,
                pipeline_stage_index,
                pipeline_total_stages,
                pipeline_attempt,
                pipeline_awaiting_approval,
            }
        })
        .collect();

    Ok(ResponseJson(ApiResponse::success(
        WorkspaceSummaryResponse { summaries },
    )))
}

/// Compute diff stats for a workspace.
pub async fn compute_workspace_diff_stats(
    deployment: &DeploymentImpl,
    workspace: &Workspace,
) -> Option<DiffStats> {
    let stats = services::services::diff_stream::compute_diff_stats(
        &deployment.db().pool,
        deployment.git(),
        workspace,
    )
    .await?;

    Some(DiffStats {
        files_changed: stats.files_changed,
        lines_added: stats.lines_added,
        lines_removed: stats.lines_removed,
    })
}
