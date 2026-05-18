use crate::registry::Registry;
use crate::status::find_live_phase;
use crate::tmux;
use anyhow::Result;
use tracing::info;

// ---------------------------------------------------------------------------
// Registry / status snapshot
// ---------------------------------------------------------------------------

/// Build a human-readable snapshot of all registered projects and their
/// current worktree phases.  Injected into the orchestrator prompt at spawn
/// time so the agent has full situational awareness without needing to run
/// any discovery commands first.
///
/// Format example:
/// ```
/// PROJECT         SHORT  WORKTREE             PHASE
/// --------------- -----  -------------------- -------
/// warehouse-svc   WIS    WIS-olive            dev
///                        WIS-cedar            idle
/// msg-service     MSG    MSG-main             idle
/// ```
pub fn format_registry_snapshot(registry: &Registry) -> String {
    let session_opt = tmux::current_session().ok();

    let mut lines = Vec::new();
    lines.push(format!(
        "{:<30}  {:<6}  {:<22}  {:<12}  {}",
        "PROJECT", "SHORT", "WORKTREE", "GH_ACCOUNT", "PHASE"
    ));
    lines.push(format!(
        "{:-<30}  {:-<6}  {:-<22}  {:-<12}  {:-<7}",
        "", "", "", "", ""
    ));

    for project in &registry.projects {
        let worktrees: Vec<_> = registry
            .worktrees
            .iter()
            .filter(|w| w.project_short == project.short)
            .collect();

        let gh_account_col = project.gh_account.as_deref().unwrap_or("");

        if worktrees.is_empty() {
            lines.push(format!(
                "{:<30}  {:<6}  {:<22}  {:<12}  {}",
                project.name, project.short, "(no worktrees)", gh_account_col, ""
            ));
            continue;
        }

        for (i, wt) in worktrees.iter().enumerate() {
            let phase = match &session_opt {
                None => "?".to_string(),
                Some(session) => find_live_phase(session, &wt.window_name)
                    .unwrap_or_else(|| "idle".to_string()),
            };
            let project_col = if i == 0 { project.name.as_str() } else { "" };
            let short_col = if i == 0 { project.short.as_str() } else { "" };
            let account_col = if i == 0 { gh_account_col } else { "" };
            lines.push(format!(
                "{:<30}  {:<6}  {:<22}  {:<12}  {}",
                project_col, short_col, wt.window_name, account_col, phase
            ));
        }
    }

    lines.join("\n")
}

// ---------------------------------------------------------------------------
// Prompt builder
// ---------------------------------------------------------------------------

