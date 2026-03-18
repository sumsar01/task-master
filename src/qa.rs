use crate::registry::Registry;
use crate::tmux;
use anyhow::{Context, Result};
use tracing::info;

/// Build the inline QA loop prompt passed to the opencode agent.
///
/// The prompt is self-contained so the agent doesn't need to discover any
/// external instructions file — it starts with fresh context and full
/// instructions every time.
pub fn build_qa_prompt(repo: &str, branch: &str, pr_number: u64) -> String {
    let handoff_body = format!(
        "QA agent summary\\n\\nCompleted QA review. Here is what was done:\\n\
         - [list fixes applied]\\n\
         - [list comments resolved]\\n\
         - [anything left for humans]\\n\\n\
         Ready for human review."
    );
    let escalation_body = format!(
        "QA agent escalation\\n\\nAfter 3 iterations I was unable to fully resolve all issues. \
         Human input needed:\\n\\n\
         **Remaining CI failures:**\\n\
         - [list each failing check and why you could not fix it]\\n\\n\
         **Remaining review comments needing human decision:**\\n\
         - [list each comment]\\n\\n\
         **What I did fix:**\\n\
         - [list]"
    );

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
         Post a PR comment summarising what you did:\n\
           gh pr comment {pr} --body \"{handoff}\"\n\
         \n\
         Remove the wip label:\n\
           gh pr edit {pr} --remove-label wip\n\
         \n\
         Then stop.\n\
         \n\
         ESCALATION (after 3 iterations, still not clean)\n\
         \n\
         Post a PR comment with a clear escalation summary:\n\
           gh pr comment {pr} --body \"{escalation}\"\n\
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
        handoff = handoff_body,
        escalation = escalation_body,
    )
}

/// Spawn a QA agent for the given worktree and PR number.
pub fn cmd_qa(registry: &Registry, worktree_name: &str, pr_number: u64) -> Result<()> {
    let worktree = registry.find_worktree(worktree_name).with_context(|| {
        format!(
            "Worktree '{}' not found. Run `task-master list` to see available worktrees.",
            worktree_name
        )
    })?;

    // Detect the GitHub repo slug (owner/name) from the git remote inside the worktree.
    let repo_slug = detect_repo_slug(&worktree.abs_path.to_string_lossy())?;

    // Detect the current branch of the worktree.
    let branch = detect_branch(&worktree.abs_path.to_string_lossy())?;

    let prompt = build_qa_prompt(&repo_slug, &branch, pr_number);

    // QA windows are named "<original-window>-qa", e.g. "WIS-olive-qa".
    let qa_window_name = format!("{}-qa", worktree_name);
    let abs_path_str = worktree.abs_path.to_string_lossy().to_string();

    let session = tmux::current_session()?;

    info!(
        "[{}] Spawning QA agent for PR #{} in session '{}', dir {}",
        qa_window_name, pr_number, session, abs_path_str
    );

    let is_new = tmux::spawn_window(&session, &qa_window_name, &abs_path_str, &prompt)?;

    if is_new {
        println!(
            "Spawned QA agent '{}' in a new window for PR #{}.",
            qa_window_name, pr_number
        );
    } else {
        println!(
            "Sent QA task to existing '{}' window for PR #{}.",
            qa_window_name, pr_number
        );
    }

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
        let prompt = build_qa_prompt("acme/repo", "feature/foo", 42);
        assert!(prompt.contains("PR #42"));
        assert!(prompt.contains("feature/foo"));
        assert!(prompt.contains("acme/repo"));
    }
}
