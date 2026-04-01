use crate::services::pipeline_types::{HandoffArtifact, VerdictIssue};

#[derive(Debug, Clone, Copy)]
pub enum IssuePresentation {
    Inline,
    BulletList,
}

#[derive(Debug, Clone, Copy)]
pub enum PlanPresentation {
    Inline,
    Block,
}

pub struct HandoffPromptLabels<'a> {
    pub plan: &'a str,
    pub summary: &'a str,
}

pub struct HandoffPromptFormat<'a> {
    pub labels: HandoffPromptLabels<'a>,
    pub plan_presentation: PlanPresentation,
    pub issue_presentation: IssuePresentation,
}

pub fn build_handoff_prompt_additions(
    artifact: &HandoffArtifact,
    format: HandoffPromptFormat<'_>,
) -> Vec<String> {
    let mut prompt_additions = Vec::new();

    if let Some(plan) = artifact.final_plan.as_deref() {
        match format.plan_presentation {
            PlanPresentation::Inline => {
                prompt_additions.push(format!("{}: {}", format.labels.plan, plan));
            }
            PlanPresentation::Block => {
                prompt_additions.push(format!("{}:\n{}", format.labels.plan, plan));
            }
        }
    }
    if let Some(summary) = artifact.review_summary.as_deref() {
        prompt_additions.push(format!("{}: {}", format.labels.summary, summary));
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

    append_issue_prompt_additions(
        &mut prompt_additions,
        &artifact.issues,
        format.issue_presentation,
    );

    if let Some(report) = artifact.test_report.as_deref() {
        prompt_additions.push(format!("Test report:\n{}", report));
    }

    prompt_additions
}

fn append_issue_prompt_additions(
    prompt_additions: &mut Vec<String>,
    issues: &[VerdictIssue],
    issue_presentation: IssuePresentation,
) {
    if issues.is_empty() {
        return;
    }

    match issue_presentation {
        IssuePresentation::Inline => {
            for issue in issues {
                let loc = match (&issue.file, issue.line) {
                    (Some(file), Some(line)) => format!(" ({}:{})", file, line),
                    (Some(file), None) => format!(" ({})", file),
                    _ => String::new(),
                };
                prompt_additions.push(format!("Issue{}: {}", loc, issue.description));
            }
        }
        IssuePresentation::BulletList => {
            let formatted: Vec<String> = issues.iter().map(format_bulleted_issue).collect();
            prompt_additions.push(format!("Outstanding issues:\n{}", formatted.join("\n")));
        }
    }
}

fn format_bulleted_issue(issue: &VerdictIssue) -> String {
    match (&issue.file, issue.line) {
        (Some(file), Some(line)) => format!("- {}:{}: {}", file, line, issue.description),
        (Some(file), None) => format!("- {}: {}", file, issue.description),
        _ => format!("- {}", issue.description),
    }
}
