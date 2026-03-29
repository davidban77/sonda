# Agent Workflow

This project is developed by a team of Claude Code agents, each with a specific role.

**All code changes — features, bug fixes, patches, one-offs — must go through the full agent
workflow: implementer → tester → reviewer + UAT.** This is not limited to numbered slices.

## Roles

| Role | Subagent | Responsibility |
|------|----------|---------------|
| **Implementer** | `@implementer` | Writes production code in an isolated worktree. Does not write tests. |
| **Tester** | `@tester` | Writes unit + integration tests, runs them. |
| **Reviewer** | `@reviewer` | Audits code against architecture doc and conventions. Read-only. |
| **UAT** | `@uat` | Builds the binary, validates observable behavior end-to-end. |
| **Doc** | `@doc` | Writes/maintains user-facing MkDocs documentation. |

## Feature Branch Workflow

All code changes follow this workflow on a **feature branch**. The orchestrator manages the branch.

```
0. Create feature branch:  git checkout -b feat/<name> main
1. @implementer            → worktree off feature branch; orchestrator merges worktree into feature branch
2. @tester                 → runs on feature branch (no worktree), commits tests
3. @reviewer               → reads feature branch, reports PASS/FAIL/PASS WITH NOTES
4. @uat                    → builds feature branch, validates observable behavior
5. Fix any issues           → orchestrator commits fixes to feature branch
6. @doc (if needed)        → worktree off feature branch; orchestrator merges worktree into feature branch
7. Push feature branch, create single PR
8. Clean up worktrees after PR merges
```

If any role reports a BLOCKER, the implementer re-runs to fix it before retrying.

**Key rules:**
- The orchestrator **MUST be on the feature branch** before invoking any agent.
- **Never merge worktree branches into `main`** — merge them into the feature branch.
- The tester runs directly on the feature branch (no worktree).
- The doc agent does NOT create a separate PR when part of a feature pipeline.
- `main` stays clean until the PR merges via GitHub.
- Ad-hoc fixes go directly on the feature branch.

## Workflow per Slice

Phase-plan slices follow the same feature branch workflow. The slice ID is passed via `$ARGUMENTS`:

```
0. git checkout -b feat/slice-0.2 main
1. @implementer 0.2   → worktree; orchestrator merges into feat/slice-0.2
2. @tester 0.2        → commits to feat/slice-0.2
3. @reviewer 0.2      → reads feat/slice-0.2, reports
4. @uat 0.2           → builds feat/slice-0.2, validates
5. Push and create PR; human reviews and approves
```

## Rules for All Agents

- **Read the slice spec first** from the phase plan in `docs/`.
- **Read `docs/architecture.md`** for design decisions before writing or reviewing code.
- **Read the crate `CLAUDE.md`** before modifying any crate.
- **One slice at a time.** Never work ahead.
- **Commit after each role.** Implementer commits code, tester commits tests. Reviewer and UAT report only.
- **Exit gates are hard.** A slice is not done until all four roles have passed.

## Subagent Details

Definitions live in `.claude/agents/`. Frontmatter controls tools, model, permissionMode, and
isolation. The slice ID is passed via `$ARGUMENTS`.

## Worktree Cleanup

Worktrees carry their own `target/` (~2 GB+). The orchestrator must prune after merging:

```bash
rm -rf .claude/worktrees/   # delete worktree directories and build artifacts
git worktree prune           # remove git's stale references
```

Run cleanup after completing a slice, merging a feature branch, or when >5 worktrees exist.
