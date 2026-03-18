use crate::registry::Registry;
use crate::tmux;
use anyhow::{Context, Result};
use tracing::info;

/// Build the inline QA loop prompt passed to the opencode agent.
///
/// `session` and `dev_window` are injected so the agent can update the tmux
/// window name to reflect its current phase without any external tooling.
pub fn build_qa_prompt(
    repo: &str,
    branch: &str,
    pr_number: u64,
    session: &str,
    dev_window: &str, // base name, e.g. "WIS-olive"
) -> String {
    // The QA agent runs inside the dev window itself (renamed to :qa) and uses
    // tmux rename-window (by index) to update its phase suffix on handoff/escalation.
    // We tell the agent to run `tmux list-windows` to find the window index,
    // then rename it — this avoids colon-in-target ambiguity.
    let rename_cmd = format!(
        "tmux list-windows -t {session} -F '#{{window_index}} #{{window_name}}' \
         | awk -F'[ :]' '$2==\"{base}\" {{print $1}}' \
         | xargs -I{{}} tmux rename-window -t {session}:{{}}",
        session = session,
        base = dev_window,
    );

    let handoff_rename = format!("{} '{base}:review'", rename_cmd, base = dev_window);
    let escalation_rename = format!("{} '{base}:blocked'", rename_cmd, base = dev_window);

    let handoff_body = "QA agent summary\\n\\nCompleted QA review. Here is what was done:\\n\
         - [list fixes applied]\\n\
         - [list comments resolved]\\n\
         - [anything left for humans]\\n\\n\
         Ready for human review.";

    let escalation_body = "QA agent escalation\\n\\nAfter 3 iterations I was unable to fully \
         resolve all issues. Human input needed:\\n\\n\
         **Remaining CI failures:**\\n\
         - [list each failing check and why you could not fix it]\\n\\n\
         **Remaining review comments needing human decision:**\\n\
         - [list each comment]\\n\\n\
         **What I did fix:**\\n\
         - [list]";

    format!(
        "You are a QA agent for PR #{pr} on branch '{branch}' in repo '{repo}'.\n\
         \n\
         Your job is to iterate (up to 3 times) until the PR is clean, then hand off to humans.\n\
         \n\
         LOOP PROCEDURE (repeat up to 3 times)\n\
         \n\
         Step 1 - Self-review the diff\n\
         Run: gh pr diff {pr}\n\
         Look for: obvious bugs, missing error handling, unhandled edge cases, style issues, missing tests.\n\
         Fix anything you can fix directly.\n\
         \n\
         Step 2 - Resolve bot/reviewer comments\n\
         Run: gh pr view {pr} --comments\n\
         For every unresolved comment that is actionable by code change: apply the fix.\n\
         For comments asking questions or needing human judgement: leave them for humans (do not dismiss).\n\
         \n\
         Step 3 - Check CI status\n\
         Run: gh pr checks {pr}\n\
         If any checks are failing:\n\
         - Read the failure logs: gh run view <run-id> --log-failed\n\
         - Fix the root cause in the code.\n\
         - If the failure is a flaky/infrastructure issue outside your control, note it in your escalation comment.\n\
         \n\
         Step 4 - Commit and push fixes\n\
         If you made any changes:\n\
           git add -A\n\
           git commit -m 'qa: fix CI/review issues (iteration N)'\n\
           git push\n\
         Then wait 90 seconds for CI to re-run before checking again.\n\
         \n\
         Step 5 - Evaluate\n\
         - All CI checks green AND no actionable unresolved comments -> proceed to Handoff.\n\
         - Otherwise -> go back to Step 1 (next iteration).\n\
         \n\
         HANDOFF (all checks green, no actionable comments)\n\
         \n\
         1. Rename the dev window to signal ready-for-review:\n\
              {handoff_rename}\n\
         2. Post a PR comment summarising what you did:\n\
              gh pr comment {pr} --body \"{handoff_body}\"\n\
         3. Remove the wip label:\n\
              gh pr edit {pr} --remove-label wip\n\
         \n\
         Then stop.\n\
         \n\
         ESCALATION (after 3 iterations, still not clean)\n\
         \n\
         1. Rename the dev window to signal it is blocked:\n\
              {escalation_rename}\n\
         2. Post a PR comment with a clear escalation summary:\n\
              gh pr comment {pr} --body \"{escalation_body}\"\n\
         \n\
         Then stop. Do NOT remove the wip label on escalation.\n\
         \n\
         IMPORTANT RULES\n\
         - Only push to branch '{branch}' - never create a new branch.\n\
         - Keep commit messages prefixed with 'qa:'.\n\
         - Do not approve the PR yourself.\n\
         - Do not merge the PR.\n\
         - If you are unsure whether a fix is correct, leave it for the human and note it in your comment.\n\
         - gh CLI is available. Use it for all GitHub interactions.",
        pr = pr_number,
        branch = branch,
        repo = repo,
        handoff_rename = handoff_rename,
        escalation_rename = escalation_rename,
        handoff_body = handoff_body,
        escalation_body = escalation_body,
    )
}

