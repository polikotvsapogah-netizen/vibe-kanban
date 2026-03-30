# Multi-Agent Pipeline for Vibe Kanban

## Overview

Add a multi-agent pipeline feature to Vibe Kanban that chains multiple AI agent stages (Planner, Reviewer, Builder, Code Reviewer, Tester) with configurable agents, approval policies, retry cycles, and escalation to a second agent.

## Pipeline Stages (Default — 5 stages)

```
Planner ↔ Reviewer (up to 7 cycles, stops on first approved)
         │
         ▼ structured handoff
Builder → Code Reviewer → Builder (fix, follow-up) → Tester
              ↑                                        |
              └────────────────────────────────────────┘
```

1. **Planner** — creates a detailed implementation plan from the task description
2. **Reviewer** — critically reviews the plan. Fail → back to Planner. Uses its own session, cycles via follow-up
3. **Builder** — writes code following the approved plan. Also acts as Fixer via follow-up when Code Reviewer finds issues
4. **Code Reviewer** — reviews written code via existing review route. Fail → Builder (follow-up fix)
5. **Tester** — plans tests based on spec and architecture, writes and runs them. Fail → back to Code Reviewer

Fixer is not a separate stage — it is Builder follow-up within the same session.

## Transition Table

| Current Stage | Verdict | Next Stage |
|---|---|---|
| Planner | approved | Reviewer |
| Planner | failed | pause |
| Reviewer | approved | Builder |
| Reviewer | needs_changes | Planner |
| Reviewer | retry exhausted | pause |
| Builder | approved | Code Reviewer |
| Builder | failed | pause |
| Code Reviewer | approved | Tester |
| Code Reviewer | needs_changes | Builder (follow-up fix) |
| Code Reviewer | retry exhausted | escalate / pause |
| Tester | approved | ready_for_pr / complete |
| Tester | needs_changes | Code Reviewer |
| Tester | retry exhausted | escalate / pause |

## Pipeline Config Structure

Pipeline is described as a JSON config stored as a pipeline profile.

```json
{
  "name": "Full Pipeline",
  "enable_second_agent": false,
  "default_max_retries": 7,
  "auto_create_pr": true,
  "stages": [
    {
      "id": "planner",
      "role": "planner",
      "agent": "claude_code",
      "approval": "auto",
      "on_success": "reviewer",
      "on_fail": "pause",
      "max_retries": null
    },
    {
      "id": "reviewer",
      "role": "reviewer",
      "agent": "claude_code",
      "approval": "approval",
      "on_success": "builder",
      "on_fail": "planner",
      "max_retries": 3
    },
    {
      "id": "builder",
      "role": "builder",
      "agent": "claude_code",
      "approval": "auto",
      "on_success": "code_reviewer",
      "on_fail": "pause",
      "max_retries": null
    },
    {
      "id": "code_reviewer",
      "role": "code_reviewer",
      "agent": "claude_code",
      "approval": "auto",
      "on_success": "tester",
      "on_fail": "builder",
      "max_retries": 3
    },
    {
      "id": "tester",
      "role": "tester",
      "agent": "claude_code",
      "approval": "auto",
      "on_success": "complete",
      "on_fail": "code_reviewer",
      "escalate_agent": "codex",
      "escalate_after_retries": 3,
      "max_retries": null
    }
  ]
}
```

### Config Field Reference

| Field | Description |
|-------|-------------|
| `name` | Pipeline profile display name |
| `enable_second_agent` | Enable second AI agent for escalation when primary agent fails after retries |
| `default_max_retries` | Default maximum retry count for all stages with correction cycles (default: 7). Cycle stops on first `approved` verdict — this is an upper bound, not a target |
| `auto_create_pr` | Automatically create a Pull Request after successful pipeline completion (gated: single-repo + valid auth only, otherwise status = `ready_for_pr`) |
| `id` | Unique stage identifier |
| `role` | Stage role — determines the task the agent performs (planning, review, coding, testing) |
| `agent` | Which AI agent executes this stage (claude_code, codex, gemini, etc.) |
| `approval` | Whether user confirmation is required before this stage starts. `auto` — starts automatically, `approval` — waits for user OK |
| `on_success` | Which stage to run after successful completion. `complete` = pipeline finished |
| `on_fail` | What happens on failure. Can point to a previous stage for rework or `pause` for user decision |
| `max_retries` | Max retry count for this specific stage's correction cycle. `null` = use `default_max_retries` |
| `escalate_agent` | Second agent that takes over when primary fails after retries. Receives full context: plan, code, and errors |
| `escalate_after_retries` | How many failed attempts before escalating to the second agent |

## Architecture: PipelineController + Existing Execution Flow

### Why a Separate PipelineController

