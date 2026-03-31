use crate::registry::Registry;
use crate::tmux;
use anyhow::Result;
use tracing::info;

/// Build the inline planning prompt passed to the opencode agent.
///
/// The agent is instructed to:
///   1. Explore the codebase and understand the existing architecture.
///   2. Ask clarifying questions using opencode's native question tool.
///   3. Decompose the task into concrete beads issues (`bd create`, `bd dep add`).
///   4. Rename the window to `:ready` when done.
///
/// `session` and `window_base` are injected so the agent can rename its own
/// tmux window without any external tooling, using the same awk-based pattern
/// as the QA agent.
pub fn build_plan_prompt(task: &str, session: &str, window_base: &str) -> String {
    // Awk-based rename command that avoids colon-in-target tmux ambiguity.
    let rename_cmd = tmux::build_rename_cmd(session, window_base);
    let ready_rename = format!("{} '{base}:ready'", rename_cmd, base = window_base);

    format!(
        "You are a planning agent. Your ONLY job is to analyse the codebase, ask any \
         clarifying questions, then decompose the following task into a set of beads issues \
         ready for dev agents to pick up. You must NOT write any code or modify any source \
         files.\n\
         \n\
         TASK\n\
         {task}\n\
         \n\
         PHASE 1 — Understand the codebase\n\
         \n\
         Read the relevant parts of the repo. Focus on:\n\
         - Existing architecture and conventions\n\
         - Files, modules, or systems the task will touch\n\
         - Anything that might constrain the implementation\n\
         \n\
         PHASE 2 — Resolve open questions\n\
          \n\
          For any open question about how to approach the task:\n\
          \n\
          1. First, answer it yourself using what you found in Phase 1. Most implementation\n\
             details, naming choices, and design patterns can be decided by reading the\n\
             existing code and following its conventions.\n\
          \n\
          2. Document your assumptions in the relevant issue descriptions (--description),\n\
             not in separate files. Use phrasing like: \"Assuming X because Y — revisit if Z.\"\n\
          \n\
          3. Only use the `question` tool when ALL of the following are true:\n\
             - The answer would fundamentally change the scope or architecture of the plan\n\
             - You cannot make a reasonable assumption from the codebase\n\
             - Getting it wrong would require discarding most of the work\n\
          \n\
          For everything else — naming, ordering, minor design choices, edge-case handling —\n\
          make a call and document it in the issue description.\n\
         \n\
         PHASE 3 — Create beads issues\n\
         \n\
         Break the task down into concrete, independently-workable issues. For each issue:\n\
         \n\
         ```bash\n\
         bd create \"<title>\" \\\n\
           --description=\"<why this issue exists and exactly what needs to be done>\" \\\n\
           --type=feature|task|bug|chore \\\n\
           --priority=0-4 \\\n\
           --json\n\
         ```\n\
         \n\
         Guidelines for good issues:\n\
         - Title is short and action-oriented (\"Add X\", \"Refactor Y\", \"Fix Z\")\n\
         - Description explains the WHY and the WHAT, not just restates the title\n\
         - Each issue is completable by a single dev agent in one session\n\
         - Priority reflects actual urgency: 0=critical, 1=high, 2=medium, 3=low, 4=backlog\n\
         \n\
         PHASE 4 — Wire up dependencies\n\
         \n\
         For any issue that must be completed before another can start:\n\
         ```bash\n\
         bd dep add <blocked-issue-id> <blocking-issue-id>   # blocked depends on blocking\n\
         ```\n\
         \n\
         After wiring deps, verify the graph looks correct:\n\
         ```bash\n\
         bd ready --json   # shows unblocked issues — your starting points\n\
         bd blocked --json # confirms blocked issues have correct deps\n\
         ```\n\
         \n\
         PHASE 5 — Signal completion\n\
         \n\
         Once all issues are created and deps wired, rename this window to signal the plan\n\
         is ready for a dev agent:\n\
         \n\
         {ready_rename}\n\
         \n\
         Then print a brief summary:\n\
         - How many issues were created\n\
         - Which issues are immediately ready (no blockers)\n\
         - Any open questions or assumptions you documented in issue descriptions\n\
         \n\
         IMPORTANT RULES\n\
         - Do NOT modify any source files.\n\
         - Do NOT open PRs or create branches.\n\
         - Do NOT start implementing — only plan.\n\
         - Use `bd create` for ALL task tracking; do not create markdown todo lists.\n\
         - If you discover issues unrelated to this task while exploring, create them with\n\
           `--deps discovered-from:<nearest-relevant-issue-id>` so they are linked.\n\
         - bd CLI is available. Use `bd --help` if you need to check command syntax.",
        task = task,
        ready_rename = ready_rename,
    )
}

