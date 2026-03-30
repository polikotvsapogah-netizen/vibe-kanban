const PLANNER_PROMPT: &str = "You are a planning agent. Create a detailed implementation plan for \
the given task. Break it into clear steps with file paths, functions, and expected changes. \
Output the plan in structured markdown. \
Include your complete plan in the `revised_plan` field of your verdict JSON.";

const REVIEWER_CONSENSUS_PROMPT: &str = "You are a plan review agent in consensus mode. You may \
rewrite and improve the plan. Return your improved version along with what you changed and why. \
If the plan is good as-is, approve it. Your verdict JSON must include: verdict, summary, \
revised_plan (the full improved plan text if you changed it), what_changed (list of changes), \
why_changed (reasoning for each change), unresolved_issues (remaining concerns), and issues \
(list of {file, line, description} objects for specific code-level concerns). \
Include the final agreed plan in the `revised_plan` field of your verdict JSON.";

const REVIEWER_STRICT_PROMPT: &str = "You are a plan review agent in strict mode. Critique the \
plan but do not rewrite it. List blockers, missing steps, risks, and suggested fixes. If the \
plan is good, approve it. Your verdict JSON must include: verdict, summary, blockers \
(blocking issues that must be fixed), non_blockers (minor issues that should be addressed), \
missing_steps (steps the plan is missing), risks (potential risks), suggested_fixes \
(concrete fix suggestions), and issues (list of {file, line, description} objects).";

const BUILDER_PROMPT: &str = "You are an implementation agent. Write code following the approved \
plan exactly. Implement each step from the plan. Focus on correctness and tests.";

const BUILDER_FIX_PROMPT: &str = "You are an implementation agent in fix mode. You are receiving \
code review feedback. Address each issue raised by the reviewer. Fix bugs, logic errors, and \
missing error handling. Do not deviate from the original plan unless the reviewer specifically \
requests a change. Focus on the specific issues listed.";

const CODE_REVIEWER_PROMPT: &str = "You are a code review agent. Review all code changes made by \
the builder. Check for bugs, logic errors, deviations from the plan, missing error handling, and \
security issues. List issues with file paths and line numbers.";

const TESTER_PROMPT: &str = "You are a testing agent. Based on the task description, plan, and \
implementation: 1) Create a test plan covering key scenarios, 2) Write the tests, 3) Run the \
tests and report results.";

const FINISHER_PROMPT: &str = "You are a finalization agent. Your job is to prepare the code for merge: \
1) Run the full test suite and verify all tests pass, \
2) Run linter/formatter if available, \
3) Review the git log to create a clear PR description, \
4) Summarize: what was implemented, files changed, tests passing. \
If tests fail, report the failures and do NOT approve. \
If everything passes, approve with a summary suitable for a PR description.";

const DEFAULT_PROMPT: &str = "You are a pipeline agent. Complete the assigned task carefully and \
report your results.";

const VERDICT_INSTRUCTION: &str = "\n\nIMPORTANT: End your response with a verdict block:\n\
```json\n\
{\"verdict\": \"approved|needs_changes|failed\", \"summary\": \"brief description\", \"issues\": []}\n\
```";

fn policy_instruction(policy: &str) -> Option<&'static str> {
    match policy {
        "use_subagents" => Some(
            "Use the Agent tool to spawn parallel subagents for independent subtasks where possible.",
        ),
        "require_tests" => Some(
            "You MUST write and run tests before completing. Do not report success without passing tests.",
        ),
        "require_root_cause" => {
            Some("Identify the root cause of each issue before fixing. Do not apply blind fixes.")
        }
        "no_done_without_verification" => Some(
            "Do not report success without running verification commands and confirming output.",
        ),
        _ => None,
    }
}