`spawn_exit_monitor()` is already 328 lines handling 13+ concerns (exit, commit, cleanup, queued follow-ups, finalization, remote sync, analytics). Pipeline transition logic must live in a separate module that exit_monitor calls.

### PipelineController Module

A thin, deterministic state machine. Not an "intelligent" service — just reads verdict, updates state, starts next stage.

```
spawn_exit_monitor() completes execution
         │
         ▼
pipeline_controller.handle_stage_completed(ctx, execution_process_id)
         │
         ▼
Parse verdict from CodingAgentTurn.summary
         │
         ▼
┌─ verdict.approved ────────────────────┐
│ Read on_success of current stage      │
│ Build structured handoff artifact     │
│ Start next stage                      │
└───────────────────────────────────────┘
         │
┌─ verdict.needs_changes ───────────────────────┐
│ retry_count < max_retries?                     │
│  ├─ YES → on_fail stage, retry_count++        │
│  │        pass feedback in handoff             │
│  └─ NO → enable_second_agent?                 │
│       ├─ YES → escalate_agent with full ctx   │
│       └─ NO → status = paused, notify user    │
└───────────────────────────────────────────────┘
```

### Idempotency & Locking Guarantees

PipelineController must guarantee:
- **One active stage per workspace** — check `pipeline_states.status == running` and `current_stage_id` before starting
- **Safe transitions** — update `pipeline_states` in DB first, then start execution. If execution start fails, rollback `pipeline_states` to previous state. Note: true DB+process atomicity is not possible since child process spawn happens outside DB transactions
- **Dedupe callbacks** — exit monitor passes `execution_process_id`; controller checks it matches current stage in `stage_history`. Already processed → skip

## Verdict Contract

Verdict is parsed from `CodingAgentTurn.summary`. Each role's prompt ends with:

```
End your response with a verdict block:
\`\`\`json
{"verdict": "approved|needs_changes|failed", "summary": "...", "issues": [...]}
\`\`\`
```

### Verdict Parsing Strategy

`CodingAgentTurn.summary` is currently truncated to 4096 characters. A long final response can cut off the verdict JSON block. To handle this:

1. **v1**: Parse verdict from the **end** of summary — scan last 1024 characters for a JSON block matching the verdict schema. If not found, treat as `failed` → `paused`
2. **Future**: Add a dedicated `stage_verdict` field to `CodingAgentTurn` (requires migration) for reliable storage independent of summary truncation

Agents always exit with code 0. Both `approved` and `needs_changes` are successful exits. Exit code != 0 means crash/error, not a stage verdict.

## Stage Approval Contract

When a stage has `approval: "approval"`, PipelineController pauses **before starting** that stage.

### Pre-stage Approval (before execution)

Pipeline pauses and presents the handoff from the previous stage for user review:

```json
{
  "awaiting_approval": true,
  "approval_stage_id": "reviewer",
  "approval_type": "pre_stage",
  "approval_payload": {
    "from_stage": "planner",
    "handoff": {
      "final_plan": "...",
      "constraints": ["..."]
    }
  }
}
```

User sees what will be passed to the next stage and decides: approve (proceed) or reject (go back).

### Post-stage Result (after execution)

Not an approval gate — this is the verdict from the completed stage, stored separately in `stage_history`:

```json
{
  "stage_id": "reviewer",
  "verdict": "approved",
  "summary": "plan reviewed, no issues",
  "handoff": { "..." }
}
```

### API Endpoints

- `POST /api/workspaces/:id/pipeline/approve` — continue pipeline to next stage
- `POST /api/workspaces/:id/pipeline/reject` — return to previous stage with user feedback
- `POST /api/workspaces/:id/pipeline/pause` — manually pause pipeline

UI shows [Approve] [Reject] buttons when `awaiting_approval = true`.

## Pipeline State Storage

### Two-level storage

**1. `pipeline_states` table — runtime source of truth:**

Tied to `workspace_id` (not session). One workspace = one pipeline instance.

```sql
CREATE TABLE pipeline_states (
    id                  BLOB PRIMARY KEY,
    workspace_id        BLOB NOT NULL UNIQUE,
    pipeline_config     TEXT NOT NULL,       -- JSON snapshot of config
    current_stage_id    TEXT NOT NULL,
    status              TEXT NOT NULL,       -- running/paused/completed/failed/ready_for_pr
    retry_counts        TEXT NOT NULL,       -- JSON
    stage_history       TEXT NOT NULL,       -- JSON array of stage completions
    handoff_artifacts   TEXT NOT NULL,       -- JSON structured handoffs
    role_sessions       TEXT NOT NULL,       -- JSON map of role → session_id
    awaiting_approval   INTEGER NOT NULL DEFAULT 0,
    approval_stage_id   TEXT,
    approval_payload    TEXT,               -- JSON
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL
);
```

