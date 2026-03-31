use std::collections::HashMap;

use anyhow::{Context, Result};
use db::models::{
    coding_agent_turn::CodingAgentTurn, execution_process::ExecutionContext,
    pipeline_state::PipelineState,
};
use sqlx::SqlitePool;
use tracing;
use uuid::Uuid;

use crate::services::{
    pipeline_handoff::{
        HandoffPromptFormat, HandoffPromptLabels, IssuePresentation, PlanPresentation,
        build_handoff_prompt_additions,
    },
    pipeline_types::{
        HandoffArtifact, PipelineConfig, PipelineStatus, StageConfig, StageHistoryEntry, Verdict,
        VerdictStatus,
    },
    verdict_parser::parse_verdict,
};

// ── TransitionAction ────────────────────────────────────────────────

/// What the caller should do after the controller processes a stage completion.
#[derive(Debug, Clone)]
pub enum TransitionAction {
    /// Launch the next (or retry) stage.
    StartStage {
        stage_id: String,
        role: String,
        agent: String,
        session_id: Option<Uuid>,
        prompt_additions: Vec<String>,
        is_follow_up: bool,
    },
    /// The next stage requires human approval before starting.
    AwaitApproval { stage_id: String },
    /// The entire pipeline finished successfully.
    Completed,
    /// Pipeline completed and a PR should be created automatically.
    ReadyForPr,
    /// The pipeline is paused (max retries, failure, etc.).
    Paused { reason: String },
    /// No pipeline is configured for this workspace.
    NoPipeline,
}

// ── PipelineController ──────────────────────────────────────────────

pub struct PipelineController;

impl PipelineController {
    /// Main entry point: called when a stage's execution process finishes.
    ///
    /// Reads the agent's verdict, updates the pipeline state in the DB,
    /// and returns a `TransitionAction` telling the caller what to do next.
    pub async fn handle_stage_completed(
        pool: &SqlitePool,
        ctx: &ExecutionContext,
    ) -> Result<TransitionAction> {
        // 1. Load pipeline state for this workspace.
        let workspace_id = ctx.workspace.id;
        let state = PipelineState::find_by_workspace_id(pool, workspace_id)
            .await
            .context("Failed to query pipeline state")?;

        let state = match state {
            Some(s) => s,
            None => return Ok(TransitionAction::NoPipeline),
        };

        // 2. Deserialise stored config & mutable maps.
        let config: PipelineConfig =
            serde_json::from_str(&state.pipeline_config).context("Bad pipeline_config JSON")?;
        let mut retry_counts: HashMap<String, u32> =
            serde_json::from_str(&state.retry_counts).context("Bad retry_counts JSON")?;
        let mut stage_history: Vec<StageHistoryEntry> =
            serde_json::from_str(&state.stage_history).context("Bad stage_history JSON")?;
        let mut handoff_artifacts: HashMap<String, HandoffArtifact> =
            serde_json::from_str(&state.handoff_artifacts).context("Bad handoff_artifacts JSON")?;
        let mut role_sessions: HashMap<String, String> =
            serde_json::from_str(&state.role_sessions).context("Bad role_sessions JSON")?;

        // 2b. Dedup check: skip if this execution was already processed.
        let already_processed = stage_history.iter().any(|e| {
            e.execution_process_id == ctx.execution_process.id.to_string()
                && e.completed_at.is_some()
        });
        if already_processed {
            tracing::warn!(
                "Duplicate callback for execution {}, skipping",
                ctx.execution_process.id
            );
            return Ok(TransitionAction::NoPipeline);
        }

        // 3. Identify current stage config.
        let current_stage = config
            .stages
            .iter()
            .find(|s| s.id == state.current_stage_id)
            .context("Current stage not found in config")?;

        // 4. Extract verdict from the coding-agent turn summary.
        let verdict = Self::extract_verdict(pool, ctx).await;

        // 5. Update stage history.
        Self::update_stage_history(
            &mut stage_history,
            &state.current_stage_id,
            ctx,
            verdict.as_ref(),
        );

        // 6. Determine transition.
        let action = Self::determine_transition(
            &config,
            current_stage,
            verdict.as_ref(),
            &mut retry_counts,
            &mut handoff_artifacts,
            &mut role_sessions,
            ctx.session.id,
            &stage_history,
        );

        // 7. Persist state changes.
        let (next_stage_id, next_status) = match &action {
            TransitionAction::StartStage { stage_id, .. } => {
                (stage_id.clone(), PipelineStatus::Running)
            }
            TransitionAction::AwaitApproval { stage_id } => {
                (stage_id.clone(), PipelineStatus::Paused)
            }
            TransitionAction::Completed => {
                (state.current_stage_id.clone(), PipelineStatus::Completed)
            }
            TransitionAction::ReadyForPr => {
                (state.current_stage_id.clone(), PipelineStatus::ReadyForPr)
            }
            TransitionAction::Paused { .. } => {
                (state.current_stage_id.clone(), PipelineStatus::Paused)
            }
            TransitionAction::NoPipeline => {
                // Nothing to persist.
                return Ok(action);
            }
        };

        let next_status = next_status.to_string();

        PipelineState::update_stage(
            pool,
            workspace_id,
            &next_stage_id,
            &next_status,
            &serde_json::to_string(&retry_counts)?,
            &serde_json::to_string(&stage_history)?,
            &serde_json::to_string(&handoff_artifacts)?,
            &serde_json::to_string(&role_sessions)?,
        )
        .await
        .context("Failed to update pipeline state")?;

        // If we need approval, also set the approval flag.
        if let TransitionAction::AwaitApproval { ref stage_id } = action {
            let payload = serde_json::json!({ "stage_id": stage_id }).to_string();
            PipelineState::set_approval(pool, workspace_id, stage_id, &payload)
                .await
                .context("Failed to set approval flag")?;
        }

        Ok(action)
    }

