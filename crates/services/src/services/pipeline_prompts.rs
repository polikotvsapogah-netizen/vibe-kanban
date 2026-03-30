const PLANNER_PROMPT: &str = "You are a planning agent. Create a detailed implementation plan for \
the given task. Break it into clear steps with file paths, functions, and expected changes. \
Output the plan in structured markdown.";

const REVIEWER_CONSENSUS_PROMPT: &str = "You are a plan review agent in consensus mode. You may \
rewrite and improve the plan. Return your improved version along with what you changed and why. \
If the plan is good as-is, approve it.";

const REVIEWER_STRICT_PROMPT: &str = "You are a plan review agent in strict mode. Critique the \
plan but do not rewrite it. List blockers, missing steps, risks, and suggested fixes. If the \
plan is good, approve it.";

const BUILDER_PROMPT: &str = "You are an implementation agent. Write code following the approved \
plan exactly. Implement each step from the plan. Focus on correctness and tests.";

const CODE_REVIEWER_PROMPT: &str = "You are a code review agent. Review all code changes made by \
the builder. Check for bugs, logic errors, deviations from the plan, missing error handling, and \
security issues. List issues with file paths and line numbers.";

const TESTER_PROMPT: &str = "You are a testing agent. Based on the task description, plan, and \
implementation: 1) Create a test plan covering key scenarios, 2) Write the tests, 3) Run the \
tests and report results.";

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
        "require_root_cause" => Some(
            "Identify the root cause of each issue before fixing. Do not apply blind fixes.",
        ),
        "no_done_without_verification" => Some(
            "Do not report success without running verification commands and confirming output.",
        ),
        _ => None,
    }
}

pub fn get_stage_prompt(
    role: &str,
    _workflow_profile: Option<&str>,
    workflow_mode: Option<&str>,
    policies: &[String],
) -> String {
    let base = match role {
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
        _ => PLANNER_PROMPT,
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
        for role in &["planner", "reviewer", "builder", "code_reviewer", "tester"] {
            let prompt = get_stage_prompt(role, None, None, &[]);
            assert!(
                prompt.contains("verdict"),
                "verdict missing for role: {role}"
            );
        }
    }

    #[test]
    fn test_unknown_role_falls_back_to_planner() {
        let prompt = get_stage_prompt("unknown_role", None, None, &[]);
        assert!(prompt.contains("planning agent"));
    }
}
