use crate::registry::Registry;
use crate::slug;
use crate::tmux;
use crate::worktree;
use anyhow::{bail, Context, Result};
use std::path::PathBuf;
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

/// Build the spawn prompt for an ephemeral worktree.
///
/// Like `build_spawn_prompt` but tells the agent its branch is already created
/// and that the worktree is ephemeral (will be cleaned up after the PR merges).
pub fn build_ephemeral_spawn_prompt(
    window_name: &str,
    branch_name: &str,
    user_prompt: &str,
) -> String {
    let base_name = tmux::base_window_name(window_name);
    format!(
        "{user_prompt}

## Starting point

This is an ephemeral worktree. Your branch `{branch_name}` has already been created.
You do NOT need to run `git checkout -b` — start committing directly on this branch.

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
your session before the command can return. Always use `task-master notify` instead.

Note: this worktree is ephemeral. It will be automatically removed after its PR is
merged or closed via `task-master cleanup --merged`.",
        user_prompt = user_prompt,
        branch_name = branch_name,
        base_name = base_name,
    )
}

// ---------------------------------------------------------------------------
// Command — normal spawn
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

// ---------------------------------------------------------------------------
// Command — ephemeral spawn
// ---------------------------------------------------------------------------

/// Spawn an agent in a newly-created ephemeral worktree.
///
/// `project_short` must be a project short name (e.g. "MSG"), not a full window name.
/// A unique worktree name and branch are generated automatically using the project's
/// configured `ephemeral_prefix` and `ephemeral_branch_prefix` (or sensible defaults).
///
/// The new worktree is registered in task-master.toml with `ephemeral = true` so that
/// `task-master cleanup --merged` can remove it once its branch is merged.
pub fn cmd_spawn_ephemeral(
    registry: &Registry,
    base_dir: &PathBuf,
    project_short: &str,
    prompt: &str,
) -> Result<String> {
    // Resolve the project to get its ephemeral config fields.
    let project = registry.find_project(project_short).with_context(|| {
        format!(
            "Project '{}' not found. Run `task-master list` to see available projects.",
            project_short
        )
    })?;

    let branch_prefix = project
        .ephemeral_branch_prefix
        .as_deref()
        .unwrap_or("feat/");

    // Generate a unique slug. Retry once on the (astronomically unlikely) collision.
    let (worktree_name, window_name) = {
        let slug1 = slug::generate_slug(project.ephemeral_prefix.as_deref());
        let candidate1 = format!("{}-{}", project.short, slug1);
        if registry.find_worktree(&candidate1).is_none() {
            (slug1, candidate1)
        } else {
            let slug2 = slug::generate_slug(project.ephemeral_prefix.as_deref());
            let candidate2 = format!("{}-{}", project.short, slug2);
            if registry.find_worktree(&candidate2).is_none() {
                (slug2, candidate2)
            } else {
                bail!(
                    "Could not generate a unique worktree name for project '{}' after 2 attempts. \
                     Try again or pick a name manually with `task-master add-worktree`.",
                    project_short
                );
            }
        }
    };

    let branch_name = format!("{}{}", branch_prefix, worktree_name);

    info!(
        "[ephemeral] Creating worktree '{}' on branch '{}' for project '{}'",
        window_name, branch_name, project_short
    );

    // Create the worktree, register in TOML, install hooks etc.
    let (resolved_window_name, abs_path) = worktree::create_ephemeral_worktree(
        registry,
        base_dir,
        project_short,
        &worktree_name,
        &branch_name,
    )?;

    // Reload the registry so the new worktree appears in it.
    let updated_registry = Registry::load(base_dir.clone())
        .context("Failed to reload registry after creating ephemeral worktree")?;
    let _ = updated_registry; // Used implicitly via the resolved paths above.

    let session = tmux::current_session()?;
    let abs_path_str = abs_path.to_string_lossy().to_string();
    let full_prompt = build_ephemeral_spawn_prompt(&resolved_window_name, &branch_name, prompt);
    let base_name = tmux::base_window_name(&resolved_window_name);

    // Ephemeral worktrees always get a fresh window — no pre-existing window expected.
    tmux::spawn_window(
        &session,
        &resolved_window_name,
        &abs_path_str,
        &full_prompt,
        None,
    )?;
    tmux::set_window_phase(&session, base_name, Some("dev"))?;

    Ok(format!(
        "Created ephemeral worktree '{}' (branch '{}').\nSpawned agent in '{}:dev'.",
        resolved_window_name, branch_name, base_name
    ))
}