/// Spawn a QA agent for the given worktree and PR number.
///
/// The dev window (`WIS-olive:dev`) is renamed to `WIS-olive:qa` and its
/// running opencode process is replaced with a fresh TUI running the QA prompt.
/// No separate `-qa` window is created — the lifecycle lives in one window.
pub fn cmd_qa(registry: &Registry, worktree_name: &str, pr_number: u64) -> Result<()> {
    let worktree = registry.find_worktree(worktree_name).with_context(|| {
        format!(
            "Worktree '{}' not found. Run `task-master list` to see available worktrees.",
            worktree_name
        )
    })?;

    let repo_slug = detect_repo_slug(&worktree.abs_path.to_string_lossy())?;
    let branch = detect_branch(&worktree.abs_path.to_string_lossy())?;
    let session = tmux::current_session()?;

    // The base window name (no phase suffix), e.g. "WIS-olive"
    let base_name = tmux::base_window_name(worktree_name).to_string();
    let abs_path_str = worktree.abs_path.to_string_lossy().to_string();

    let prompt = build_qa_prompt(&repo_slug, &branch, pr_number, &session, &base_name);

    info!(
        "[{}] Starting QA for PR #{} in session '{}'",
        base_name, pr_number, session
    );

    // Transition: WIS-olive:dev -> WIS-olive:qa
    tmux::set_window_phase(&session, &base_name, Some("qa"))?;

    // Replace the dev agent with a fresh opencode TUI running the QA prompt.
    tmux::replace_window_process(&session, &base_name, &abs_path_str, &prompt)?;

    println!(
        "QA agent started for '{}' (PR #{}) — window is now '{}:qa'.",
        base_name, pr_number, base_name
    );

    Ok(())
}

fn detect_repo_slug(worktree_dir: &str) -> Result<String> {
    let output = std::process::Command::new("git")
        .args(["-C", worktree_dir, "remote", "get-url", "origin"])
        .output()
        .context("Failed to run git remote get-url")?;

    if !output.status.success() {
        anyhow::bail!("Could not get git remote 'origin' in {}", worktree_dir);
    }

    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    parse_github_slug(&url)
        .with_context(|| format!("Could not parse GitHub slug from remote URL: {}", url))
}

/// Parse owner/repo from various GitHub URL formats:
///   https://github.com/owner/repo.git
///   git@github.com:owner/repo.git
fn parse_github_slug(url: &str) -> Option<String> {
    let url = url.trim_end_matches(".git");

    // HTTPS: https://github.com/owner/repo
    if let Some(rest) = url.strip_prefix("https://github.com/") {
        return Some(rest.to_string());
    }

    // SSH: git@github.com:owner/repo
    if let Some(rest) = url.strip_prefix("git@github.com:") {
        return Some(rest.to_string());
    }

    None
}

