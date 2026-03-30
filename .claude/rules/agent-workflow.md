# Agent Workflow

This project is developed by a team of Claude Code agents, each with a specific role.

**All code changes — features, bug fixes, patches, one-offs — must go through the full agent
workflow: implementer → reviewer + UAT.** This is not limited to numbered slices.

## Roles

| Role | Subagent | Responsibility |
|------|----------|---------------|
| **Implementer** | `@implementer` | Writes production code and tests in an isolated worktree. |
| **Reviewer** | `@reviewer` | Audits code and tests against architecture doc and conventions. Read-only. |
| **UAT** | `@uat` | Builds the binary, validates observable behavior end-to-end. |
| **Doc** | `@doc` | Writes/maintains user-facing MkDocs documentation. |

## Feature Branch Workflow

All code changes follow this workflow on a **feature branch**. The orchestrator manages the branch.

```
0. Create feature branch:  git checkout -b feat/<name> main
1. @implementer            → worktree off feature branch; orchestrator merges worktree into feature branch
2. @doc + @reviewer + @uat → all three in parallel
                              doc in worktree; reviewer + UAT on feature branch
3. Orchestrator merges doc worktree into feature branch
4. Fix any issues           → orchestrator commits fixes to feature branch
5. Push feature branch, create single PR
6. Clean up worktrees after PR merges
```

If any role reports a BLOCKER, the implementer re-runs to fix it before retrying.

**Key rules:**
- The orchestrator **MUST be on the feature branch** before invoking any agent.
- **Never merge worktree branches into `main`** — merge them into the feature branch.
- Doc, reviewer, and UAT can all run in parallel after the implementer finishes.
- The doc agent does NOT create a separate PR when part of a feature pipeline.
- `main` stays clean until the PR merges via GitHub.
- Ad-hoc fixes go directly on the feature branch.

## Parallel Sessions (Session Worktrees)

For running multiple Claude Code sessions in parallel on the same repo, each session should
operate in its own **session worktree**. The main checkout stays on `main` permanently.

**The human creates the session worktree before launching Claude Code:**

```bash
# From the main checkout (always on main):
git worktree add .claude/sessions/<name> -b feat/<name>

# Launch Claude Code in that session:
cd .claude/sessions/<name>
claude
```

Each session is independent. The Claude instance in each worktree follows the standard feature
branch workflow above — the orchestrator is already on the feature branch because the worktree
IS the branch. Agents within the session create their own sub-worktrees as normal.

**Parallel sessions example:**
```
.claude/sessions/
├── feat-status-output/    ← Terminal 1: cd here, run claude
├── fix-kafka-flag/        ← Terminal 2: cd here, run claude
└── docs-update-guides/    ← Terminal 3: cd here, run claude
```

**When NOT to use session worktrees:** if you're only working on one thing at a time, just
create a branch directly in the main checkout. Session worktrees are for parallelism.

## Workflow per Slice

Phase-plan slices follow the same feature branch workflow. The slice ID is passed via `$ARGUMENTS`:

```
0. git checkout -b feat/slice-0.2 main
1. @implementer 0.2    → worktree; orchestrator merges into feat/slice-0.2
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

Definitions live in `.claude/agents/`. Frontmatter controls tools, model, permissionMode, and
isolation. The slice ID is passed via `$ARGUMENTS`.

## Worktree Cleanup

Worktrees carry their own `target/` (~2 GB+). The orchestrator must prune after merging:

```bash
rm -rf .claude/worktrees/   # delete agent worktree directories and build artifacts
git worktree prune           # remove git's stale references
```

For session worktrees, clean up after the PR merges:

```bash
rm -rf .claude/sessions/<name>/   # delete the session worktree
git worktree prune                 # remove git's stale references
git branch -d feat/<name>          # delete the merged branch
```

Run cleanup after completing a slice, merging a feature branch, or when >5 worktrees exist.
