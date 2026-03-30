# Multi-Agent Pipeline for Vibe Kanban

## Overview

Add a multi-agent pipeline feature to Vibe Kanban that chains multiple AI agent stages (Planner, Reviewer, Builder, Code Reviewer, Fixer, Tester) with configurable agents, approval policies, retry cycles, and escalation to a second agent.

## Pipeline Stages (Default)

```
Planner → Reviewer → Builder → Code Reviewer → Fixer → Tester
   ↑          |         ↑            |      ↑      |       |
   └──────────┘         └────────────┘      └──────┘       |
  (план плохой)       (баги в коде)    (цикл правок)       |
                                                           |
                        Code Reviewer ←────────────────────┘
                             ↑              (продукт сломан)
                             └── Fixer ← ...
```

1. **Planner** — creates a detailed implementation plan from the task description
2. **Reviewer** — critically reviews the plan for quality, completeness, edge cases. Fail → back to Planner
3. **Builder** — writes code following the approved plan
4. **Code Reviewer** — reviews written code for bugs, logic errors, deviations from plan. Fail → Fixer
5. **Fixer** — fixes issues found in code review. Always returns to Code Reviewer for re-check
6. **Tester** — plans tests based on spec and architecture, writes and runs them. Fail → back to Code Reviewer

## Pipeline Config Structure

Pipeline is described as a JSON config stored as a pipeline profile.

```json
{
  "name": "Full Pipeline",
  "enable_second_agent": false,
  "default_max_retries": 3,
  "auto_create_pr": true,
  "stages": [
    {
      "id": "planner",
      "role": "planner",
      "agent": "claude_code",
      "approval": "auto",
      "use_subagents": false,
      "on_success": "reviewer",
      "on_fail": "pause",
      "max_retries": null
    },
    {
      "id": "reviewer",
      "role": "reviewer",
      "agent": "claude_code",
      "approval": "approval",
      "use_subagents": true,
      "on_success": "builder",
      "on_fail": "planner",
      "max_retries": 3
    },
    {
      "id": "builder",
      "role": "builder",
      "agent": "claude_code",
      "approval": "auto",
      "use_subagents": true,
      "on_success": "code_reviewer",
      "on_fail": "pause",
      "max_retries": null
    },
    {
      "id": "code_reviewer",
      "role": "code_reviewer",
      "agent": "claude_code",
      "approval": "auto",
      "use_subagents": true,
      "on_success": "tester",
      "on_fail": "fixer",
      "max_retries": 3
    },
    {
      "id": "fixer",
      "role": "fixer",
      "agent": "claude_code",
      "approval": "auto",
      "use_subagents": false,
      "on_success": "code_reviewer",
      "on_fail": "code_reviewer",
      "max_retries": null
    },
    {
      "id": "tester",
      "role": "tester",
      "agent": "claude_code",
      "approval": "auto",
      "use_subagents": true,
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
| `default_max_retries` | Default maximum retry count for all stages with correction cycles (default: 3) |
| `auto_create_pr` | Automatically create a Pull Request after successful pipeline completion, moving the task card to "In Review" |
| `id` | Unique stage identifier |
| `role` | Stage role — determines the task the agent performs (planning, review, coding, testing) |
| `agent` | Which AI agent executes this stage (claude_code, codex, gemini, etc.) |
| `approval` | Whether user confirmation is required before this stage starts. `auto` — starts automatically, `approval` — waits for user OK |
| `use_subagents` | Allow agent to spawn parallel subagents for faster work (e.g., Builder writes multiple modules simultaneously) |
| `on_success` | Which stage to run after successful completion. `complete` = pipeline finished |
| `on_fail` | What happens on failure. Can point to a previous stage for rework or `pause` for user decision |
| `max_retries` | Max retry count for this specific stage's correction cycle. `null` = use `default_max_retries` |
| `escalate_agent` | Second agent that takes over when primary fails after retries. Receives full context: plan, code, and errors |
| `escalate_after_retries` | How many failed attempts before escalating to the second agent |

## Approach: Pipeline Config + Existing Execution Flow (Approach C)

Pipeline config with a state machine drives transitions. Logic is embedded into the existing `spawn_exit_monitor()` — no separate orchestrator service needed.

### Why Approach C

- Retry cycles (Code Reviewer <-> Fixer, Tester -> Code Reviewer) are native — just transitions in the config
- Adding/removing stages = editing JSON config, not rebuilding action chains
- Escalation to Codex = `on_fail_after_retries: "escalate_to_codex"` in config
- Minimal new code — reuses existing exit monitor and execution flow

## Exit Monitor — Transition Logic

When an agent completes a stage, `handle_pipeline_transition()` in `spawn_exit_monitor()` determines what happens next:

```
Agent completed
       │
       ▼
