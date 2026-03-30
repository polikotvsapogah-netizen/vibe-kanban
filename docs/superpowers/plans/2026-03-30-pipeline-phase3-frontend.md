# Multi-Agent Pipeline — Phase 3: Frontend Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add pipeline UI to Vibe Kanban — pipeline progress on workspace cards, pipeline settings in task creation, approval buttons, and pipeline API client.

**Architecture:** Pipeline fields already exist in WorkspaceSummary types. Add a `pipelineApi` client, extend workspace cards with progress indicator, add pipeline toggle with stage config in workspace creation, and pipeline approval dialog.

**Tech Stack:** React, TypeScript, Tailwind CSS, @vibe/ui components, React Query, i18n

**Spec:** `docs/superpowers/specs/2026-03-30-multi-agent-pipeline-design.md`

---

## File Structure

| File | Responsibility |
|------|---------------|
| **Create:** `packages/web-core/src/shared/lib/pipelineApi.ts` | API client for pipeline endpoints |
| **Create:** `packages/web-core/src/shared/components/PipelineProgress.tsx` | Pipeline progress indicator (dots + stage name) |
| **Create:** `packages/web-core/src/shared/components/PipelineApprovalBar.tsx` | Approve/Reject buttons for pipeline stages |
| **Create:** `packages/web-core/src/shared/components/PipelineSettings.tsx` | Pipeline config panel for workspace creation |
| **Modify:** `packages/ui/src/components/IssueWorkspaceCard.tsx` | Add pipeline progress to workspace cards |
| **Modify:** workspace creation dialog | Add pipeline toggle + settings |
| **Create:** `packages/web-core/src/i18n/locales/en/pipeline.json` | English translations |
| **Create:** `packages/web-core/src/i18n/locales/ru/pipeline.json` | Russian translations (tooltips) |

---

### Task 1: Pipeline API Client
### Task 2: Pipeline Progress Component
### Task 3: Pipeline Progress on Workspace Cards
### Task 4: Pipeline Approval Bar
### Task 5: Pipeline Settings Component
### Task 6: Pipeline Settings in Workspace Creation
### Task 7: i18n Translations