    // ── Helpers ─────────────────────────────────────────────────────

    /// Try to read the verdict from the CodingAgentTurn summary that
    /// corresponds to this execution process.
    async fn extract_verdict(pool: &SqlitePool, ctx: &ExecutionContext) -> Option<Verdict> {
        let turn = CodingAgentTurn::find_by_execution_process_id(pool, ctx.execution_process.id)
            .await
            .ok()
            .flatten();

        let summary = turn.and_then(|t| t.summary)?;
        let verdict = parse_verdict(&summary);
        if verdict.is_none() {
            tracing::warn!(
                execution_process_id = %ctx.execution_process.id,
                "No verdict found in agent summary"
            );
        }
        verdict
    }

    /// Append an entry to the stage history.
    fn update_stage_history(
        history: &mut Vec<StageHistoryEntry>,
        stage_id: &str,
        ctx: &ExecutionContext,
        verdict: Option<&Verdict>,
    ) {
        let result = match verdict {
            Some(v) => match v.verdict {
                VerdictStatus::Approved => "approved".to_string(),
                VerdictStatus::NeedsChanges => "needs_changes".to_string(),
                VerdictStatus::Failed => "failed".to_string(),
            },
            None => "no_verdict".to_string(),
        };

        history.push(StageHistoryEntry {
            stage_id: stage_id.to_string(),
            execution_process_id: ctx.execution_process.id.to_string(),
            result,
            verdict: verdict.cloned(),
            started_at: ctx.execution_process.started_at.to_rfc3339(),
            completed_at: ctx.execution_process.completed_at.map(|t| t.to_rfc3339()),
        });
    }

