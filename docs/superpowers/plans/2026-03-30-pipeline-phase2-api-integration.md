# Multi-Agent Pipeline — Phase 2: API & Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Connect the Phase 1 pipeline state machine to real execution — start stages, manage sessions per role, inject role prompts, handle approvals via API, and send notifications. After Phase 2, a pipeline can run end-to-end.

**Architecture:** The exit monitor's `TransitionAction::StartStage` now triggers real executor starts. A new `PipelineExecutor` service builds `ExecutorAction` from stage config, creates/reuses sessions per role, injects handoff context into prompts, and calls `container.start_execution()`. Three new API endpoints let users approve/reject/pause pipelines. Notifications use the existing `NotificationService`.

**Tech Stack:** Rust, Axum (routes), SQLx, serde_json, tokio, uuid

**Spec:** `docs/superpowers/specs/2026-03-30-multi-agent-pipeline-design.md`
**Phase 1:** `docs/superpowers/plans/2026-03-30-pipeline-phase1-backend-core.md` (completed)

---

## File Structure

| File | Responsibility |
|------|---------------|
| **Create:** `crates/services/src/services/pipeline_executor.rs` | Builds ExecutorAction from stage config, manages role sessions, injects prompts |
| **Create:** `crates/services/src/services/pipeline_prompts.rs` | Role-specific prompt templates (hardcoded, keyed by workflow_profile) |
| **Create:** `crates/server/src/routes/workspaces/pipeline.rs` | API endpoints: approve, reject, pause, status |
| **Modify:** `crates/server/src/routes/workspaces/mod.rs` | Register pipeline routes |
| **Modify:** `crates/local-deployment/src/container.rs` | Replace TODO logging with real stage execution |
| **Modify:** `crates/services/src/services/mod.rs` | Register new modules |
| **Modify:** `crates/services/src/services/pipeline_controller.rs` | Add role_sessions update after stage start |

---

### Task 1: Pipeline Prompts — Role Templates

**Files:**
- Create: `crates/services/src/services/pipeline_prompts.rs`
- Modify: `crates/services/src/services/mod.rs`

- [ ] **Step 1: Create pipeline_prompts.rs**

This module returns role-specific prompt text based on `workflow_profile` and `workflow_mode`. No external dependencies — just string templates.

```rust
/// Returns the role-specific prompt addition for a pipeline stage.
/// This is appended to the user's task description via executor append_prompt.
pub fn get_stage_prompt(
    role: &str,
    workflow_profile: Option<&str>,
    workflow_mode: Option<&str>,
    policies: &[String],
) -> String {
    let base = match role {
        "planner" => PLANNER_PROMPT,
        "reviewer" => match workflow_mode.unwrap_or("consensus") {
            "strict" => REVIEWER_STRICT_PROMPT,
            _ => REVIEWER_CONSENSUS_PROMPT,
        },
        "builder" => BUILDER_PROMPT,
        "code_reviewer" => CODE_REVIEWER_PROMPT,
        "tester" => TESTER_PROMPT,
        _ => DEFAULT_PROMPT,
    };

    let mut prompt = base.to_string();

    // Append policy instructions
    for policy in policies {
        if let Some(instruction) = get_policy_instruction(policy) {
            prompt.push_str("\n\n");
            prompt.push_str(instruction);
        }
    }

    // Always append verdict instruction
    prompt.push_str(VERDICT_INSTRUCTION);

    prompt
}
```

Each prompt constant should be a complete prompt string. The verdict instruction tells the agent to output the structured JSON verdict at the end.