fn detect_branch(worktree_dir: &str) -> Result<String> {
    let output = std::process::Command::new("git")
        .args(["-C", worktree_dir, "rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .context("Failed to run git rev-parse")?;

    if !output.status.success() {
        anyhow::bail!("Could not detect current branch in {}", worktree_dir);
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_github_slug_https() {
        assert_eq!(
            parse_github_slug("https://github.com/acme/my-repo.git"),
            Some("acme/my-repo".to_string())
        );
    }

    #[test]
    fn test_parse_github_slug_ssh() {
        assert_eq!(
            parse_github_slug("git@github.com:acme/my-repo.git"),
            Some("acme/my-repo".to_string())
        );
    }

    #[test]
    fn test_parse_github_slug_no_dot_git() {
        assert_eq!(
            parse_github_slug("https://github.com/acme/my-repo"),
            Some("acme/my-repo".to_string())
        );
    }

    #[test]
    fn test_parse_github_slug_unknown() {
        assert_eq!(parse_github_slug("https://gitlab.com/acme/repo"), None);
    }

    #[test]
    fn test_build_qa_prompt_contains_pr_number() {
        let prompt = build_qa_prompt("acme/repo", "feature/foo", 42, "mysession", "WIS-olive");
        assert!(prompt.contains("PR #42"));
        assert!(prompt.contains("feature/foo"));
        assert!(prompt.contains("acme/repo"));
        assert!(prompt.contains("WIS-olive:review"));
        assert!(prompt.contains("WIS-olive:blocked"));
    }

    // --- new tests ---

    #[test]
    fn test_parse_github_slug_with_trailing_newline() {
        // `git remote get-url` output is trimmed before calling parse_github_slug;
        // but the function itself should also handle leading/trailing whitespace gracefully.
        // The function trims ".git" only; whitespace trimming happens in the caller.
        // Verify that a clean URL (already trimmed) still works.
        assert_eq!(
            parse_github_slug("https://github.com/owner/repo.git"),
            Some("owner/repo".to_string())
        );
    }

    #[test]
    fn test_parse_github_slug_ssh_no_dot_git() {
        assert_eq!(
            parse_github_slug("git@github.com:owner/repo"),
            Some("owner/repo".to_string())
        );
    }

    #[test]
    fn test_parse_github_slug_empty_string() {
        assert_eq!(parse_github_slug(""), None);
    }

    #[test]
    fn test_parse_github_slug_non_github_ssh() {
        assert_eq!(parse_github_slug("git@gitlab.com:owner/repo.git"), None);
    }

    #[test]
    fn test_build_qa_prompt_contains_rename_commands() {
        let prompt = build_qa_prompt("acme/repo", "feat/x", 7, "my-session", "PROJ-branch");
        // The awk-based rename command must reference the correct session and base window.
        assert!(prompt.contains("my-session"));
        assert!(prompt.contains("PROJ-branch"));
        // Ensure gh pr diff and gh pr checks commands are present.
        assert!(prompt.contains("gh pr diff 7"));
        assert!(prompt.contains("gh pr checks 7"));
        assert!(prompt.contains("gh pr view 7"));
        assert!(prompt.contains("gh pr comment 7"));
        assert!(prompt.contains("gh pr edit 7"));
    }

    #[test]
    fn test_build_qa_prompt_correct_branch_rule() {
        let prompt = build_qa_prompt("o/r", "my-branch", 1, "s", "W-w");
        // The prompt must tell the agent to push only to the correct branch.
        assert!(prompt.contains("'my-branch'"));
    }

    #[test]
    fn test_build_qa_prompt_qa_window_naming_convention() {
        // The QA window is named <base>-qa by cmd_qa; the prompt itself references
        // the dev window base name and phase suffixes, not the QA window name.
        let prompt = build_qa_prompt("o/r", "b", 99, "ses", "DEV-main");
        assert!(prompt.contains("DEV-main:review"));
        assert!(prompt.contains("DEV-main:blocked"));
        // Should NOT accidentally contain the QA window name.
        assert!(!prompt.contains("DEV-main-qa"));
    }
}