Read pipeline config from session metadata
       │
       ▼
Determine result: success or failure?
  ├─ exit code 0 + agent did not request fixes → SUCCESS
  └─ exit code != 0 or agent reported problems → FAIL
       │
       ▼
┌─ SUCCESS ─────────────────────────┐
│ Read on_success of current stage  │
│ "reviewer" → start reviewer      │
│ "complete" → pipeline finished   │
└───────────────────────────────────┘
       │
┌─ FAIL ────────────────────────────────────────┐
│ retry_count < max_retries?                     │
│  ├─ YES → on_fail stage (e.g., planner)       │
│  │        retry_count++                        │
│  │        pass feedback in prompt              │
│  └─ NO → enable_second_agent?                 │
│       ├─ YES → start escalate_agent (Codex)   │
│       │        with full context               │
│       └─ NO → PAUSE, notify user              │
└───────────────────────────────────────────────┘
```

### Success/Failure Detection

Each stage's agent receives a role-specific prompt instructing it to exit with code 0 on success and code 1 on failure with a description of issues found.

## Pipeline State

Stored as a JSON field in session metadata. Pipeline state is tied 1:1 to a session.

```json
{
  "pipeline_config": { "..." },
  "current_stage_id": "builder",
  "status": "running",
  "retry_counts": {
    "reviewer→planner": 0,
    "code_reviewer→fixer": 1,
    "tester→code_reviewer": 0
  },
  "stage_history": [
    {
      "stage_id": "planner",
      "execution_process_id": "uuid-1",
      "result": "success",
      "started_at": "...",
      "completed_at": "..."
    }
  ]
}
```

### Fields

| Field | Description |
|-------|-------------|
| `pipeline_config` | Snapshot of the pipeline config at task creation time (changes to settings don't affect running pipelines) |
| `current_stage_id` | Currently active stage |
| `status` | `running` / `paused` / `completed` / `failed` |
| `retry_counts` | Retry counters for each correction cycle |
| `stage_history` | Log of all completed stages with execution_process_id links (for accessing logs of each stage) |

## Context Passing Between Stages

### Same Agent Type (Claude → Claude, Codex → Codex)

Uses `--resume {session_id}` — the next agent gets the full conversation history of the previous stage. Session ID is stored in `CodingAgentTurn` after each execution.

### Different Agent Types (Claude → Codex) or Escalation

Prompt injection — output of the previous agent (plan, code diff, errors) is extracted and inserted into the next agent's prompt:

- Plan: extracted from Planner execution logs
- Code: `git diff` from pipeline start to current state
- Errors: from the last failed stage's logs

Full context is passed without truncation (models support 1M context).

### Automatic Selection

The system automatically chooses the method: resume if agents are the same type, prompt injection if different. Not exposed in UI.

## Prompt Templates

Hardcoded in the codebase, not visible to users. Each role gets a system prompt via `append_prompt`:

- **Planner**: Create detailed implementation plan, output structured steps. Exit 0 when done.
- **Reviewer**: Critically review plan for edge cases, architecture, security. Exit 0 if good, exit 1 with issues.
- **Builder**: Implement the approved plan. Use subagents for parallel modules if enabled. Exit 0 when done.
- **Code Reviewer**: Review all changes for bugs, logic errors, plan deviations. Exit 0 if clean, exit 1 with issues list.
- **Fixer**: Fix all issues from code review. Exit 0 when all fixed.
- **Tester**: Plan tests based on spec and architecture, write and run them. Exit 0 if all pass, exit 1 with failure descriptions.

When `use_subagents: true`, an instruction is appended: "Use the Agent tool to spawn parallel subagents for independent subtasks."

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
│  Fixer        [Claude Code ▾]  [Auto    ▾] (?) │
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
| Builder (?) | Пишет код по одобренному плану |
| Code Review (?) | Проверяет написанный код на ошибки, баги и соответствие плану |
| Fixer (?) | Исправляет проблемы найденные на ревью кода |
| Tester (?) | Планирует тесты на основе ТЗ и архитектуры, пишет и запускает их |
| Agent dropdown (?) | Какой AI-агент выполняет этот этап |
| Approval dropdown (?) | Auto — этап запускается автоматически. Одобрение — ждёт вашего подтверждения перед началом |
| Subagents (?) | Разрешить агенту запускать параллельных подагентов для ускорения работы |
| Second agent (?) | Когда основной агент не может исправить ошибки после нескольких попыток, задача передаётся второму агенту с полным контекстом: план, код и описание ошибок |
| After attempts (?) | После скольких неудачных попыток основного агента подключить второго |
| Create PR (?) | После успешного прохождения всех этапов pipeline автоматически создаст Pull Request и карточка перейдёт в колонку "In Review" |

### Kanban Card Progress

Compact progress indicator on the task card:

```
┌──────────────────────────────────┐
│ Task Title                       │
│                                  │
│ ● ● ● ○ ○ ○   Builder (3/6)     │
│ Attempt 1/3 · Claude Code    🔄 │
└──────────────────────────────────┘
```

States:
- 🔄 Running
- ⏸ Paused (waiting for approval or retry limit reached)
- "Escalation · Codex" when second agent is active

Details (stage history, logs, errors) visible on card click.

### Kanban Column Auto-Transitions

Uses existing Vibe Kanban auto-move mechanisms:
- **Todo → In Progress**: workspace creation (already exists)
- **In Progress → In Review**: auto-create PR after pipeline success (new, via `auto_create_pr` option)
- **In Review → Done**: PR merge (already exists)

## Key Files to Modify

### Backend (Rust)

| File | Change |
|------|--------|
| `crates/executors/src/actions/mod.rs` | Add `PipelineConfig` struct, stage definitions |
| `crates/executors/src/executors/claude.rs` | Role-specific `append_prompt` injection based on pipeline stage |
| `crates/services/src/services/container.rs` | Add `handle_pipeline_transition()`, pipeline state management |
| `crates/local-deployment/src/container.rs` | Extend `spawn_exit_monitor()` with pipeline transition logic |
| `crates/server/src/routes/sessions/mod.rs` | Accept pipeline config in attempt creation, store in session metadata |
| `crates/db/src/models/` | Add pipeline_state field to session or new model |

### Frontend (TypeScript/React)

| File | Change |
|------|--------|
| `packages/web-core/src/features/kanban/ui/` | Pipeline progress indicator on task cards |
| `packages/web-core/src/shared/dialogs/` | Pipeline settings panel in task creation form |
| `shared/types.ts` | Pipeline config and state TypeScript types (generated from Rust) |

## Modes

### Single Agent (Pipeline off)
Current behavior — one agent, one execution. No changes needed.

### Pipeline (Single agent type)
All stages use the same agent (e.g., Claude Code). Context passed via `--resume`. Most efficient mode.

### Pipeline (Two agents with escalation)
Primary agent runs all stages. If Tester fails after `escalate_after_retries`, full context is passed to second agent (e.g., Codex) via prompt injection. Second agent reviews and fixes the entire implementation.