    /// Pure, deterministic transition logic — no I/O.
    ///
    /// Given the pipeline config, the current stage, the verdict, and the
    /// mutable retry / handoff / role-session maps, determine what to do next.
    ///
    /// `retry_counts` and `role_sessions` may be mutated (incremented /
    /// recorded) as a side-effect to keep the bookkeeping in one place.
    pub fn determine_transition(
        config: &PipelineConfig,
        current_stage: &StageConfig,
        verdict: Option<&Verdict>,
        retry_counts: &mut HashMap<String, u32>,
        handoff_artifacts: &mut HashMap<String, HandoffArtifact>,
        role_sessions: &mut HashMap<String, String>,
        session_id: Uuid,
        stage_history: &[StageHistoryEntry],
    ) -> TransitionAction {
        let verdict = match verdict {
            Some(v) => v,
            None => {
                return TransitionAction::Paused {
                    reason: "No verdict returned by agent".to_string(),
                };
            }
        };

        match verdict.verdict {
            VerdictStatus::Approved => Self::handle_approved(
                config,
                current_stage,
                verdict,
                handoff_artifacts,
                role_sessions,
                session_id,
                stage_history,
            ),
            VerdictStatus::NeedsChanges => Self::handle_needs_changes(
                config,
                current_stage,
                verdict,
                retry_counts,
                handoff_artifacts,
                role_sessions,
                session_id,
                stage_history,
            ),
            VerdictStatus::Failed => TransitionAction::Paused {
                reason: format!("Stage {} failed: {}", current_stage.id, verdict.summary),
            },
        }
    }

    // ── Private transition helpers ──────────────────────────────────

    fn handle_approved(
        config: &PipelineConfig,
        current_stage: &StageConfig,
        verdict: &Verdict,
        handoff_artifacts: &mut HashMap<String, HandoffArtifact>,
        role_sessions: &mut HashMap<String, String>,
        session_id: Uuid,
        stage_history: &[StageHistoryEntry],
    ) -> TransitionAction {
        // "complete" means the pipeline is done.
        if current_stage.on_success == "complete" {
            if config.auto_create_pr {
                return TransitionAction::ReadyForPr;
            }
            return TransitionAction::Completed;
        }

        // Find the next stage referenced by on_success.
        let next_stage = match config
            .stages
            .iter()
            .find(|s| s.id == current_stage.on_success)
        {
            Some(s) => s,
            None => {
                return TransitionAction::Paused {
                    reason: format!(
                        "on_success stage '{}' not found in config",
                        current_stage.on_success
                    ),
                };
            }
        };

        let incoming = handoff_artifacts.get(&current_stage.id);
        let artifact = Self::build_handoff_artifact(
            &current_stage.id,
            &next_stage.id,
            verdict,
            incoming,
            None,
        );
        handoff_artifacts.insert(next_stage.id.clone(), artifact);

        // Record the session for this role so follow-ups reuse it.
        role_sessions.insert(current_stage.role.clone(), session_id.to_string());

        // Check if the next stage requires human approval (only on first visit).
        if next_stage.approval == "approval" {
            let already_visited = stage_history.iter().any(|e| e.stage_id == next_stage.id);
            if !already_visited {
                return TransitionAction::AwaitApproval {
                    stage_id: next_stage.id.clone(),
                };
            }
        }

        // Look up the session for the target role (not the current stage's role).
        let target_session = role_sessions
            .get(&next_stage.role)
            .and_then(|s| Uuid::parse_str(s).ok());
        let is_follow_up = target_session.is_some();

        TransitionAction::StartStage {
            stage_id: next_stage.id.clone(),
            role: next_stage.role.clone(),
            agent: next_stage.agent.clone(),
            session_id: target_session,
            prompt_additions: Self::build_prompt_additions(handoff_artifacts, &next_stage.id),
            is_follow_up,
        }
    }

