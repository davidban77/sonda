# Agent Workflow

This project is developed by a team of Claude Code agents, each with a specific role.

**All code changes — features, bug fixes, patches, one-offs — must go through the full agent
workflow: implementer → reviewer + UAT.** This is not limited to numbered slices.

## Roles

| Role | Subagent | Responsibility |
|------|----------|---------------|
| **Implementer** | `@implementer` | Writes production code and tests on the current branch. |
| **Reviewer** | `@reviewer` | Audits code and tests against architecture doc and conventions. Read-only. |
| **UAT** | `@uat` | Builds the binary, validates observable behavior end-to-end. |
| **Doc** | `@doc` | Writes/maintains user-facing MkDocs documentation. |

## Feature Branch Workflow

All code changes follow this workflow on a **feature branch**. The orchestrator manages the branch.

```
0. Create feature branch:  git checkout -b feat/<name> main
1. @implementer            → works directly on the feature branch
2. @doc + @reviewer + @uat → all three in parallel on the feature branch
3. Fix any issues           → orchestrator sends fixes back through implementer
4. Push feature branch, create single PR
5. Human reviews and merges
```

If any role reports a BLOCKER, the implementer re-runs to fix it before retrying.

**Key rules:**
- The orchestrator **MUST be on the feature branch** before invoking any agent.
- All agents work on the **current branch in the current directory** — no worktree isolation.
- Doc, reviewer, and UAT can all run in parallel after the implementer finishes.
- The doc agent does NOT create a separate PR when part of a feature pipeline.
- `main` stays clean until the PR merges via GitHub.
- Ad-hoc fixes go directly on the feature branch.

## Session Management

The **human** manages worktrees and sessions. The orchestrator never creates worktrees.

- The human creates worktrees/sessions before launching Claude Code
- Claude Code works in whatever directory it was launched from
- The orchestrator focuses on the current folder and git branch — no worktree logistics

**Parallel sessions example:**
```
.claude/sessions/
├── feat-status-output/    ← Terminal 1: cd here, run claude
├── fix-kafka-flag/        ← Terminal 2: cd here, run claude
└── docs-update-guides/    ← Terminal 3: cd here, run claude
```

## Workflow per Slice

Phase-plan slices follow the same feature branch workflow. The slice ID is passed via `$ARGUMENTS`:

```
0. git checkout -b feat/slice-0.2 main
1. @implementer 0.2    → works on feat/slice-0.2
2. @doc + @reviewer 0.2 + @uat 0.2  → in parallel
3. Fix issues, push, create PR; human reviews and approves
```

## Rules for All Agents

- **Read the slice spec first** from the phase plan in `docs/`.
- **Read `docs/architecture.md`** for design decisions before writing or reviewing code.
- **Read the crate `CLAUDE.md`** before modifying any crate.
- **One slice at a time.** Never work ahead.
- **Commit after the implementer.** Implementer commits code and tests. Reviewer and UAT report only.
- **Exit gates are hard.** A slice is not done until reviewer and UAT have passed.

## Exceptional Use: Tester Agent

The tester agent (`@tester`) is available for cases where adversarial testing adds clear value
beyond what the implementer + reviewer + UAT provide:

- Security-sensitive code changes
- Large cross-crate refactors
- Cases where the orchestrator explicitly requests a separate testing pass

When used, the tester runs on the feature branch after the implementer and before the
reviewer + UAT stage.

## Subagent Details

Definitions live in `.claude/agents/`. Frontmatter controls tools, model, and permissionMode.
The slice ID is passed via `$ARGUMENTS`.