Include constants:
- `PLANNER_PROMPT` — "You are a planning agent. Create a detailed implementation plan..."
- `REVIEWER_CONSENSUS_PROMPT` — "You are a plan review agent in consensus mode. You may rewrite and improve the plan..."
- `REVIEWER_STRICT_PROMPT` — "You are a plan review agent in strict mode. Critique the plan but do not rewrite it..."
- `BUILDER_PROMPT` — "You are an implementation agent. Write code following the approved plan..."
- `CODE_REVIEWER_PROMPT` — "You are a code review agent. Review all changes..."
- `TESTER_PROMPT` — "You are a testing agent. Plan tests, write them, run them..."
- `DEFAULT_PROMPT` — generic fallback
- `VERDICT_INSTRUCTION` — "End your response with a verdict block: ```json ..."
- Policy instructions for: `use_subagents`, `require_tests`, `require_root_cause`, `no_done_without_verification`

- [ ] **Step 2: Register module**

Add `pub mod pipeline_prompts;` to `crates/services/src/services/mod.rs`.

- [ ] **Step 3: Build**

Run: `cd /Users/aipolikot/myai/Vibe-coding && cargo build --package services 2>&1 | tail -5`

- [ ] **Step 4: Commit**

```bash
cd /Users/aipolikot/myai/Vibe-coding && git add crates/services/src/services/pipeline_prompts.rs crates/services/src/services/mod.rs && git commit -m "feat(pipeline): add role-specific prompt templates keyed by workflow_profile"
```

---

### Task 2: Pipeline Executor — Stage Launcher

**Files:**
- Create: `crates/services/src/services/pipeline_executor.rs`
- Modify: `crates/services/src/services/mod.rs`

This is the core of Phase 2. It takes a `TransitionAction::StartStage` and turns it into a real execution.

- [ ] **Step 1: Read existing patterns**

Read these files to understand exact types and method signatures:
- `crates/server/src/routes/sessions/mod.rs` lines 124-234 (follow_up flow)
- `crates/services/src/services/container.rs` (start_execution signature)
- `crates/executors/src/actions/mod.rs` (ExecutorAction, CodingAgentInitialRequest, CodingAgentFollowUpRequest)
- `crates/services/src/services/pipeline_controller.rs` (TransitionAction fields)
- `crates/db/src/models/session.rs` (Session::create, CreateSession)

- [ ] **Step 2: Create pipeline_executor.rs**

The module should have one main function:

```rust
pub async fn start_pipeline_stage(
    container: &impl ContainerService,
    pool: &SqlitePool,
    workspace: &Workspace,
    stage_config: &StageConfig,
    session_id: Option<String>,       // Some = follow-up existing session, None = create new
    prompt_additions: &[String],      // Handoff context from controller
    task_description: &str,           // Original task description
    is_follow_up: bool,
) -> Result<(ExecutionProcess, Session), PipelineExecutorError>
```

Logic:
1. Get or create session for this role:
   - If `session_id` is Some → load existing session, use follow-up
   - If None → create new session with `Session::create()`
2. Build the prompt: task_description + stage prompt (from pipeline_prompts) + handoff context
3. Build `ExecutorAction`:
   - If follow-up: `CodingAgentFollowUpRequest { prompt, session_id, executor_config, ... }`
   - If new: `CodingAgentInitialRequest { prompt, executor_config, ... }`
   - For code_reviewer role: use `ReviewRequest` instead
4. Call `container.start_execution(&workspace, &session, &action, &ExecutionProcessRunReason::CodingAgent)`
5. Return the execution process and session (so caller can update role_sessions map)

- [ ] **Step 3: Register module**

Add `pub mod pipeline_executor;` to `crates/services/src/services/mod.rs`.

- [ ] **Step 4: Build**

Run: `cd /Users/aipolikot/myai/Vibe-coding && cargo build --package services 2>&1 | tail -10`

- [ ] **Step 5: Commit**

```bash
cd /Users/aipolikot/myai/Vibe-coding && git add crates/services/src/services/pipeline_executor.rs crates/services/src/services/mod.rs && git commit -m "feat(pipeline): add PipelineExecutor — builds and starts stage executions"
```

---

### Task 3: Exit Monitor — Real Stage Execution

**Files:**
- Modify: `crates/local-deployment/src/container.rs`