/// Spawn a planning agent for the given worktree and task description.
///
/// Always starts a fresh opencode session using `--agent plan`:
/// - If a window for the worktree already exists: kills the current process via
///   replace_window_process and starts a fresh plan-mode session. The prompt is
///   sent exactly once by replace_window_process.
/// - If no window exists: creates a new one named `<base>:dev` via spawn_window
///   (which sends the opencode command once), then immediately renames it to :plan.
///
/// Note: spawn_window is NOT called when a window already exists, to avoid the
/// double-send bug where spawn_window injects the prompt into the live process
/// and replace_window_process injects it again after C-c.
pub fn cmd_plan(registry: &Registry, worktree_name: &str, task: &str) -> Result<String> {
    let worktree = registry.require_worktree(worktree_name)?;

    let session = tmux::current_session()?;
    let base_name = tmux::base_window_name(worktree_name).to_string();
    let abs_path_str = worktree.abs_path.to_string_lossy().to_string();

    let prompt = build_plan_prompt(task, &session, &base_name);

    info!(
        "[{}] Starting planning agent in session '{}'",
        base_name, session
    );

    let window_exists = tmux::find_window_index(&session, &base_name).is_some();

    let msg = if window_exists {
        // Rename first so the window reflects :plan before we kill+replace.
        tmux::set_window_phase(&session, &base_name, Some("plan"))?;
        // replace_window_process sends the prompt exactly once (after C-c).
        tmux::replace_window_process(&session, &base_name, &abs_path_str, &prompt, Some("plan"))?;
        format!(
            "Planning agent started in existing window '{}:plan'.",
            base_name
        )
    } else {
        // spawn_window creates the window (named :dev) and sends the opencode
        // command exactly once.
        tmux::spawn_window(&session, &base_name, &abs_path_str, &prompt, Some("plan"))?;
        // Immediately rename :dev -> :plan.
        tmux::set_window_phase(&session, &base_name, Some("plan"))?;
        format!("Planning agent started in new window '{}:plan'.", base_name)
    };

    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_plan_prompt_contains_task() {
        let prompt = build_plan_prompt("implement OAuth login", "mysession", "WIS-olive");
        assert!(prompt.contains("implement OAuth login"));
    }

    #[test]
    fn test_build_plan_prompt_contains_ready_rename() {
        let prompt = build_plan_prompt("some task", "my-session", "WIS-olive");
        assert!(
            prompt.contains("WIS-olive:ready"),
            "prompt should reference ready phase rename"
        );
        assert!(
            prompt.contains("my-session"),
            "prompt should reference the session name"
        );
    }

    #[test]
    fn test_build_plan_prompt_no_code_changes() {
        let prompt = build_plan_prompt("add feature", "s", "W-w");
        assert!(
            prompt.contains("Do NOT modify any source files"),
            "prompt must forbid code changes"
        );
        assert!(
            prompt.contains("Do NOT open PRs"),
            "prompt must forbid opening PRs"
        );
    }

    #[test]
    fn test_build_plan_prompt_bd_commands_present() {
        let prompt = build_plan_prompt("task", "s", "W-w");
        assert!(
            prompt.contains("bd create"),
            "prompt must include bd create"
        );
        assert!(
            prompt.contains("bd dep add"),
            "prompt must include bd dep add"
        );
        assert!(
            prompt.contains("bd ready"),
            "prompt must include bd ready verification"
        );
    }

    #[test]
    fn test_build_plan_prompt_rename_uses_awk_pattern() {
        // Must use the index-based awk rename to avoid colon-in-target ambiguity.
        let prompt = build_plan_prompt("task", "sess", "PROJ-branch");
        assert!(prompt.contains("awk"), "should use awk-based rename");
        assert!(
            prompt.contains("PROJ-branch"),
            "should reference the window base name"
        );
        assert!(
            prompt.contains("PROJ-branch:ready"),
            "should rename to :ready phase"
        );
    }

    #[test]
    fn test_build_plan_prompt_question_tool_mentioned() {
        let prompt = build_plan_prompt("complex task", "s", "W-w");
        assert!(
            prompt.contains("question"),
            "prompt should mention opencode question tool for critical escalations"
        );
        assert!(
            prompt.contains("fundamentally change the scope"),
            "prompt should restrict question tool to scope-changing decisions only"
        );
        assert!(
            prompt.contains("Document your assumptions"),
            "prompt should instruct agent to self-answer and document assumptions"
        );
    }

    #[test]
    fn test_build_plan_prompt_is_deterministic() {
        // Calling build_plan_prompt twice with the same args must produce the same output.
        // This guards against any accidental stateful/random content.
        let a = build_plan_prompt("implement OAuth", "sess", "WIS-olive");
        let b = build_plan_prompt("implement OAuth", "sess", "WIS-olive");
        assert_eq!(a, b, "build_plan_prompt must be deterministic");
    }
}
