# Skill: Commit

Stage and commit changes. Do NOT append a `Co-Authored-By` trailer.

## Steps

1. Run `git status` and `git diff` (staged + unstaged) to understand what changed.
2. Analyze the changes and draft a concise commit message:
   - Use conventional commit prefixes: `feat:`, `fix:`, `chore:`, `test:`, `docs:`, `refactor:`.
   - Focus on the "why", not the "what".
   - Keep the first line under 72 characters.
3. Stage the relevant files by name (avoid `git add -A` or `git add .`).
4. Commit. Pass the message via HEREDOC:
   ```bash
   git commit -m "$(cat <<'EOF'
   <type>: <short description>
   EOF
   )"
   ```
5. Run `git status` to verify success.

## Rules

- No `Co-Authored-By` trailer.
- No `--no-verify` or `--no-gpg-sign`.
- Never amend unless explicitly asked.
- Do not push unless explicitly asked.
- If there are no changes, do not create an empty commit.
