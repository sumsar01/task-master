use crate::registry::Registry;
use crate::tmux;
use crate::worktree;
use anyhow::Result;
use tracing::info;

// ---------------------------------------------------------------------------
// Prompt builder
// ---------------------------------------------------------------------------

/// Build the full prompt passed to the opencode spawn session.
///
/// Wraps the user-provided task description with mandatory PR workflow steps
/// so every spawned agent knows how to open a PR and notify the supervisor.
pub fn build_spawn_prompt(window_name: &str, user_prompt: &str) -> String {
    let base_name = tmux::base_window_name(window_name);
    format!(
        "{user_prompt}

## Starting point

Your worktree has been reset to master. Create a new branch before making any changes:
  git checkout -b feat/<short-description>

## PR workflow (MANDATORY)

When you are ready to open a PR, you MUST follow these steps in order:

1. Push the branch explicitly:
   git push origin HEAD

2. Open the PR with --no-push and --label wip:
   gh pr create --no-push --label wip --title \"<title>\" --body \"<body>\"

3. Read the PR number from the URL printed by gh pr create, then notify the supervisor:
   task-master notify {base_name} <pr-number>

NEVER use `gh pr create` without `--no-push` — it pushes via the GitHub API and
bypasses the git post-push hook, so QA will never start automatically.
NEVER call `task-master qa` directly — it replaces the running process and will kill
your session before the command can return. Always use `task-master notify` instead.",
        user_prompt = user_prompt,
        base_name = base_name,
    )
}

// ---------------------------------------------------------------------------
// Command
// ---------------------------------------------------------------------------

pub fn cmd_spawn(
    registry: &Registry,
    window_name: &str,
    prompt: &str,
    force: bool,
) -> Result<String> {
    let worktree = registry.require_worktree(window_name)?;

    worktree::reset_worktree_to_master(&worktree.abs_path, force)?;

    let session = tmux::current_session()?;
    let abs_path_str = worktree.abs_path.to_string_lossy().to_string();
    let prompt_owned = build_spawn_prompt(window_name, prompt);
    let prompt = prompt_owned.as_str();

    info!(
        "[{}] Spawning in session '{}', dir {}",
        window_name, session, abs_path_str
    );

    let base_name = tmux::base_window_name(window_name);

    let is_new = if tmux::find_window_index(&session, base_name).is_none() {
        // No window yet — create it fresh.
        tmux::spawn_window(&session, window_name, &abs_path_str, prompt, None)?;
        true
    } else {
        // Window already exists (possibly in :plan, :qa, :review, or :dev phase).
        // Always replace the running process with a fresh opencode dev session so
        // we don't accidentally send prompts into a plan/qa agent's chat input.
        tmux::set_window_phase(&session, base_name, Some("dev"))?;
        tmux::replace_window_process(&session, base_name, &abs_path_str, prompt, None)?;
        false
    };

    // Ensure :dev phase on new windows too (spawn_window sets it but be explicit).
    tmux::set_window_phase(&session, base_name, Some("dev"))?;

    let msg = if is_new {
        format!("Spawned '{}:dev' in a new window.", base_name)
    } else {
        format!(
            "Replaced existing '{}' window with fresh dev session (now '{}:dev').",
            base_name, base_name
        )
    };
    Ok(msg)
}
