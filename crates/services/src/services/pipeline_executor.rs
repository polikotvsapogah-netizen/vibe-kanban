use std::{path::PathBuf, str::FromStr};

use anyhow::Result;
use db::models::{
    coding_agent_turn::CodingAgentTurn,
    execution_process::ExecutionProcessRunReason,
    pipeline_state::PipelineState,
    session::{CreateSession, Session},
    workspace::Workspace,
    workspace_repo::WorkspaceRepo,
};
use executors::{
    actions::{
        ExecutorAction, ExecutorActionType,
        coding_agent_follow_up::CodingAgentFollowUpRequest,
        coding_agent_initial::CodingAgentInitialRequest,
        review::{RepoReviewContext, ReviewRequest},
    },
    executors::{BaseCodingAgent, build_review_prompt},
    profile::ExecutorConfig,
};
use sqlx::SqlitePool;
use thiserror::Error;
use uuid::Uuid;

use crate::services::{
    container::ContainerService, pipeline_prompts, pipeline_types::PipelineConfig,
};

// ── Error type ─────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum PipelineExecutorError {
    #[error("Stage '{0}' not found in pipeline config")]
    StageNotFound(String),

    #[error("Unknown agent type: {0}")]
    UnknownAgent(String),

    #[error("Pipeline config parse error: {0}")]
    ConfigParse(#[from] serde_json::Error),

    #[error(transparent)]
    Database(#[from] sqlx::Error),

    #[error(transparent)]
    Session(#[from] db::models::session::SessionError),

    #[error(transparent)]
    Container(#[from] crate::services::container::ContainerError),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

// ── Return type ────────────────────────────────────────────────────

pub struct StartedStage {
    pub execution_process_id: Uuid,
    pub session_id: Uuid,
}

// ── Main function ──────────────────────────────────────────────────

/// Bridge between the pipeline controller's `TransitionAction::StartStage`
/// and an actual executor start.
///
/// 1. Resolves (or creates) a DB session.
/// 2. Builds the prompt from role template + handoff context.
/// 3. Constructs the appropriate `ExecutorAction`.
/// 4. Calls `container.start_execution(...)`.
pub async fn start_pipeline_stage(
    container: &(impl ContainerService + ?Sized + Sync),
    pool: &SqlitePool,
    workspace: &Workspace,
    pipeline_state: &PipelineState,
    stage_id: &str,
    role: &str,
    agent: &str,
    session_id: Option<Uuid>,
    prompt_additions: &[String],
    is_follow_up: bool,
) -> Result<StartedStage, PipelineExecutorError> {
    // ── 1. Get or create session ───────────────────────────────────

    let session = match session_id {
        Some(sid) => {
            // Try to find the existing session; create a new one if missing.
            match Session::find_by_id(pool, sid).await? {
                Some(s) => s,
                None => create_pipeline_session(pool, agent, role, workspace.id).await?,
            }
        }
        None => create_pipeline_session(pool, agent, role, workspace.id).await?,
    };

    // ── 2. Build prompt ────────────────────────────────────────────

    let config: PipelineConfig = serde_json::from_str(&pipeline_state.pipeline_config)?;

    let stage_config = config
        .stages
        .iter()
        .find(|s| s.id == stage_id)
        .ok_or_else(|| PipelineExecutorError::StageNotFound(stage_id.to_string()))?;

    // If this is a follow-up for a builder role (e.g., returning from code review),
    // override workflow_profile to "receiving-code-review" for the fix-loop prompt,
    // and add the "require_root_cause" policy.
    let mut effective_policies = stage_config.policies.clone();
    let effective_profile = if is_follow_up && role == "builder" {
        effective_policies.push("require_root_cause".to_string());
        Some("receiving-code-review")
    } else {
        stage_config.workflow_profile.as_deref()
    };

    let role_prompt = pipeline_prompts::get_stage_prompt(
        role,
        effective_profile,
        stage_config.workflow_mode.as_deref(),
        &effective_policies,
    );

    let stage_prompt = if prompt_additions.is_empty() {
        role_prompt
    } else {
        format!("{}\n\n{}", role_prompt, prompt_additions.join("\n\n"))
    };

    // ── 3. Resolve executor (agent string → BaseCodingAgent) ──────

    let base_agent = parse_agent_name(agent)?;
    let executor_config = ExecutorConfig::new(base_agent);

    // ── 4. Build ExecutorAction ────────────────────────────────────

    let action_type = if role == "code_reviewer" {
        // Code reviewers use ReviewRequest.
        let review_agent_session = if is_follow_up {
            agent_session_id_for_follow_up(pool, session.id).await
        } else {
            None
        };

        // Build review context from workspace repos.
        let review_context = build_pipeline_review_context(container, pool, workspace).await?;
        let prompt = build_pipeline_review_prompt(&stage_prompt, review_context.as_deref());

        ExecutorActionType::ReviewRequest(ReviewRequest {
            executor_config,
            context: review_context,
            prompt,
            session_id: review_agent_session,
            working_dir: None,
        })
    } else if is_follow_up {
        // Try to find the agent's internal session ID for a follow-up.
        match agent_session_id_for_follow_up(pool, session.id).await {
            Some(agent_sid) => {
                ExecutorActionType::CodingAgentFollowUpRequest(CodingAgentFollowUpRequest {
                    prompt: stage_prompt,
                    session_id: agent_sid,
                    reset_to_message_id: None,
                    executor_config,
                    working_dir: None,
                })
            }
            None => {
                // No previous agent session — fall back to initial.
                ExecutorActionType::CodingAgentInitialRequest(CodingAgentInitialRequest {
                    prompt: stage_prompt,
                    executor_config,
                    working_dir: None,
                })
            }
        }
    } else {
        ExecutorActionType::CodingAgentInitialRequest(CodingAgentInitialRequest {
            prompt: stage_prompt,
            executor_config,
            working_dir: None,
        })
    };

    let executor_action = ExecutorAction::new(action_type, None);

    // ── 5. Start execution ─────────────────────────────────────────

    let execution_process = container
        .start_execution(
            workspace,
            &session,
            &executor_action,
            &ExecutionProcessRunReason::CodingAgent,
        )
        .await?;

    Ok(StartedStage {
        execution_process_id: execution_process.id,
        session_id: session.id,
    })
}

// ── Helpers ────────────────────────────────────────────────────────

async fn create_pipeline_session(
    pool: &SqlitePool,
    agent: &str,
    role: &str,
    workspace_id: Uuid,
) -> Result<Session, PipelineExecutorError> {
    let create = CreateSession {
        executor: Some(agent.to_string()),
        name: Some(format!("pipeline-{}", role)),
    };
    let session = Session::create(pool, &create, Uuid::new_v4(), workspace_id).await?;
    Ok(session)
}

/// Map a human-readable agent name (e.g. "claude_code", "CLAUDE_CODE")
/// to `BaseCodingAgent`.
fn parse_agent_name(agent: &str) -> Result<BaseCodingAgent, PipelineExecutorError> {
    // Normalise to SCREAMING_SNAKE_CASE for strum parsing.
    let normalised = agent.to_ascii_uppercase();
    BaseCodingAgent::from_str(&normalised)
        .map_err(|_| PipelineExecutorError::UnknownAgent(agent.to_string()))
}

fn build_pipeline_review_prompt(
    additional_prompt: &str,
    context: Option<&[RepoReviewContext]>,
) -> String {
    build_review_prompt(context, Some(additional_prompt))
}

async fn build_pipeline_review_context(
    container: &(impl ContainerService + ?Sized + Sync),
    pool: &SqlitePool,
    workspace: &Workspace,
) -> Result<Option<Vec<RepoReviewContext>>, PipelineExecutorError> {
    let repos =
        WorkspaceRepo::find_repos_with_target_branch_for_workspace(pool, workspace.id).await?;
    if repos.is_empty() {
        return Ok(None);
    }

    let container_ref = container.ensure_container_exists(workspace).await?;
    let workspace_path = PathBuf::from(container_ref.as_str());

    let mut contexts = Vec::new();
    for repo in repos {
        let worktree_path = workspace_path.join(&repo.repo.name);
        if let Ok(base_commit) =
            container
                .git()
                .get_fork_point(&worktree_path, &repo.target_branch, &workspace.branch)
        {
            contexts.push(RepoReviewContext {
                repo_id: repo.repo.id,
                repo_name: repo.repo.display_name,
                base_commit,
            });
        }
    }

    if contexts.is_empty() {
        Ok(None)
    } else {
        Ok(Some(contexts))
    }
}

/// Look up the agent-internal session ID from the most recent
/// `CodingAgentTurn` for this DB session, so we can build a follow-up request.
async fn agent_session_id_for_follow_up(pool: &SqlitePool, session_id: Uuid) -> Option<String> {
    CodingAgentTurn::find_latest_session_info(pool, session_id)
        .await
        .ok()
        .flatten()
        .map(|info| info.session_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_pipeline_review_prompt_includes_repo_context() {
        let context = vec![RepoReviewContext {
            repo_id: Uuid::new_v4(),
            repo_name: "repo-a".to_string(),
            base_commit: "abc123".to_string(),
        }];

        let prompt = build_pipeline_review_prompt(
            "Review all code changes made by the builder.",
            Some(&context),
        );

        assert!(prompt.contains("Repository: repo-a"));
        assert!(prompt.contains("base commit abc123"));
        assert!(prompt.contains("git diff abc123..HEAD"));
        assert!(prompt.contains("Review all code changes made by the builder."));
    }
}