Replace the TODO logging in the exit monitor with real stage execution calls.

- [ ] **Step 1: Read current exit monitor integration**

Read the pipeline match block we added in Phase 1 (~line 597-640 in container.rs).

- [ ] **Step 2: Replace TODO arms with real execution**

For `TransitionAction::StartStage`:
1. Load pipeline_state to get stage config and handoff artifacts
2. Build the prompt from handoff artifacts
3. Call `pipeline_executor::start_pipeline_stage()`
4. Update `role_sessions` in pipeline_state with the new session_id
5. Update `stage_history` with new in-progress entry

For `TransitionAction::AwaitApproval`:
1. Send notification via `container.notification_service().notify()`

For `TransitionAction::Paused`:
1. Send notification

For `TransitionAction::ReadyForPr`:
1. Send notification
2. Optionally auto-create PR (gated check: single repo + valid auth)

For `TransitionAction::Completed`:
1. Send notification
2. Finalize task

- [ ] **Step 3: Build**

Run: `cd /Users/aipolikot/myai/Vibe-coding && cargo build 2>&1 | tail -10`

- [ ] **Step 4: Commit**

```bash
cd /Users/aipolikot/myai/Vibe-coding && git add crates/local-deployment/src/container.rs && git commit -m "feat(pipeline): connect exit monitor to real stage execution"
```

---

### Task 4: Pipeline API Endpoints

**Files:**
- Create: `crates/server/src/routes/workspaces/pipeline.rs`
- Modify: `crates/server/src/routes/workspaces/mod.rs`

Three endpoints for pipeline management:

- [ ] **Step 1: Read route patterns**

Read `crates/server/src/routes/workspaces/mod.rs` and `crates/server/src/routes/workspaces/execution.rs` for patterns.

- [ ] **Step 2: Create pipeline.rs with three endpoints**

```rust
// POST /api/workspaces/:id/pipeline/approve
// - Clears awaiting_approval flag
// - Starts the approved stage
// - Returns updated pipeline status

// POST /api/workspaces/:id/pipeline/reject
// - Body: { feedback: String }
// - Returns to on_fail stage with feedback
// - Starts the previous stage

// POST /api/workspaces/:id/pipeline/pause
// - Sets pipeline status to paused
// - Does NOT stop running execution (that's a separate stop endpoint)

// GET /api/workspaces/:id/pipeline/status
// - Returns current pipeline state (stage, status, retry counts, history)
```

Each endpoint:
1. Load workspace from extension (middleware handles this)
2. Load pipeline_state by workspace_id
3. Validate state (e.g., approve only works when awaiting_approval=true)
4. Update pipeline_state
5. For approve/reject: start next stage via pipeline_executor

- [ ] **Step 3: Register routes**

In `crates/server/src/routes/workspaces/mod.rs`, add:
```rust
.nest("/pipeline", pipeline::router())
```

- [ ] **Step 4: Build**

Run: `cd /Users/aipolikot/myai/Vibe-coding && cargo build 2>&1 | tail -10`

- [ ] **Step 5: Commit**

```bash
cd /Users/aipolikot/myai/Vibe-coding && git add crates/server/src/routes/workspaces/pipeline.rs crates/server/src/routes/workspaces/mod.rs && git commit -m "feat(pipeline): add approve/reject/pause/status API endpoints"
```

---

### Task 5: Pipeline Creation — Hook into Workspace Start

**Files:**
- Modify: `crates/server/src/routes/sessions/mod.rs` or `crates/server/src/routes/workspaces/create.rs`

When a user creates a workspace with pipeline config, initialize the pipeline_state.

- [ ] **Step 1: Read workspace creation flow**

Read `crates/server/src/routes/workspaces/create.rs` — understand how workspaces are created and how the first execution is started.

- [ ] **Step 2: Add pipeline initialization**

After workspace is created and first execution starts:
1. If request contains pipeline config → create PipelineState record
2. Set first_stage_id from config
3. Inject stage prompt into the first execution's prompt

