# Agent Workflow Guide — How to Run Sonda's Development Team

This guide explains how to use Claude Code's subagent system to develop Sonda slice by slice.

## Prerequisites

- Claude Code CLI installed and authenticated
- This repo cloned locally
- Rust toolchain installed (`rustup`, `cargo`)

## Your `.claude/` Structure

```
.claude/
├── agents/              ← Subagent definitions (the "team")
│   ├── implementer.md   ← Writes production code (sonnet, isolated worktree)
│   ├── tester.md        ← Writes and runs tests (sonnet)
│   ├── reviewer.md      ← Audits code quality (opus, read-only)
│   └── uat.md           ← Validates as a real user (opus)
└── skills/              ← Reusable workflow patterns
    ├── add-generator/   ← Steps for adding a ValueGenerator
    ├── add-encoder/     ← Steps for adding an Encoder
    └── add-sink/        ← Steps for adding a Sink
```

## Running a Slice

### Step 1: Start Claude Code in the repo root

```bash
cd /path/to/sonda
claude
```

Claude Code automatically reads `CLAUDE.md` and discovers the agents in `.claude/agents/`.

### Step 2: Spawn the implementer

In the Claude Code session, type:

```
@implementer 0.0
```

This spawns the implementer subagent with `$ARGUMENTS=0.0`. It will:
- Read the slice spec from `docs/phase-0-mvp.md`
- Read `docs/architecture.md` and the relevant crate `CLAUDE.md`
- Verify prerequisites compile
- Write the production code
- Run `cargo build`, `cargo clippy`, `cargo fmt`
- Commit with message `feat(slice-0.0): ...`

The implementer runs in an **isolated git worktree** — it can't break your main branch.

### Step 3: Review the implementer's work

When the implementer finishes, review its output. If it looks good, proceed.

### Step 4: Spawn the tester

```
@tester 0.0
```

The tester reads the slice spec and the implementer's code, writes tests, runs them, and commits.

### Step 5: Spawn the reviewer

```
@reviewer 0.0
```

The reviewer is **read-only** (plan permission mode). It audits code against the architecture doc
and reports PASS / FAIL / PASS WITH NOTES. It does not modify any files.

### Step 6: Spawn the UAT agent

```
@uat 0.0
```

The UAT agent builds the binary, runs it as a real user would, and validates observable behavior
against the slice's UAT criteria. It reports a structured verdict.

### Step 7: Human approval gate

Review all four reports. If everything passes, move to the next slice:

```
@implementer 0.1
```

If any role reported a BLOCKER, fix it by re-running the implementer:

```
@implementer 0.0
```

Then re-run tester → reviewer → UAT.

## Slice Execution Order

Follow the dependency chain in each phase plan. Phase 0 example:

```
0.0 → 0.1 → 0.2 → 0.3 → 0.4 → 0.5 → 0.6 → 0.7 → 0.8
```

Each slice's "Input state" section tells you what must exist before starting it.

## How Subagents Work

| Property | Implementer | Tester | Reviewer | UAT |
|----------|-------------|--------|----------|-----|
| Model | sonnet | sonnet | opus | opus |
| Can write files | ✓ | ✓ | ✗ | ✗ |
| Isolation | worktree | none | none | none |
| Permission mode | acceptEdits | acceptEdits | plan | default |
| Commits | yes | yes | no | no |

**Model choice**: Sonnet is fast and good for straightforward implementation. Opus is used for
reviewer and UAT because they need deeper reasoning about correctness and architecture compliance.

**Isolation**: The implementer uses `isolation: worktree` which creates a temporary git worktree.
This means its changes are isolated until merged. Other agents work on the main working tree.

**Permission mode**: `acceptEdits` lets the agent write files with your approval. `plan` makes
the reviewer read-only — it can only observe and report.

## Using Skills

Skills are reusable workflow guides that agents reference during their work. You can also
reference them directly:

```
Read the skill at .claude/skills/add-generator/SKILL.md and follow it to add a spike generator.
```

Skills don't run as agents — they're knowledge documents that standardize how recurring tasks
should be done.

## Tips

1. **One slice at a time.** Don't skip ahead. Each slice depends on the verified output of the
   previous one.

2. **Trust but verify.** Read the agent reports. The reviewer and UAT agents are specifically
   designed to catch issues the implementer missed.

3. **Re-run, don't patch.** If a slice has issues, re-run the implementer for that slice rather
   than manually patching code. This keeps the agent workflow clean.

4. **Phase plans are the source of truth.** If you're unsure what a slice should produce, read
   the phase plan doc — it has exact file lists, type signatures, and acceptance criteria.

5. **Check git log after each agent.** Agents commit their work. Use `git log --oneline` to
   see what changed and verify commit messages follow the convention.