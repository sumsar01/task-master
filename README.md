# task-master

**AI agent orchestrator for tmux worktrees.**

task-master lets you run multiple [opencode](https://opencode.ai) AI agents in parallel, each isolated in its own git worktree and tmux window. It handles the full lifecycle: spawning dev agents, automated QA, planning, post-deploy validation, and a supervisor that keeps everything on track.

---

## How it works

Each project is cloned as a **bare repo** with one or more **worktrees** — lightweight checkouts that share the same git objects but each have their own working directory, branch, and process space. An agent gets a dedicated tmux window per worktree and can work independently without interfering with other agents.

```
task-master/
├── task-master.toml         # your local config (gitignored)
└── projects/
    └── my-service/          # bare repo
        ├── main/            # worktree  →  tmux window: SVC-main
        ├── feature-a/       # worktree  →  tmux window: SVC-feature-a
        └── bugfix/          # worktree  →  tmux window: SVC-bugfix
```

The **supervisor** runs in its own tmux window and polls every 5 minutes. It detects when agents finish, triggers the QA agent automatically when a branch is pushed, and posts PR comments when work is ready for human review.

---

## Prerequisites

- [Rust](https://rustup.rs) (to build)
- [tmux](https://github.com/tmux/tmux)
- [opencode](https://opencode.ai) CLI
- [gh](https://cli.github.com) (GitHub CLI, for QA/PR integration)
- [bd (beads)](https://github.com/anomalyco/beads) (issue tracker used by agents)

---

## Install

```bash
git clone https://github.com/sumsar01/task-master
cd task-master
cargo build --release

# Optionally put the binary on your PATH
ln -s "$PWD/target/release/task-master" /usr/local/bin/task-master
```

---

## Setup

### 1. Configure your projects

Copy the example config and fill in your values:

```bash
cp task-master.example.toml task-master.toml
```

`task-master.toml` is gitignored — your project and worktree names stay local.

```toml
[ui]
theme = "tokyonight"   # TUI color theme (see TUI section for available themes)

[[projects]]
name = "my-service"
short = "SVC"          # tmux window prefix: SVC-main, SVC-feature, …
repo = "projects/my-service"
group = "Work"         # optional: super-group label in the TUI list
context = "payments"   # optional: bounded-context tag (for future filtering)
git_name = "Your Name" # optional: override git user.name for this project's worktrees
git_email = "you@example.com"  # optional: override git user.email

  [[projects.worktrees]]
  name = "main"

  [[projects.worktrees]]
  name = "feature"
```

**All config fields:**

| Field | Required | Description |
|---|---|---|
| `name` | yes | Full repo/directory name under `projects/` |
| `short` | yes | Short prefix for tmux window names (e.g. `SVC` → `SVC-main`) |
| `repo` | yes | Path relative to this file where the bare repo lives |
| `group` | no | Super-group label for the TUI list; projects sharing a group are shown under a collapsible header |
| `context` | no | Bounded-context tag (stored for future filtering) |
| `git_name` | no | Git `user.name` to use for commits in this project's worktrees |
| `git_email` | no | Git `user.email` to use for commits in this project's worktrees |
| `ephemeral_prefix` | no | Word prefix for auto-generated ephemeral worktree names (e.g. `spruce` → `SVC-spruce-7f3a`) |
| `ephemeral_branch_prefix` | no | Branch prefix for ephemeral branches (default: `feat/`) |
| `collapsed` | no | TUI collapse state — written automatically when you collapse/expand |

Set `git_name`/`git_email` when the worktrees live under a path that triggers an unintended `includeIf` rule in `~/.gitconfig`. Run `task-master fix-git-identity` once to apply these values to existing bare repos.

### 2. Clone a project and add worktrees

```bash
# Clone a repo as a bare repo and register it
task-master add-project my-service SVC https://github.com/org/my-service

# Add a worktree (checked out to a branch)
task-master add-worktree SVC feature-a --branch feat/new-thing

# List everything
task-master list
```

### 3. Install QA hooks (one-time per machine)

```bash
task-master install-qa-hooks
```

Installs a `post-push` git hook in every worktree. When an agent pushes a branch, the hook fires and notifies the supervisor to start QA automatically.

### 4. Install agent configs (one-time per machine)

```bash
task-master install-agent-configs
```

Copies `plan.md`, `qa.md`, and `e2e.md` from the task-master project into each worktree's `.opencode/agents/` directory so that `opencode --agent plan/qa/e2e` works when running inside those worktrees. New worktrees added with `add-worktree` receive these configs automatically.

### 5. Start the supervisor

```bash
task-master supervise
```

This opens a `supervisor` tmux window that polls every 5 minutes, detecting stalled agents and driving phase transitions.

---

## Core workflow

### Spawn a dev agent

```bash
task-master spawn SVC-main "implement the login flow"
```

The agent gets a fresh tmux window (`SVC-main:dev`), a reset worktree, and the prompt. It owns everything from there: creating issues in beads, branching, committing, opening a PR, and notifying the supervisor.

### Ephemeral worktrees

For most work, use `--ephemeral` instead of managing worktree names manually. task-master auto-creates a uniquely-named worktree, runs the agent in it, and marks it for cleanup once the branch is merged:

```bash
# Pass the project short name — a name like SVC-spruce-7f3a is generated automatically
task-master spawn --ephemeral SVC "implement the login flow"

# Clean up all ephemeral worktrees whose branch is merged or PR is closed
task-master cleanup --merged
```

The generated name prefix can be customised with `ephemeral_prefix` in `task-master.toml`.

### Monitor everything

```bash
task-master status    # live tmux phase for every worktree
task-master stats     # token usage and cost, optionally --days N
task-master tui       # interactive TUI dashboard
```

---

## TUI

```bash
task-master tui
```

Opens an interactive dashboard showing all projects and worktrees with their live tmux phase. Projects can be grouped and collapsed. The TUI writes config changes (theme, collapsed state) back to `task-master.toml` automatically.

**Keybindings:**

| Key | Action |
|---|---|
| `j` / `k` | Navigate down / up |
| `Enter` / `Space` | Collapse/expand group or project |
| `s` | Spawn a dev agent in the selected worktree |
| `p` | Spawn a planning agent in the selected worktree |
| `P` | Add a new project |
| `N` | Add a new worktree to the selected project |
| `T` | Open theme picker (live preview, `Esc` to cancel) |
| `?` | Show help |
| `q` | Quit |

**Themes:** `tokyonight` (default), `gruvbox`, `catppuccin`, `nord`, `dracula`, `rosepine`, `github_light`, `catppuccin_latte`, `rosepine_dawn`, `gruvbox_light`, `solarized_light`. Press `T` to pick interactively; the choice is saved to `[ui] theme` in `task-master.toml`.

---

## Agent lifecycle

Each worktree window moves through phases as work progresses:

```
SVC-main              ← idle
SVC-main:dev          ← dev agent running
SVC-main:qa           ← QA agent running (auto-triggered on git push)
SVC-main:review       ← QA passed, awaiting human review
SVC-main:blocked      ← QA escalated, needs human input
SVC-main:dev-stalled  ← dev agent exited unexpectedly (supervisor detected)
SVC-main:qa-stalled   ← QA agent exited, PR state unknown
```

Terminal states (`:review`, `:blocked`, `:*-stalled`) are human-facing. Type directly in the window or run `task-master reset SVC-main` to return to idle.

---

## Agents

### Dev agent

Spawned by `task-master spawn`. Receives your prompt, explores the codebase, creates [beads](https://github.com/anomalyco/beads) issues for tracking, implements the work, and opens a PR.

**PR protocol agents must follow** (3 steps, in order):

```bash
git push origin HEAD
gh pr create --no-push --label wip --title "feat: ..." --body "..."
task-master notify SVC-main <pr-number>
```

`task-master notify` is safe to call from inside an agent — it writes a wake stamp that the supervisor picks up within ~2 seconds. **Never call `task-master qa` from inside an agent** — it replaces the running process.

### QA agent

Auto-triggered by the supervisor after a push. Up to 3 iterations of:

1. Self-reviews the diff
2. Reads and resolves bot/reviewer comments
3. Checks CI status; fixes failures
4. Pushes fixes, waits, re-checks

On success: renames window to `:review`, removes `wip` label, posts PR comment.  
On failure after 3 iterations: renames to `:blocked`, posts escalation comment.

Can also be triggered manually (humans only):

```bash
task-master qa SVC-main 42
```

### Planning agent

For complex tasks, decompose work into beads issues before spawning a dev agent:

```bash
task-master plan SVC-main "add webhook retry with exponential backoff"
```

The planning agent reads the codebase, asks clarifying questions in the opencode UI, and creates a dependency graph of issues. Window is renamed to `:ready` when done.

```
SVC-main:plan    ← planning agent running
SVC-main:ready   ← issues created, ready for dev agent
```

Kick off dev work from the `:ready` window:

```bash
task-master spawn SVC-main "start on the first ready issue"
```

### E2e agent

Post-deploy validation against a live environment:

```bash
task-master e2e SVC-main 42
```

Reads the PR diff, generates a validation plan specific to the changes, executes it, and can fix + redeploy up to 3 times before escalating.

```
SVC-main:e2e         ← validation running
SVC-main:e2e-done    ← passed
SVC-main:e2e-blocked ← escalated, needs human input
```

### Supervisor

Monitors all worktree windows every 5 minutes:

```bash
task-master supervise
```

- Detects stalled agents (opencode exited without renaming the window)
- Respawns QA when notified via `task-master notify`
- Posts PR comments on state transitions
- Never modifies code — read-only except for window renames and PR comments

---

## Command reference

| Command | Description |
|---|---|
| `spawn <worktree> <prompt>` | Spawn a dev agent (use `--ephemeral` with project short name to auto-create a named worktree; `--force` to discard uncommitted changes) |
| `plan <worktree> <prompt>` | Spawn a planning agent |
| `qa <worktree> [pr]` | Spawn a QA agent (humans only) |
| `notify <worktree> <pr>` | Notify supervisor a PR is ready (agent-safe) |
| `e2e <worktree> <pr>` | Spawn an e2e validation agent |
| `send <worktree> <prompt>` | Send a prompt to the running opencode session without resetting the worktree |
| `supervise` | Start the supervisor polling loop |
| `status` | Live phase status of all worktrees |
| `stats [--days N]` | Token usage and cost |
| `tui` | Open the interactive TUI dashboard |
| `list` | List all projects and worktrees |
| `add-project <name> <short> <url>` | Clone a bare repo and register it |
| `add-worktree <project> <name> [--branch]` | Add a worktree |
| `remove-worktree <worktree> [--force] [--keep-branch]` | Remove a worktree (omit `--keep-branch` to also delete the remote branch) |
| `cleanup [--merged] [--all] [--force]` | Remove ephemeral worktrees whose branch is merged or PR is closed |
| `install-qa-hooks` | Install post-push hooks in all worktrees |
| `install-agent-configs` | Copy agent configs (plan/qa/e2e) into all worktrees |
| `reset <worktree>` | Clear a window's phase suffix back to idle |
| `fix-git-identity` | Apply per-project `git_name`/`git_email` overrides to all bare repos |

---

## Environment

| Variable | Description |
|---|---|
| `TASK_MASTER_DIR` | Override the base directory (default: current directory) |

---

## License

MIT