pub fn get_stage_prompt(
    role: &str,
    workflow_profile: Option<&str>,
    workflow_mode: Option<&str>,
    policies: &[String],
) -> String {
    let base = match workflow_profile.unwrap_or(role) {
        "brainstorming" => match workflow_mode.unwrap_or("consensus") {
            "strict" => REVIEWER_STRICT_PROMPT,
            _ => REVIEWER_CONSENSUS_PROMPT,
        },
        "executing-plans" => BUILDER_PROMPT,
        "requesting-code-review" => CODE_REVIEWER_PROMPT,
        "receiving-code-review" => BUILDER_FIX_PROMPT,
        "verification-before-completion" => TESTER_PROMPT,
        "finishing-a-development-branch" => FINISHER_PROMPT,
        _ => match role {
            "planner" => PLANNER_PROMPT,
            "reviewer" => {
                if workflow_mode == Some("strict") {
                    REVIEWER_STRICT_PROMPT
                } else {
                    REVIEWER_CONSENSUS_PROMPT
                }
            }
            "builder" => BUILDER_PROMPT,
            "code_reviewer" => CODE_REVIEWER_PROMPT,
            "tester" => TESTER_PROMPT,
            "finisher" => FINISHER_PROMPT,
            _ => DEFAULT_PROMPT,
        },
    };

    let mut prompt = base.to_string();

    for policy in policies {
        if let Some(instruction) = policy_instruction(policy.as_str()) {
            prompt.push('\n');
            prompt.push_str(instruction);
        }
    }

    prompt.push_str(VERDICT_INSTRUCTION);
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_planner_role() {
        let prompt = get_stage_prompt("planner", None, None, &[]);
        assert!(prompt.contains("planning agent"));
        assert!(prompt.contains("verdict"));
    }

    #[test]
    fn test_reviewer_consensus() {
        let prompt = get_stage_prompt("reviewer", None, Some("consensus"), &[]);
        assert!(prompt.contains("consensus mode"));
    }

    #[test]
    fn test_reviewer_strict() {
        let prompt = get_stage_prompt("reviewer", None, Some("strict"), &[]);
        assert!(prompt.contains("strict mode"));
    }

    #[test]
    fn test_reviewer_defaults_to_consensus() {
        let prompt = get_stage_prompt("reviewer", None, None, &[]);
        assert!(prompt.contains("consensus mode"));
    }

    #[test]
    fn test_policy_appended() {
        let policies = vec!["require_tests".to_string()];
        let prompt = get_stage_prompt("builder", None, None, &policies);
        assert!(prompt.contains("MUST write and run tests"));
    }

    #[test]
    fn test_verdict_always_appended() {
        for role in &["planner", "reviewer", "builder", "code_reviewer", "tester", "finisher"] {
            let prompt = get_stage_prompt(role, None, None, &[]);
            assert!(
                prompt.contains("verdict"),
                "verdict missing for role: {role}"
            );
        }
    }

    #[test]
    fn test_unknown_role_falls_back_to_default() {
        let prompt = get_stage_prompt("unknown_role", None, None, &[]);
        assert!(prompt.contains("pipeline agent"));
    }

    #[test]
    fn test_workflow_profile_brainstorming() {
        let prompt = get_stage_prompt("builder", Some("brainstorming"), None, &[]);
        assert!(prompt.contains("consensus mode"));
    }

    #[test]
    fn test_workflow_profile_brainstorming_strict() {
        let prompt = get_stage_prompt("builder", Some("brainstorming"), Some("strict"), &[]);
        assert!(prompt.contains("strict mode"));
    }

    #[test]
    fn test_finisher_role() {
        let prompt = get_stage_prompt("finisher", None, None, &[]);
        assert!(prompt.contains("finalization agent"));
        assert!(prompt.contains("verdict"));
    }

    #[test]
    fn test_workflow_profile_finishing() {
        let prompt = get_stage_prompt("builder", Some("finishing-a-development-branch"), None, &[]);
        assert!(prompt.contains("finalization agent"));
    }

    #[test]
    fn test_workflow_profile_receiving_code_review() {
        let prompt = get_stage_prompt("builder", Some("receiving-code-review"), None, &[]);
        assert!(prompt.contains("fix mode"));
    }
}