    fn handle_needs_changes(
        config: &PipelineConfig,
        current_stage: &StageConfig,
        verdict: &Verdict,
        retry_counts: &mut HashMap<String, u32>,
        handoff_artifacts: &mut HashMap<String, HandoffArtifact>,
        role_sessions: &mut HashMap<String, String>,
        session_id: Uuid,
        _stage_history: &[StageHistoryEntry],
    ) -> TransitionAction {
        // Bug 1 fix: check for "pause" BEFORE any retry counter logic.
        if current_stage.on_fail == "pause" {
            return TransitionAction::Paused {
                reason: format!(
                    "Stage '{}' needs changes: {}",
                    current_stage.id, verdict.summary
                ),
            };
        }

        let max_retries = current_stage
            .max_retries
            .unwrap_or(config.default_max_retries);
        let current_retries = retry_counts.get(&current_stage.id).copied().unwrap_or(0);

        if current_retries < max_retries {
            // Retry: increment counter and go to on_fail stage.
            retry_counts.insert(current_stage.id.clone(), current_retries + 1);

            let fail_stage_id = &current_stage.on_fail;
            let fail_stage = match config.stages.iter().find(|s| &s.id == fail_stage_id) {
                Some(s) => s,
                None => {
                    return TransitionAction::Paused {
                        reason: format!("on_fail stage '{}' not found in config", fail_stage_id),
                    };
                }
            };

            // Build a handoff with the review issues so the retry stage knows
            // what to fix.
            let incoming = handoff_artifacts.get(&current_stage.id);
            let artifact = Self::build_handoff_artifact(
                &current_stage.id,
                &fail_stage.id,
                verdict,
                incoming,
                None,
            );
            handoff_artifacts.insert(fail_stage.id.clone(), artifact);

            role_sessions.insert(current_stage.role.clone(), session_id.to_string());

            // Look up the session for the target role.
            let target_session = role_sessions
                .get(&fail_stage.role)
                .and_then(|s| Uuid::parse_str(s).ok());
            let is_follow_up = target_session.is_some();

            TransitionAction::StartStage {
                stage_id: fail_stage.id.clone(),
                role: fail_stage.role.clone(),
                agent: fail_stage.agent.clone(),
                session_id: target_session,
                prompt_additions: Self::build_prompt_additions(handoff_artifacts, &fail_stage.id),
                is_follow_up,
            }
        } else {
            // Max retries reached — try escalation.
            let escalate_after = current_stage.escalate_after_retries.unwrap_or(max_retries);

            if current_retries >= escalate_after {
                if let Some(ref escalate_agent) = current_stage.escalate_agent {
                    // Escalate to a different agent.
                    let fail_stage_id = &current_stage.on_fail;
                    let fail_stage = match config.stages.iter().find(|s| &s.id == fail_stage_id) {
                        Some(s) => s,
                        None => {
                            return TransitionAction::Paused {
                                reason: format!(
                                    "on_fail stage '{}' not found in config",
                                    fail_stage_id
                                ),
                            };
                        }
                    };

                    let incoming = handoff_artifacts.get(&current_stage.id);
                    let artifact = Self::build_handoff_artifact(
                        &current_stage.id,
                        &fail_stage.id,
                        verdict,
                        incoming,
                        None,
                    );
                    handoff_artifacts.insert(fail_stage.id.clone(), artifact);

                    role_sessions.insert(current_stage.role.clone(), session_id.to_string());

                    return TransitionAction::StartStage {
                        stage_id: fail_stage.id.clone(),
                        role: fail_stage.role.clone(),
                        agent: escalate_agent.clone(),
                        session_id: None, // escalation = fresh session
                        prompt_additions: Self::build_prompt_additions(
                            handoff_artifacts,
                            &fail_stage.id,
                        ),
                        is_follow_up: false, // escalation = fresh session
                    };
                }
            }

            // No escalation available — pause.
            TransitionAction::Paused {
                reason: format!(
                    "Stage '{}' exhausted {} retries",
                    current_stage.id, max_retries
                ),
            }
        }
    }

    /// Build prompt additions from any handoff artifact targeting `stage_id`.
    fn build_prompt_additions(
        handoff_artifacts: &HashMap<String, HandoffArtifact>,
        stage_id: &str,
    ) -> Vec<String> {
        handoff_artifacts
            .get(stage_id)
            .map(|artifact| {
                build_handoff_prompt_additions(
                    artifact,
                    HandoffPromptFormat {
                        labels: HandoffPromptLabels {
                            plan: "Revised plan",
                            summary: "Previous review summary",
                        },
                        plan_presentation: PlanPresentation::Inline,
                        issue_presentation: IssuePresentation::Inline,
                    },
                )
            })
            .unwrap_or_default()
    }

