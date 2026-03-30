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
