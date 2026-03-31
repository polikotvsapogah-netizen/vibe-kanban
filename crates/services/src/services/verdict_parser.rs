use crate::services::pipeline_types::Verdict;

/// Parse a `Verdict` from an agent summary.
///
/// Tries, in order:
/// 1. JSON fenced code blocks (` ```json … ``` `)
/// 2. Raw JSON objects that contain a `"verdict"` key
pub fn parse_verdict(summary: &str) -> Option<Verdict> {
    extract_verdict_json(summary).and_then(try_parse_verdict_json)
}

/// Extract the parseable verdict JSON payload from an agent summary.
pub fn extract_verdict_json(summary: &str) -> Option<&str> {
    // 1. Try fenced code blocks first — most reliable signal.
    // Search from the end because the pipeline contract requires the verdict block
    // to be the final structured JSON in the response.
    for block in extract_json_code_blocks(summary).into_iter().rev() {
        if try_parse_verdict_json(block).is_some() {
            return Some(block.trim());
        }
    }

    // 2. Fall back to scanning for a raw JSON object.
    try_find_raw_verdict_json_slice(summary)
}

/// Return slices of text that appear inside ` ```json … ``` ` fences.
fn extract_json_code_blocks(text: &str) -> Vec<&str> {
    let mut blocks = Vec::new();
    let fence_open = "```json";
    let fence_close = "```";

    let mut search_from = 0;
    while let Some(start) = text[search_from..].find(fence_open) {
        let abs_start = search_from + start + fence_open.len();
        // The content begins after the fence tag; skip an optional newline.
        let content_start = if text[abs_start..].starts_with('\n') {
            abs_start + 1
        } else {
            abs_start
        };

        if let Some(end_offset) = text[content_start..].find(fence_close) {
            let content_end = content_start + end_offset;
            blocks.push(text[content_start..content_end].trim());
            search_from = content_end + fence_close.len();
        } else {
            break;
        }
    }
    blocks
}

/// Try to deserialise `json_str` as a `Verdict`.
fn try_parse_verdict_json(json_str: &str) -> Option<Verdict> {
    serde_json::from_str(json_str.trim()).ok()
}

/// Scan `text` for a `{` that begins a JSON object containing `"verdict"`,
/// then try to parse the balanced object.
fn try_find_raw_verdict_json_slice(text: &str) -> Option<&str> {
    for (abs_brace, _) in text.rmatch_indices('{') {
        let candidate = &text[abs_brace..];

        // Quick check: the object must contain the key "verdict".
        if candidate.contains("\"verdict\"") {
            if let Some(end) = find_matching_brace(candidate) {
                let json_candidate = &candidate[..=end];
                if try_parse_verdict_json(json_candidate).is_some() {
                    return Some(json_candidate.trim());
                }
            }
        }
    }
    None
}

/// Find the index of the `}` that closes the first `{` in `s`.
/// Returns `None` if the braces are unbalanced or there is no opening brace.
fn find_matching_brace(s: &str) -> Option<usize> {
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escape_next = false;

    for (i, ch) in s.char_indices() {
        if escape_next {
            escape_next = false;
            continue;
        }
        match ch {
            '\\' if in_string => escape_next = true,
            '"' => in_string = !in_string,
            '{' if !in_string => depth += 1,
            '}' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::pipeline_types::VerdictStatus;

    #[test]
    fn test_parse_verdict_from_code_block() {
        let summary = r#"
Some review text here.

```json
{
  "verdict": "approved",
  "summary": "All checks pass."
}
```
"#;
        let v = parse_verdict(summary).expect("should parse verdict");
        assert_eq!(v.verdict, VerdictStatus::Approved);
        assert_eq!(v.summary, "All checks pass.");
    }

    #[test]
    fn test_parse_verdict_needs_changes() {
        let summary = r#"
Review complete.

```json
{
  "verdict": "needs_changes",
  "summary": "Found issues.",
  "issues": [
    {"description": "Missing error handling", "file": "src/main.rs", "line": 42}
  ]
}
```
"#;
        let v = parse_verdict(summary).expect("should parse verdict");
        assert_eq!(v.verdict, VerdictStatus::NeedsChanges);
        assert_eq!(v.issues.len(), 1);
        assert_eq!(v.issues[0].description, "Missing error handling");
        assert_eq!(v.issues[0].file.as_deref(), Some("src/main.rs"));
        assert_eq!(v.issues[0].line, Some(42));
    }

    #[test]
    fn test_parse_verdict_from_raw_json() {
        let summary = r#"Here is my verdict: {"verdict": "approved", "summary": "LGTM"}"#;
        let v = parse_verdict(summary).expect("should parse raw JSON verdict");
        assert_eq!(v.verdict, VerdictStatus::Approved);
        assert_eq!(v.summary, "LGTM");
    }

    #[test]
    fn test_parse_verdict_not_found() {
        let summary = "No structured verdict here, just plain text.";
        assert!(parse_verdict(summary).is_none());
    }

    #[test]
    fn test_parse_verdict_truncated_summary() {
        // Build a summary that is well over 1024 bytes, with the verdict only at the end.
        let padding = "x".repeat(5000);
        let verdict_block = r#"
```json
{
  "verdict": "approved",
  "summary": "Passed after long review."
}
```"#;
        let summary = format!("{}{}", padding, verdict_block);
        let v = parse_verdict(&summary).expect("should find verdict in tail");
        assert_eq!(v.verdict, VerdictStatus::Approved);
    }

    #[test]
    fn test_parse_verdict_with_large_revised_plan_block() {
        let revised_plan = "Step 1: investigate.\n".repeat(180);
        let summary = format!(
            "Planner notes before verdict.\n```json\n{{\n  \"verdict\": \"approved\",\n  \"summary\": \"Plan prepared.\",\n  \"issues\": [],\n  \"revised_plan\": {revised_plan:?}\n}}\n```"
        );

        let verdict = parse_verdict(&summary).expect("should parse large verdict block");
        assert_eq!(verdict.verdict, VerdictStatus::Approved);
        assert_eq!(verdict.summary, "Plan prepared.");
        assert_eq!(verdict.revised_plan.as_deref(), Some(revised_plan.as_str()));
    }

    #[test]
    fn test_parse_verdict_with_consensus_fields() {
        let summary = r#"
```json
{
  "verdict": "needs_changes",
  "summary": "Plan revised.",
  "revised_plan": "Use async everywhere.",
  "what_changed": ["Switched to async", "Added retries"],
  "why_changed": ["Performance", "Reliability"]
}
```
"#;
        let v = parse_verdict(summary).expect("should parse consensus fields");
        assert_eq!(v.verdict, VerdictStatus::NeedsChanges);
        assert_eq!(v.revised_plan.as_deref(), Some("Use async everywhere."));
        assert_eq!(v.what_changed, vec!["Switched to async", "Added retries"]);
        assert_eq!(v.why_changed, vec!["Performance", "Reliability"]);
    }

    #[test]
    fn test_parse_verdict_with_unicode() {
        let summary = format!(
            "Проверка плана завершена. Всё выглядит хорошо. {} ```json\n{{\"verdict\": \"approved\", \"summary\": \"Всё ок\", \"issues\": []}}\n```",
            "🎯".repeat(500)
        );
        let verdict = parse_verdict(&summary).unwrap();
        assert_eq!(verdict.verdict, VerdictStatus::Approved);
    }
}