/// Build the full prompt passed to the orchestrator opencode session.
pub fn build_orchestrate_prompt(
    task: &str,
    session: &str,
    window_base: &str,
    registry_snapshot: &str,
) -> String {
    let rename_cmd = tmux::build_rename_cmd(session, window_base);
    let done_rename = format!("{} '{base}:done'", rename_cmd, base = window_base);
    let blocked_rename = format!("{} '{base}:blocked'", rename_cmd, base = window_base);

    format!(
        "You are an orchestrator agent. Your ONLY job is to decompose a cross-repo task, \
delegate sub-tasks to the right projects, and monitor progress. You must NOT write any code, \
open PRs, modify files, or create branches yourself.\n\
\n\
TASK\n\
{task}\n\
\n\
CURRENT STATE OF ALL PROJECTS\n\
\n\
The following snapshot was captured at the moment you were spawned. Worktrees with phase \
'idle' have no active agent and are available for work. Worktrees with phase 'dev', 'qa', \
'review', 'plan', or 'blocked' are occupied.\n\
\n\
{registry_snapshot}\n\
\n\
PHASE 1 — Understand the task and identify relevant projects\n\
\n\
Read the task description carefully. Based on the project names, short codes, groups, and \
context tags shown above, decide which projects are relevant. You do NOT need to explore \
the codebases yourself — that is the job of the sub-agents.\n\
\n\
PHASE 2 — Decompose into per-project sub-tasks\n\
\n\
Break the task into concrete sub-tasks, one or more per relevant project. For each sub-task:\n\
\n\
```bash\n\
bd create --title=\"<short title>\" \\\n\
  --description=\"## Project\\n<SHORT>\\n\\n## Why\\n<reason>\\n\\n## What\\n<what to do>\" \\\n\
  --type=feature|task|bug \\\n\
  --priority=0-4 \\\n\
  --json\n\
```\n\
\n\
Wire up any cross-project dependencies with:\n\
```bash\n\
bd dep add <blocked-id> <blocking-id>\n\
```\n\
\n\
PHASE 3 — Delegate sub-tasks to agents\n\
\n\
For each sub-task (in dependency order — unblocked ones first):\n\
\n\
1. Always spawn an ephemeral worktree for each sub-task:\n\
   ```bash\n\
   task-master spawn --ephemeral <SHORT> \"<sub-task prompt>\"\n\
   ```\n\
   Ephemeral worktrees are automatically cleaned up after their branch is merged,\n\
   keeping the workspace tidy without manual intervention.\n\
2. Mark the beads issue as claimed:\n\
   ```bash\n\
   bd update <id> --claim\n\
   ```\n\
\n\
The sub-task prompt you write should be a self-contained task description that the agent \n\
can act on immediately. Include the project context, what to implement, and any cross-repo \n\
coordination the agent should be aware of (e.g. 'Service B expects endpoint X from Service A').\n\
\n\
Note: the GH_CONFIG_DIR environment variable is automatically set for each spawned agent\n\
based on the gh_account configured for the project in task-master.toml. Agents do not need\n\
to run `gh auth switch` manually.\n\
\n\
PHASE 4 — Monitor and coordinate\n\
\n\
After delegating, monitor progress by periodically running:\n\
```bash\n\
task-master status\n\
```\n\
\n\
Watch for worktrees entering ':review' phase (PR ready for QA) or ':blocked' phase \n\
(agent needs help). When a sub-task PR is merged, close the corresponding beads issue:\n\
```bash\n\
bd close <id> --reason=\"PR merged\"\n\
```\n\
\n\
If a sub-agent is blocked and needs guidance, send it a follow-up prompt:\n\
```bash\n\
task-master send <WINDOW-NAME> \"<guidance>\"\n\
```\n\
\n\
PHASE 5 — Signal completion\n\
\n\
When ALL sub-tasks are complete (all beads issues closed), rename this window to :done:\n\
\n\
{done_rename}\n\
\n\
Then print a summary:\n\
- Which projects were involved\n\
- Which PRs were opened (window names and PR numbers if known)\n\
- Any assumptions or decisions made during delegation\n\
\n\
If you are blocked and need human input (e.g. a sub-agent is stuck and you cannot resolve it),\n\
rename this window to :blocked and explain what needs human attention:\n\
\n\
{blocked_rename}\n\
\n\
IMPORTANT RULES\n\
- Do NOT modify any source files, write code, or open PRs yourself.\n\
- Do NOT use `task-master qa` directly — sub-agents do that via `task-master notify`.\n\
- Use `bd` for ALL task tracking. Do not use markdown todo lists.\n\
- Always use `task-master spawn --ephemeral <SHORT>` — never spawn into named worktrees.\n\
- Ephemeral worktrees are cleaned up automatically after their branch is merged.\n\
- If the task only involves one project, you are still the right tool — delegate to that project.\n\
- If you are unsure which project owns something, err toward creating an issue and letting the\n\
  sub-agent figure it out from the codebase.\n\
- Use `task-master status` (not the snapshot above) when you need a live view during monitoring.",
        task = task,
        registry_snapshot = registry_snapshot,
        done_rename = done_rename,
        blocked_rename = blocked_rename,
    )
}

// ---------------------------------------------------------------------------
// Window naming helpers
// ---------------------------------------------------------------------------

/// Generate the tmux window base name for an orchestrator with the given label.
///
/// Label should be a short slug (max ~20 chars) derived from the first few
/// words of the task description.  Example: "implement-oauth-login".
pub fn orchestrate_window_name(label: &str) -> String {
    format!("orchestrate-{}", label)
}

/// Returns `true` when `name` looks like an orchestrator window base name
/// (i.e. starts with `"orchestrate-"`).
pub fn is_orchestrate_window(name: &str) -> bool {
    name.starts_with("orchestrate-")
}