**2. `issue.extension_metadata` — best-effort UI mirror:**

For kanban card display. Written best-effort — pipeline controller does not depend on success (workspace may not have issue_id).

```json
{
  "pipeline": {
    "current_stage": "builder",
    "status": "running",
    "attempt": "1/3",
    "ready_for_pr": false
  }
}
```

### Role Session Map

Each role gets its own session, reused across retry cycles via follow-up:

```json
{
  "role_sessions": {
    "planner": "session-uuid-1",
    "reviewer": "session-uuid-2",
    "builder": "session-uuid-3",
    "code_reviewer": "session-uuid-4",
    "tester": "session-uuid-5"
  }
}
```

- Planner ↔ Reviewer cycle: each reuses its own session via follow-up
- Builder fix: follow-up in builder's session (same session, Fixer is not separate)
- New session created only on first invocation of a role

### Pipeline State Fields

| Field | Description |
|-------|-------------|
| `pipeline_config` | Snapshot of the pipeline config at task creation time |
| `current_stage_id` | Currently active stage |
| `status` | `running` / `paused` / `completed` / `failed` / `ready_for_pr` |
| `retry_counts` | Retry counters for each correction cycle |
| `stage_history` | Log of all completed stages with execution_process_id links |
| `handoff_artifacts` | Structured handoff data between stages |
| `role_sessions` | Map of role → session_id for session reuse |
| `awaiting_approval` | Whether pipeline is waiting for user approval |
| `approval_stage_id` | Which stage is awaiting approval |
| `approval_payload` | Verdict/handoff data for the pending approval |

## Context Passing Between Stages

### Session Reuse (Same agent, same role cycle)

Uses follow-up within the role's session. Planner follow-up gets Reviewer's feedback in prompt. Builder follow-up gets Code Reviewer's issues.

### Resume (Same agent family, cross-role)

If Builder and Code Reviewer are both Claude Code, Code Reviewer can resume builder's session via `--resume {session_id}` through the existing review route.

### Mixed Executor Rule

If Code Reviewer is a **different agent family** than Builder (e.g., Claude → Codex), review runs in a **separate session without resume**, receiving only handoff + git diff.

### Prompt Injection (Cross-agent handoff)

When agents differ, structured handoff artifact is injected into the prompt. Full context passed without truncation (models support 1M context).

### Structured Handoff Artifact

Not raw conversation history, not a single summary string. A structured package:

**Planner → Builder:**
```json
{
  "final_plan": "full agreed plan in markdown",
  "review_summary": "brief review outcome: what was contested, what was resolved",
  "constraints": ["don't break API", "keep backward compat"],
  "risks": ["migration may be slow"],
  "acceptance_criteria": ["endpoint returns 200", "tests pass"]
}
```

**Builder → Code Reviewer:**
- git diff from pipeline start
- final_plan from handoff
- acceptance_criteria

**Code Reviewer → Builder (fix):**
- list of issues with file:line references
- delivered via follow-up in builder's session

**Tester → Code Reviewer (on failure):**
- test report (pass/fail per test)
- final_plan + diff + acceptance_criteria

### Automatic Method Selection

The system chooses the context passing method automatically based on agent types in pipeline config. Not exposed in UI.

## Prompt Templates

Hardcoded in the codebase, not visible to users. Each role gets a system prompt via executor's `append_prompt`.

Subagent instructions baked into prompts for Builder, Code Reviewer, Tester. Not a separate UI setting.

All prompts in English. Each prompt ends with verdict output instruction.

## Code Review Stage

Code Review stage uses the **existing review route** (`POST /api/sessions/:id/review`) and `ReviewRequest` structure. This route already supports:
- `executor_config` for agent selection
- `context` with repo review context (repo_id, base_commit)
- `session_id` for resume (same agent family only)

For mixed executors (e.g., Claude builder → Codex reviewer), review runs in a new session with handoff + diff only.

## UI Design

### Task Creation (Pipeline Settings)

Pipeline settings appear in the task creation form, next to agent selection:

```
☐ Pipeline                                      (?)
┌────────────────────────────────────────────────┐
│  Template: [Full Pipeline ▾]                   │
│                                                │
│  Planner      [Claude Code ▾]  [Auto    ▾] (?) │
│  Reviewer     [Claude Code ▾]  [Approval▾] (?) │
│  Builder      [Claude Code ▾]  [Auto    ▾] (?) │
│  Code Review  [Claude Code ▾]  [Auto    ▾] (?) │
│  Tester       [Claude Code ▾]  [Auto    ▾] (?) │
│                                                │
│  ☐ Second agent: [Codex ▾] after [3] attempts │
│  ☑ Create PR after completion               (?)│
│                                                │
│  [+ Add stage]  [🗑 Remove stage]              │
└────────────────────────────────────────────────┘
```

