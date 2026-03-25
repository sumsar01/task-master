# task-master

AI agent orchestrator for tmux worktrees. Spawns and supervises [opencode](https://opencode.ai) agents across multiple git worktrees, with a built-in QA loop, planning agent, and e2e validation agent.

## Prerequisites

- [Rust](https://rustup.rs) (to build)
- [tmux](https://github.com/tmux/tmux)
- [opencode](https://opencode.ai) CLI
- [gh](https://cli.github.com) (GitHub CLI, for QA/PR integration)
- [bd (beads)](https://github.com/anomalyco/beads) (issue tracker used by agents)

## Build

```bash
cargo build --release
# binary at ./target/release/task-master
```

## Setup

### 1. Configure your projects

Copy the example config and fill in your own values:

```bash
cp task-master.example.toml task-master.toml
```

`task-master.toml` is gitignored — your project and worktree names stay local.

The config maps each project to a directory under `projects/` (also gitignored) and defines named worktrees:

```toml
[[projects]]
name = "my-service"
short = "SVC"          # used as tmux window prefix: SVC-main, SVC-feature, …
repo = "projects/my-service"

  [[projects.worktrees]]
  name = "main"

  [[projects.worktrees]]
  name = "feature"
```

### 2. Add projects and worktrees

```bash
# Clone a bare repo and register it
task-master add-project

# Add a new worktree to an existing project
task-master add-worktree

# List everything
task-master list
```

### 3. Install QA hooks (one-time)

```bash
task-master install-qa-hooks
```

This installs a `post-push` git hook in each worktree that notifies the supervisor when a branch is pushed.

## Core workflow

```bash
# Spawn a dev agent in a worktree
task-master spawn SVC-main "implement the login flow"

# Check live status of all worktrees
task-master status

# View token usage and cost
task-master stats
```

## Agent lifecycle

```
SVC-main           ← idle
SVC-main:dev       ← dev agent running
SVC-main:qa        ← QA agent running (triggered automatically on git push)
SVC-main:review    ← QA passed, awaiting human review
SVC-main:blocked   ← QA escalated, needs human input
```

## PR workflow (for agents)

Agents must follow this 3-step sequence when opening a PR:

```bash
git push origin HEAD
gh pr create --no-push --label wip --title "feat: ..." --body "..."
task-master notify <worktree> <pr-number>
```

`task-master notify` wakes the supervisor which spawns the QA agent. Never call `task-master qa` directly from inside an agent session — it replaces the running process.

## Supervisor

The supervisor monitors all worktree windows and drives phase transitions:

```bash
task-master supervise
```

Runs as a polling loop (every 5 minutes) in its own tmux window. Detects stalled agents, triggers QA when PRs are pushed, and posts PR comments when ready for review.

## Planning agent

For complex tasks, decompose work into [beads](https://github.com/anomalyco/beads) issues before spawning a dev agent:

```bash
task-master plan SVC-main "add webhook retry with exponential backoff"
```

The planning agent explores the codebase, asks clarifying questions, and creates a dependency graph of issues. When done the window is renamed to `:ready`.

## E2e agent

Post-deploy validation against a live environment:

```bash
task-master e2e SVC-main <pr-number>
```

Up to 3 fix+redeploy cycles before escalating.

## Environment

| Variable | Description |
|---|---|
| `TASK_MASTER_DIR` | Override the base directory (default: current directory) |

## License

MIT