/// Derive a short label from a task description: take the first 3 words,
/// lowercase, replace non-alphanumeric runs with `-`, truncate to 20 chars.
pub fn label_from_task(task: &str) -> String {
    let slug: String = task
        .split_whitespace()
        .take(3)
        .collect::<Vec<_>>()
        .join("-");
    let clean: String = slug
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' { c.to_ascii_lowercase() } else { '-' })
        .collect();
    // Collapse runs of `-` and trim to 20 chars.
    let collapsed: String = clean
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    collapsed.chars().take(20).collect()
}

/// List all orchestrator window base names currently alive in `session`.
/// Returns pairs of `(base_name, phase)`.
pub fn list_live_orchestrators(session: &str) -> Vec<(String, String)> {
    use crate::status::find_live_phase;
    // tmux list-windows to get all window names, then filter for orchestrate- prefix.
    let output = std::process::Command::new("tmux")
        .args([
            "list-windows",
            "-t",
            session,
            "-F",
            "#{window_name}",
        ])
        .output();
    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => return vec![],
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut result = Vec::new();
    for line in stdout.lines() {
        let base = tmux::base_window_name(line);
        if is_orchestrate_window(base) {
            let phase = find_live_phase(session, base).unwrap_or_default();
            // Avoid duplicates (multiple lines same base with different phases)
            if !result.iter().any(|(b, _): &(String, String)| b == base) {
                result.push((base.to_string(), phase));
            }
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Command
// ---------------------------------------------------------------------------

/// Spawn a new orchestrator agent in a fresh tmux window named
/// `orchestrate-<label>` where `label` is derived from the first few words of
/// `task`.  Multiple orchestrators can coexist simultaneously.
///
/// Unlike the old single-window design, this never replaces an existing window.
/// Each call always creates a brand-new window so concurrent tasks run in
/// parallel orchestrators.
pub fn cmd_orchestrate(registry: &Registry, task: &str) -> Result<String> {
    let session = tmux::current_session()?;

    let label = label_from_task(task);
    let window_base = orchestrate_window_name(&label);

    // Capture a live registry snapshot at spawn time so the agent has
    // full situational awareness in its initial prompt.
    let registry_snapshot = format_registry_snapshot(registry);

    let prompt = build_orchestrate_prompt(task, &session, &window_base, &registry_snapshot);

    // The orchestrator runs from the task-master base directory (where
    // task-master.toml lives), giving it access to the full registry.
    let working_dir = registry.base_dir.to_string_lossy().to_string();

    info!(
        "[{}] Spawning orchestrator in session '{}'",
        window_base, session
    );

    // Always create a new window — never replace an existing one.
    tmux::spawn_window(&session, &window_base, &working_dir, &prompt, Some("orchestrate"), None)?;
    tmux::set_window_phase(&session, &window_base, Some("active"))?;

    info!(
        "[{}] Orchestrator started in window '{}:active'.",
        window_base, window_base
    );

    // Return the window base name so callers can record group association.
    Ok(window_base)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_orchestrate_prompt_contains_task() {
        let prompt = build_orchestrate_prompt(
            "add distributed tracing across all services",
            "mysession",
            "orchestrate-add-distribute",
            "snapshot",
        );
        assert!(prompt.contains("add distributed tracing across all services"));
    }

    #[test]
    fn test_build_orchestrate_prompt_contains_snapshot() {
        let snapshot = "PROJECT  SHORT  WORKTREE  PHASE\nmy-svc   SVC    SVC-main  idle";
        let prompt =
            build_orchestrate_prompt("some task", "sess", "orchestrate-some-task", snapshot);
        assert!(prompt.contains("SVC-main"));
        assert!(prompt.contains("idle"));
    }

    #[test]
    fn test_label_from_task() {
        assert_eq!(label_from_task("implement oauth login flow"), "implement-oauth-logi"); // truncated at 20
        assert_eq!(label_from_task("add distributed tracing"), "add-distributed-trac"); // truncated at 20
        assert_eq!(label_from_task("fix"), "fix");
        assert_eq!(label_from_task("  spaces  everywhere  today  "), "spaces-everywhere-to"); // truncated at 20
        // always within 20 chars
        let long = label_from_task("abcdefghijk mnopqr stuvwx");
        assert!(long.len() <= 20);
    }

    #[test]
    fn test_orchestrate_window_name() {
        assert_eq!(orchestrate_window_name("implement-oauth"), "orchestrate-implement-oauth");
    }

    #[test]
    fn test_is_orchestrate_window() {
        assert!(is_orchestrate_window("orchestrate-foo"));
        assert!(is_orchestrate_window("orchestrate-implement-oauth-login"));
        assert!(!is_orchestrate_window("orchestrate")); // old fixed name
        assert!(!is_orchestrate_window("WIS-olive"));
        assert!(!is_orchestrate_window("supervisor"));
    }

    #[test]
    fn test_build_orchestrate_prompt_contains_done_rename() {
        let window = orchestrate_window_name("my-task");
        let prompt = build_orchestrate_prompt("task", "sess", &window, "snap");
        assert!(
            prompt.contains(&format!("{}:done", window)),
            "prompt must reference :done rename"
        );
        assert!(
            prompt.contains("sess"),
            "prompt must reference the session"
        );
    }

    #[test]
    fn test_build_orchestrate_prompt_contains_blocked_rename() {
        let window = orchestrate_window_name("my-task");
        let prompt = build_orchestrate_prompt("task", "sess", &window, "snap");
        assert!(
            prompt.contains(&format!("{}:blocked", window)),
            "prompt must reference :blocked rename"
        );
    }

    #[test]
    fn test_build_orchestrate_prompt_no_code_changes() {
        let prompt = build_orchestrate_prompt("task", "s", "orchestrate-foo", "snap");
        assert!(
            prompt.contains("Do NOT modify any source files"),
            "prompt must forbid code changes"
        );
        assert!(
            prompt.contains("must NOT write any code"),
            "prompt must forbid writing code"
        );
    }

    #[test]
    fn test_build_orchestrate_prompt_spawn_instructions() {
        let prompt = build_orchestrate_prompt("task", "s", "orchestrate-foo", "snap");
        assert!(prompt.contains("task-master spawn"));
        assert!(prompt.contains("task-master spawn --ephemeral"));
        assert!(prompt.contains("task-master status"));
        assert!(prompt.contains("task-master send"));
    }

    #[test]
    fn test_build_orchestrate_prompt_bd_commands() {
        let prompt = build_orchestrate_prompt("task", "s", "orchestrate-foo", "snap");
        assert!(prompt.contains("bd create"));
        assert!(prompt.contains("bd update"));
        assert!(prompt.contains("bd close"));
    }

    #[test]
    fn test_build_orchestrate_prompt_uses_awk_rename() {
        let window = orchestrate_window_name("my-task");
        let prompt = build_orchestrate_prompt("task", "sess", &window, "snap");
        assert!(prompt.contains("awk"), "should use awk-based rename");
        assert!(
            prompt.contains(&format!("{}:done", window)),
            "should include :done phase"
        );
    }

    #[test]
    fn test_build_orchestrate_prompt_is_deterministic() {
        let a = build_orchestrate_prompt("task", "sess", "orchestrate-foo", "snap");
        let b = build_orchestrate_prompt("task", "sess", "orchestrate-foo", "snap");
        assert_eq!(a, b, "build_orchestrate_prompt must be deterministic");
    }

    #[test]
    fn test_format_registry_snapshot_no_tmux() {
        // When not in tmux, phases show "?" — just verify it doesn't panic and
        // includes project info.
        use crate::registry::Registry;
        use std::path::PathBuf;

        let toml = r#"
[[projects]]
name = "warehouse-service"
short = "WIS"
repo = "projects/warehouse-service"

[[projects.worktrees]]
name = "olive"

[[projects]]
name = "msg-service"
short = "MSG"
repo = "projects/msg-service"
"#;
        let reg = Registry::load_from_str(toml, PathBuf::from("/fake")).unwrap();
        let snapshot = format_registry_snapshot(&reg);
        assert!(snapshot.contains("warehouse-service"));
        assert!(snapshot.contains("WIS"));
        assert!(snapshot.contains("WIS-olive"));
        assert!(snapshot.contains("msg-service"));
        assert!(snapshot.contains("MSG"));
        // No worktrees for MSG → shows placeholder
        assert!(snapshot.contains("(no worktrees)"));
    }
}