This may require adding `pipeline_config: Option<PipelineConfig>` to the workspace creation request.

- [ ] **Step 3: Build and test**

Run: `cd /Users/aipolikot/myai/Vibe-coding && cargo build 2>&1 | tail -10`

- [ ] **Step 4: Commit**

```bash
cd /Users/aipolikot/myai/Vibe-coding && git add -A && git commit -m "feat(pipeline): initialize pipeline state on workspace creation with config"
```

---

### Task 6: Workspace Summary — Pipeline Progress Data

**Files:**
- Modify: `crates/server/src/routes/workspaces/workspace_summary.rs`

Add pipeline progress data to workspace summary so the frontend can show it on cards.

- [ ] **Step 1: Read current WorkspaceSummary struct**

Read `crates/server/src/routes/workspaces/workspace_summary.rs`.

- [ ] **Step 2: Add pipeline fields to WorkspaceSummary**

```rust
pub struct WorkspaceSummary {
    // ... existing fields ...
    pub pipeline_stage: Option<String>,      // current stage id
    pub pipeline_status: Option<String>,     // running/paused/completed/etc
    pub pipeline_stage_index: Option<u32>,   // 1-based index (e.g., 3 of 5)
    pub pipeline_total_stages: Option<u32>,  // total stages count
    pub pipeline_attempt: Option<String>,    // "1/3" format
    pub pipeline_awaiting_approval: bool,
}
```

- [ ] **Step 3: Load pipeline_state in the summary query**

After loading workspaces, batch-query pipeline_states for all workspace_ids and merge into summaries.

- [ ] **Step 4: Build**

Run: `cd /Users/aipolikot/myai/Vibe-coding && cargo build 2>&1 | tail -10`

- [ ] **Step 5: Commit**

```bash
cd /Users/aipolikot/myai/Vibe-coding && git add crates/server/src/routes/workspaces/workspace_summary.rs && git commit -m "feat(pipeline): add pipeline progress to workspace summary"
```

---

### Task 7: SQLx Prep + Full Build + Test

**Files:**
- Regenerate SQLx offline data

- [ ] **Step 1: Regenerate SQLx**

Run: `cd /Users/aipolikot/myai/Vibe-coding && pnpm run prepare-db 2>&1 | tail -5`

- [ ] **Step 2: Generate TypeScript types**

Run: `cd /Users/aipolikot/myai/Vibe-coding && pnpm run generate-types 2>&1 | tail -5`

- [ ] **Step 3: Full build**

Run: `cd /Users/aipolikot/myai/Vibe-coding && cargo build 2>&1 | tail -10`

- [ ] **Step 4: Full test suite**

Run: `cd /Users/aipolikot/myai/Vibe-coding && cargo test --workspace 2>&1 | tail -20`

- [ ] **Step 5: Commit**

```bash
cd /Users/aipolikot/myai/Vibe-coding && git add -A && git commit -m "chore: regenerate SQLx data and TypeScript types for pipeline Phase 2"
```

---

## Self-Review Checklist

**Spec coverage (Phase 2):**
- [ ] Stage execution from exit monitor — Task 3
- [ ] Session management per role — Task 2
- [ ] Role-specific prompts with workflow_profile — Task 1
- [ ] Verdict instruction in all prompts — Task 1
- [ ] Approve/reject/pause API — Task 4
- [ ] Pipeline status API — Task 4
- [ ] Pipeline initialization on workspace create — Task 5
- [ ] Workspace summary with pipeline progress — Task 6
- [ ] Notifications for paused/approval/completed — Task 3
- [ ] Code review via existing review route — Task 2
- [ ] Handoff context in prompts — Task 2

**Not in Phase 2 (Phase 3 — Frontend):**
- Pipeline settings UI in task creation form
- Pipeline progress indicator on kanban cards
- Approval buttons in workspace chat
- Tooltip translations