    fn build_handoff_artifact(
        from_stage: &str,
        to_stage: &str,
        verdict: &Verdict,
        incoming: Option<&HandoffArtifact>,
        test_report: Option<String>,
    ) -> HandoffArtifact {
        HandoffArtifact {
            from_stage: from_stage.to_string(),
            to_stage: to_stage.to_string(),
            review_summary: Some(verdict.summary.clone()),
            final_plan: Self::derive_final_plan(verdict, incoming),
            constraints: incoming
                .map(|artifact| artifact.constraints.clone())
                .unwrap_or_default(),
            risks: verdict.risks.clone(),
            acceptance_criteria: incoming
                .map(|artifact| artifact.acceptance_criteria.clone())
                .unwrap_or_default(),
            issues: verdict.issues.clone(),
            test_report,
        }
    }

    fn derive_final_plan(verdict: &Verdict, incoming: Option<&HandoffArtifact>) -> Option<String> {
        verdict
            .revised_plan
            .clone()
            .or_else(|| incoming.and_then(|artifact| artifact.final_plan.clone()))
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::pipeline_types::{StageConfig, VerdictIssue};

    /// Helper: build a minimal pipeline config with the given stages.
    fn make_config(stages: Vec<StageConfig>, auto_create_pr: bool) -> PipelineConfig {
        PipelineConfig {
            name: "test-pipeline".to_string(),
            enable_second_agent: false,
            default_max_retries: 2,
            auto_create_pr,
            stages,
        }
    }

    fn make_stage(id: &str, on_success: &str, on_fail: &str) -> StageConfig {
        StageConfig {
            id: id.to_string(),
            role: format!("{}_role", id),
            agent: "claude".to_string(),
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

    fn make_verdict(status: VerdictStatus) -> Verdict {
        Verdict {
            verdict: status,
            summary: "test summary".to_string(),
            issues: vec![],
            revised_plan: None,
            what_changed: vec![],
            why_changed: vec![],
            unresolved_issues: vec![],
            blockers: vec![],
            non_blockers: vec![],
            missing_steps: vec![],
            risks: vec![],
            suggested_fixes: vec![],
        }
    }

    // 1. approved → goes to next stage (with approval check)
    #[test]
    fn test_approved_goes_to_next_stage_with_approval() {
        let mut review = make_stage("review", "implement", "implement");
        review.on_success = "implement".to_string();

        let mut implement = make_stage("implement", "complete", "implement");
        implement.approval = "approval".to_string();

        let config = make_config(vec![review.clone(), implement.clone()], false);
        let verdict = make_verdict(VerdictStatus::Approved);

        let mut retries = HashMap::new();
        let mut artifacts = HashMap::new();
        let mut sessions = HashMap::new();
        let session_id = Uuid::new_v4();

        let stage_history: Vec<StageHistoryEntry> = vec![];
        let action = PipelineController::determine_transition(
            &config,
            &review,
            Some(&verdict),
            &mut retries,
            &mut artifacts,
            &mut sessions,
            session_id,
            &stage_history,
        );

        match action {
            TransitionAction::AwaitApproval { stage_id } => {
                assert_eq!(stage_id, "implement");
            }
            other => panic!("Expected AwaitApproval, got {:?}", other),
        }
    }

    // 2. needs_changes → retries to on_fail stage
    #[test]
    fn test_needs_changes_retries() {
        let review = make_stage("review", "complete", "implement");
        let implement = make_stage("implement", "review", "implement");
        let config = make_config(vec![review.clone(), implement.clone()], false);
        let verdict = make_verdict(VerdictStatus::NeedsChanges);

        let mut retries = HashMap::new();
        let mut artifacts = HashMap::new();
        let mut sessions = HashMap::new();
        let session_id = Uuid::new_v4();

        let stage_history: Vec<StageHistoryEntry> = vec![];
        let action = PipelineController::determine_transition(
            &config,
            &review,
            Some(&verdict),
            &mut retries,
            &mut artifacts,
            &mut sessions,
            session_id,
            &stage_history,
        );

        match action {
            TransitionAction::StartStage { stage_id, .. } => {
                assert_eq!(stage_id, "implement");
                assert_eq!(retries.get("review"), Some(&1));
            }
            other => panic!("Expected StartStage, got {:?}", other),
        }
    }

    // 3. max retries → pauses
    #[test]
    fn test_max_retries_pauses() {
        let review = make_stage("review", "complete", "implement");
        let implement = make_stage("implement", "review", "implement");
        let config = make_config(vec![review.clone(), implement.clone()], false);
        let verdict = make_verdict(VerdictStatus::NeedsChanges);

        let mut retries = HashMap::new();
        retries.insert("review".to_string(), 2); // already at max (default 2)
        let mut artifacts = HashMap::new();
        let mut sessions = HashMap::new();
        let session_id = Uuid::new_v4();

        let stage_history: Vec<StageHistoryEntry> = vec![];
        let action = PipelineController::determine_transition(
            &config,
            &review,
            Some(&verdict),
            &mut retries,
            &mut artifacts,
            &mut sessions,
            session_id,
            &stage_history,
        );

        match action {
            TransitionAction::Paused { reason } => {
                assert!(reason.contains("exhausted"), "reason: {}", reason);
            }
            other => panic!("Expected Paused, got {:?}", other),
        }
    }

    // 4. no verdict → pauses
    #[test]
    fn test_no_verdict_pauses() {
        let review = make_stage("review", "complete", "implement");
        let config = make_config(vec![review.clone()], false);

        let mut retries = HashMap::new();
        let mut artifacts = HashMap::new();
        let mut sessions = HashMap::new();
        let session_id = Uuid::new_v4();

        let stage_history: Vec<StageHistoryEntry> = vec![];
        let action = PipelineController::determine_transition(
            &config,
            &review,
            None,
            &mut retries,
            &mut artifacts,
            &mut sessions,
            session_id,
            &stage_history,
        );

        match action {
            TransitionAction::Paused { reason } => {
                assert!(reason.contains("No verdict"), "reason: {}", reason);
            }
            other => panic!("Expected Paused, got {:?}", other),
        }
    }

    // 5. complete stage → Completed (and ReadyForPr when auto_create_pr)
    #[test]
    fn test_complete_stage_completed() {
        let review = make_stage("review", "complete", "review");
        let config = make_config(vec![review.clone()], false);
        let verdict = make_verdict(VerdictStatus::Approved);

        let mut retries = HashMap::new();
        let mut artifacts = HashMap::new();
        let mut sessions = HashMap::new();
        let session_id = Uuid::new_v4();

        let stage_history: Vec<StageHistoryEntry> = vec![];
        let action = PipelineController::determine_transition(
            &config,
            &review,
            Some(&verdict),
            &mut retries,
            &mut artifacts,
            &mut sessions,
            session_id,
            &stage_history,
        );

        assert!(
            matches!(action, TransitionAction::Completed),
            "Expected Completed, got {:?}",
            action
        );

        // With auto_create_pr → ReadyForPr
        let config_pr = make_config(vec![review.clone()], true);
        let action_pr = PipelineController::determine_transition(
            &config_pr,
            &review,
            Some(&verdict),
            &mut retries,
            &mut artifacts,
            &mut sessions,
            session_id,
            &stage_history,
        );

        assert!(
            matches!(action_pr, TransitionAction::ReadyForPr),
            "Expected ReadyForPr, got {:?}",
            action_pr
        );
    }

    #[test]
    fn test_strict_stage_preserves_incoming_final_plan() {
        let reviewer = make_stage("reviewer", "builder", "planner");
        let builder = make_stage("builder", "complete", "reviewer");
        let config = make_config(vec![reviewer.clone(), builder.clone()], false);
        let verdict = make_verdict(VerdictStatus::Approved);

        let mut retries = HashMap::new();
        let mut sessions = HashMap::new();
        let mut artifacts = HashMap::new();
        artifacts.insert(
            "reviewer".to_string(),
            HandoffArtifact {
                from_stage: "planner".to_string(),
                to_stage: "reviewer".to_string(),
                final_plan: Some("agreed implementation plan".to_string()),
                review_summary: Some("plan ready for review".to_string()),
                constraints: vec!["keep API stable".to_string()],
                risks: vec!["migration risk".to_string()],
                acceptance_criteria: vec!["tests pass".to_string()],
                issues: vec![],
                test_report: None,
            },
        );

        let session_id = Uuid::new_v4();
        let stage_history: Vec<StageHistoryEntry> = vec![];
        let action = PipelineController::determine_transition(
            &config,
            &reviewer,
            Some(&verdict),
            &mut retries,
            &mut artifacts,
            &mut sessions,
            session_id,
            &stage_history,
        );

        assert!(
            matches!(action, TransitionAction::StartStage { .. }),
            "Expected StartStage, got {:?}",
            action
        );

        let builder_artifact = artifacts
            .get("builder")
            .expect("builder handoff should exist");
        assert_eq!(
            builder_artifact.final_plan.as_deref(),
            Some("agreed implementation plan")
        );
        assert_eq!(
            builder_artifact.constraints,
            vec!["keep API stable".to_string()]
        );
        assert_eq!(
            builder_artifact.acceptance_criteria,
            vec!["tests pass".to_string()]
        );
    }

    #[test]
    fn test_build_prompt_additions_include_full_handoff() {
        let mut artifacts = HashMap::new();
        artifacts.insert(
            "builder".to_string(),
            HandoffArtifact {
                from_stage: "reviewer".to_string(),
                to_stage: "builder".to_string(),
                final_plan: Some("implement feature".to_string()),
                review_summary: Some("review passed".to_string()),
                constraints: vec!["keep api stable".to_string()],
                risks: vec!["migration may be slow".to_string()],
                acceptance_criteria: vec!["tests pass".to_string()],
                issues: vec![VerdictIssue {
                    file: Some("src/lib.rs".to_string()),
                    line: Some(42),
                    description: "handle timeout".to_string(),
                }],
                test_report: Some("2 passed, 0 failed".to_string()),
            },
        );

        let additions = PipelineController::build_prompt_additions(&artifacts, "builder");
        let combined = additions.join("\n");

        assert!(combined.contains("Revised plan: implement feature"));
        assert!(combined.contains("Constraints:\n- keep api stable"));
        assert!(combined.contains("Acceptance criteria:\n- tests pass"));
        assert!(combined.contains("Known risks:\n- migration may be slow"));
        assert!(combined.contains("Issue (src/lib.rs:42): handle timeout"));
        assert!(combined.contains("Test report:\n2 passed, 0 failed"));
    }

    #[test]
    fn test_build_handoff_artifact_carries_forward_context_and_verdict() {
        let incoming = HandoffArtifact {
            from_stage: "planner".to_string(),
            to_stage: "reviewer".to_string(),
            final_plan: Some("old plan".to_string()),
            review_summary: Some("old summary".to_string()),
            constraints: vec!["keep api stable".to_string()],
            risks: vec!["old risk".to_string()],
            acceptance_criteria: vec!["tests pass".to_string()],
            issues: vec![],
            test_report: None,
        };
        let verdict = Verdict {
            verdict: VerdictStatus::NeedsChanges,
            summary: "needs better error handling".to_string(),
            issues: vec![VerdictIssue {
                file: Some("src/lib.rs".to_string()),
                line: Some(42),
                description: "missing timeout handling".to_string(),
            }],
            revised_plan: Some("updated plan".to_string()),
            what_changed: vec![],
            why_changed: vec![],
            unresolved_issues: vec![],
            blockers: vec![],
            non_blockers: vec![],
            missing_steps: vec![],
            risks: vec!["migration may be slow".to_string()],
            suggested_fixes: vec![],
        };

        let artifact = PipelineController::build_handoff_artifact(
            "reviewer",
            "builder",
            &verdict,
            Some(&incoming),
            None,
        );

        assert_eq!(artifact.from_stage, "reviewer");
        assert_eq!(artifact.to_stage, "builder");
        assert_eq!(
            artifact.review_summary.as_deref(),
            Some("needs better error handling")
        );
        assert_eq!(artifact.final_plan.as_deref(), Some("updated plan"));
        assert_eq!(artifact.constraints, vec!["keep api stable"]);
        assert_eq!(artifact.acceptance_criteria, vec!["tests pass"]);
        assert_eq!(artifact.risks, vec!["migration may be slow"]);
        assert_eq!(artifact.issues.len(), 1);
        assert_eq!(artifact.issues[0].line, Some(42));
    }
}
