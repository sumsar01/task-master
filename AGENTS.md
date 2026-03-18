# Agent Instructions

This project uses **bd** (beads) for issue tracking. Run `bd onboard` to get started.

## Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work atomically
bd close <id>         # Complete work
bd dolt push          # Push beads data to remote
```

## Non-Interactive Shell Commands

**ALWAYS use non-interactive flags** with file operations to avoid hanging on confirmation prompts.

Shell commands like `cp`, `mv`, and `rm` may be aliased to include `-i` (interactive) mode on some systems, causing the agent to hang indefinitely waiting for y/n input.

**Use these forms instead:**
```bash
# Force overwrite without prompting
cp -f source dest           # NOT: cp source dest
mv -f source dest           # NOT: mv source dest
rm -f file                  # NOT: rm file

# For recursive operations
rm -rf directory            # NOT: rm -r directory
cp -rf source dest          # NOT: cp -r source dest
```

**Other commands that may prompt:**
- `scp` - use `-o BatchMode=yes` for non-interactive
- `ssh` - use `-o BatchMode=yes` to fail instead of prompting
- `apt-get` - use `-y` flag
- `brew` - use `HOMEBREW_NO_AUTO_UPDATE=1` env var

<!-- BEGIN BEADS INTEGRATION -->
## Issue Tracking with bd (beads)

**IMPORTANT**: This project uses **bd (beads)** for ALL issue tracking. Do NOT use markdown TODOs, task lists, or other tracking methods.

### Why bd?

- Dependency-aware: Track blockers and relationships between issues
- Version-controlled: Built on Dolt with cell-level merge
- Agent-optimized: JSON output, ready work detection, discovered-from links
- Prevents duplicate tracking systems and confusion

### Quick Start

**Check for ready work:**

```bash
bd ready --json
```

**Create new issues:**

```bash
bd create "Issue title" --description="Detailed context" -t bug|feature|task -p 0-4 --json
bd create "Issue title" --description="What this issue is about" -p 1 --deps discovered-from:bd-123 --json
```

**Claim and update:**

```bash
bd update <id> --claim --json
bd update bd-42 --priority 1 --json
```

**Complete work:**

```bash
bd close bd-42 --reason "Completed" --json
```

### Issue Types

- `bug` - Something broken
- `feature` - New functionality
- `task` - Work item (tests, docs, refactoring)
- `epic` - Large feature with subtasks
- `chore` - Maintenance (dependencies, tooling)

### Priorities

- `0` - Critical (security, data loss, broken builds)
- `1` - High (major features, important bugs)
- `2` - Medium (default, nice-to-have)
- `3` - Low (polish, optimization)
- `4` - Backlog (future ideas)

### Workflow for AI Agents

1. **Check ready work**: `bd ready` shows unblocked issues
2. **Claim your task atomically**: `bd update <id> --claim`
3. **Work on it**: Implement, test, document
4. **Discover new work?** Create linked issue:
   - `bd create "Found bug" --description="Details about what was found" -p 1 --deps discovered-from:<parent-id>`
5. **Complete**: `bd close <id> --reason "Done"`

### Auto-Sync

bd automatically syncs with git:

- Exports to `.beads/issues.jsonl` after changes (5s debounce)
- Imports from JSONL when newer (e.g., after `git pull`)
- No manual export/import needed!

### Important Rules

- ✅ Use bd for ALL task tracking
- ✅ Always use `--json` flag for programmatic use
- ✅ Link discovered work with `discovered-from` dependencies
- ✅ Check `bd ready` before asking "what should I work on?"
- ❌ Do NOT create markdown TODO lists
- ❌ Do NOT use external issue trackers
- ❌ Do NOT duplicate tracking systems

For more details, see README.md and docs/QUICKSTART.md.

## Landing the Plane (Session Completion)

**When ending a work session**, you MUST complete ALL steps below. Work is NOT complete until `git push` succeeds.

**MANDATORY WORKFLOW:**

1. **File issues for remaining work** - Create issues for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **PUSH TO REMOTE** - This is MANDATORY:
   ```bash
   git pull --rebase
   bd dolt push
   git push
   git status  # MUST show "up to date with origin"
   ```
5. **Clean up** - Clear stashes, prune remote branches
6. **Verify** - All changes committed AND pushed
7. **Hand off** - Provide context for next session

**CRITICAL RULES:**
- Work is NOT complete until `git push` succeeds
- NEVER stop before pushing - that leaves work stranded locally
- NEVER say "ready to push when you are" - YOU must push
- If push fails, resolve and retry until it succeeds

<!-- END BEADS INTEGRATION -->

## PR Workflow & QA Agent

task-master includes an automated QA agent loop that runs between an agent opening a PR and humans reviewing it.

### Full workflow

```
Agent does work on a worktree
  -> window:  WIS-olive:dev
  -> opens PR:  gh pr create --label wip --title "..." --body "..."
  -> pushes branch
  -> post-push hook fires automatically
  -> task-master qa <worktree> <pr-number>  (non-blocking)
  -> SAME window renamed WIS-olive:qa, fresh opencode TUI starts with QA prompt

QA agent (up to 3 iterations):
  1. Self-reviews the diff
  2. Reads and resolves bot/reviewer comments
  3. Checks CI status; fixes failures
  4. Pushes fixes, waits 90s, re-checks

When clean:
  -> window renamed WIS-olive:review
  -> Posts "Ready for human review" comment on the PR
  -> Removes the 'wip' label
  -> opencode TUI stays open — human can read summary and give follow-up instructions

If stuck after 3 iterations:
  -> window renamed WIS-olive:blocked
  -> Posts escalation comment listing what needs human input
  -> Leaves 'wip' label on
  -> opencode TUI stays open — human can read what was tried and intervene

Human reviews and merges.
```

### Setup (one-time per machine)

**1. Ensure the `wip` label exists on each GitHub repo:**
```bash
gh label create wip --color E4E669 --description "Work in progress, QA agent running"
```

**2. Install post-push hooks into all registered worktrees:**
```bash
task-master install-qa-hooks
```

This is also done automatically when you run `task-master add-worktree`.

### Manual QA trigger

If you want to trigger the QA agent manually (e.g. for a PR that already exists):
```bash
task-master qa <worktree> <pr-number>
# Example:
task-master qa WIS-olive 42
```

### Rules for agents opening PRs

- Always add `--label wip` when creating a PR:
  ```bash
  gh pr create --label wip --title "feat: add X" --body "..."
  ```
- Never remove the `wip` label yourself — the QA agent owns that.
- The QA agent will push `qa:` prefixed commits directly to your branch; do not rebase while it is running.

### QA agent tmux window lifecycle

Each worktree window has a single lifecycle — no separate QA window is created:

```
WIS-olive:dev     <- agent works here
WIS-olive:qa      <- QA agent runs here (same window, fresh opencode session)
WIS-olive:review  <- QA complete, opencode TUI stays open for human review
WIS-olive:blocked <- QA escalated, opencode TUI stays open for human intervention
```

When making follow-up instructions after QA completes, just type in the window directly.
