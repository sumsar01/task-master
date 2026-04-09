use crate::registry::Registry;
use crate::tmux;
use anyhow::Result;
use std::path::Path;
use tracing::info;

/// Build the inline planning prompt passed to the opencode agent via `--prompt`.
///
/// The `.opencode/agents/plan.md` file (present in each target worktree) provides
/// the agent's *system prompt* and metadata (model, permissions, mode) — opencode
/// reads that file directly when started with `--agent plan`.  That file must NOT
/// contain task-specific placeholders because opencode uses it verbatim.
///
/// This function builds the *user prompt* (`--prompt` argument) that carries the
/// actual task description and all phase-by-phase instructions.  It is always
/// rendered inline from the built-in string so there is no ambiguity between the
/// agent system-prompt file and the per-invocation task content.
///
/// `session` and `window_base` are injected so the agent can rename its own
/// tmux window without any external tooling, using the same awk-based pattern
/// as the QA agent.
pub fn build_plan_prompt(_base_dir: &Path, task: &str, session: &str, window_base: &str) -> String {
    // Awk-based rename command that avoids colon-in-target tmux ambiguity.
    let rename_cmd = tmux::build_rename_cmd(session, window_base);
    let ready_rename = format!("{} '{base}:ready'", rename_cmd, base = window_base);

    format!(
        "You are a planning agent. Your ONLY job is to analyse the codebase, ask any \
         clarifying questions, then decompose the following task into a clear written plan \
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
         2. Document your assumptions in the plan. Use phrasing like:\n\
            \"Assuming X because Y — revisit if Z.\"\n\
         \n\
         3. Only use the `question` tool when ALL of the following are true:\n\
            - The answer would fundamentally change the scope or architecture of the plan\n\
            - You cannot make a reasonable assumption from the codebase\n\
            - Getting it wrong would require discarding most of the work\n\
         \n\
         For everything else — naming, ordering, minor design choices, edge-case handling —\n\
         make a call and document it.\n\
         \n\
         PHASE 3 — Write the plan\n\
         \n\
         Break the task down into concrete, independently-workable tasks. For each task write:\n\
         \n\
         - Title — short and action-oriented (\"Add X\", \"Refactor Y\", \"Fix Z\")\n\
         - Description — the WHY and the WHAT; what needs to be done and why it exists\n\
         - Type — feature | task | bug | chore\n\
         - Priority — 0=critical, 1=high, 2=medium, 3=low, 4=backlog\n\
         - Depends on — list any tasks from this plan that must be done first (or \"none\")\n\
         \n\
         Each task must be completable by a single dev agent in one session.\n\
         If you discover incidental issues unrelated to this task, list them at the end\n\
         under a \"Discovered\" section with a note on why they were found.\n\
         \n\
         PHASE 4 — Signal completion\n\
         \n\
         Once the plan is written, rename this window to signal it is ready:\n\
         \n\
         {ready_rename}\n\
         \n\
         Then print a brief summary:\n\
         - How many tasks are in the plan\n\
         - Which tasks have no blockers (starting points)\n\
         - Any assumptions or open questions noted in the plan\n\
         \n\
         IMPORTANT RULES\n\
         - Do NOT modify any source files.\n\
         - Do NOT open PRs or create branches.\n\
         - Do NOT start implementing — only plan.",
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

    let prompt = build_plan_prompt(&registry.base_dir, task, &session, &base_name);

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
    use std::path::Path;

    fn no_template() -> &'static Path {
        Path::new("/tmp/no-such-dir-for-plan-tests")
    }

    #[test]
    fn test_build_plan_prompt_contains_task() {
        let prompt = build_plan_prompt(
            no_template(),
            "implement OAuth login",
            "mysession",
            "WIS-olive",
        );
        assert!(prompt.contains("implement OAuth login"));
    }

    #[test]
    fn test_build_plan_prompt_contains_ready_rename() {
        let prompt = build_plan_prompt(no_template(), "some task", "my-session", "WIS-olive");
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
        let prompt = build_plan_prompt(no_template(), "add feature", "s", "W-w");
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
    fn test_build_plan_prompt_plan_structure_present() {
        let prompt = build_plan_prompt(no_template(), "task", "s", "W-w");
        assert!(
            prompt.contains("Write the plan"),
            "prompt must include plan-writing phase"
        );
        assert!(
            prompt.contains("Priority"),
            "prompt must include priority field"
        );
        assert!(
            prompt.contains("Depends on"),
            "prompt must include dependency field"
        );
        assert!(
            !prompt.contains("bd create"),
            "prompt must not include bd create commands"
        );
    }

    #[test]
    fn test_build_plan_prompt_rename_uses_awk_pattern() {
        // Must use the index-based awk rename to avoid colon-in-target ambiguity.
        let prompt = build_plan_prompt(no_template(), "task", "sess", "PROJ-branch");
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
        let prompt = build_plan_prompt(no_template(), "complex task", "s", "W-w");
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
        let a = build_plan_prompt(no_template(), "implement OAuth", "sess", "WIS-olive");
        let b = build_plan_prompt(no_template(), "implement OAuth", "sess", "WIS-olive");
        assert_eq!(a, b, "build_plan_prompt must be deterministic");
    }
}