Pipeline checkbox disabled = single agent mode (current behavior).

### Tooltip Reference (Russian)

| Element | Tooltip |
|---------|---------|
| Pipeline (?) | Включить многоэтапный pipeline. Задача проходит через несколько AI-агентов: планирование, ревью, написание кода, проверка и тестирование |
| Planner (?) | Создаёт детальный план реализации на основе описания задачи |
| Reviewer (?) | Проверяет план на качество и полноту. Если найдены проблемы — возвращает на доработку |
| Builder (?) | Пишет код по одобренному плану. Также исправляет баги найденные на ревью кода |
| Code Review (?) | Проверяет написанный код на ошибки, баги и соответствие плану |
| Tester (?) | Планирует тесты на основе ТЗ и архитектуры, пишет и запускает их |
| Agent dropdown (?) | Какой AI-агент выполняет этот этап |
| Approval dropdown (?) | Auto — этап запускается автоматически. Одобрение — ждёт вашего подтверждения перед началом |
| Second agent (?) | Когда основной агент не может исправить ошибки после нескольких попыток, задача передаётся второму агенту с полным контекстом: план, код и описание ошибок |
| After attempts (?) | После скольких неудачных попыток основного агента подключить второго |
| Create PR (?) | После успешного прохождения всех этапов pipeline автоматически создаст Pull Request и карточка перейдёт в колонку "In Review". Работает только для single-repo workspace с настроенной git авторизацией |

### Kanban Card Progress

Compact progress indicator on the workspace card (via workspace_summary):

```
┌──────────────────────────────────┐
│ Task Title                       │
│                                  │
│ ● ● ● ○ ○   Builder (3/5)       │
│ Attempt 1/3 · Claude Code    🔄 │
└──────────────────────────────────┘
```

States:
- 🔄 Running
- ⏸ Paused (waiting for approval or retry limit reached)
- "Escalation · Codex" when second agent is active

Details (stage history, logs, errors) visible on card click.

### Kanban Column Transitions

All internal pipeline stages stay in **In Progress**. `ready_for_pr` is a pipeline status inside `pipeline_states`, NOT a kanban column.

Uses existing Vibe Kanban auto-move mechanisms:
- **Todo → In Progress**: workspace creation (already exists)
- Pipeline completes → `pipeline_states.status = ready_for_pr` (card stays in **In Progress**)
- If `auto_create_pr` enabled + single-repo + valid auth → PR created automatically → card moves to **In Review**
- If conditions not met → card stays in **In Progress** with `pipeline_states.status = ready_for_pr`, user creates PR manually
- **In Review → Done**: PR merge (already exists)

## Key Files to Modify

### Backend (Rust)

| File | Change |
|------|--------|
| `crates/services/src/services/pipeline_controller.rs` | **New file.** Thin state machine: read verdict, update state, start next stage |
| `crates/executors/src/actions/mod.rs` | Add `PipelineConfig` struct, `StageConfig`, `Verdict`, `HandoffArtifact` types |
| `crates/local-deployment/src/container.rs` | Call `pipeline_controller.handle_stage_completed()` from `spawn_exit_monitor()` |
| `crates/server/src/routes/sessions/mod.rs` | Accept pipeline config in attempt creation |
| `crates/server/src/routes/workspaces/` | Add pipeline approve/reject/pause endpoints |
| `crates/db/src/models/pipeline_state.rs` | **New file.** PipelineState model |
| `crates/db/migrations/` | **New migration.** Create `pipeline_states` table |
| `crates/server/src/routes/workspaces/workspace_summary.rs` | Add pipeline progress data to workspace summary |

### Frontend (TypeScript/React)

| File | Change |
|------|--------|
| `packages/web-core/src/features/kanban/ui/` | Pipeline progress indicator on workspace cards |
| `packages/web-core/src/shared/dialogs/` | Pipeline settings panel in task creation form |
| `packages/web-core/src/shared/components/` | Approval buttons for pipeline stage approval |
| `shared/types.ts` | Pipeline config and state TypeScript types (generated from Rust) |

## Modes

### Single Agent (Pipeline off)
Current behavior — one agent, one execution. No changes needed.

### Pipeline (Single agent type)
All stages use the same agent (e.g., Claude Code). Context passed via session follow-up and resume. Most efficient mode.

### Pipeline (Two agents with escalation)
Primary agent runs all stages. If Tester fails after `escalate_after_retries`, full context (all handoff artifacts + git diff + test report) is passed to second agent (e.g., Codex) via prompt injection. Second agent reviews and fixes the entire implementation.
