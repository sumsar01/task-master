use crate::registry::Registry;
use crate::tmux;
use anyhow::{Context, Result};
use tracing::info;

/// Build the inline QA loop prompt passed to the opencode agent.
///
/// `session` and `dev_window` are injected so the agent can update the tmux
/// window name to reflect its current phase without any external tooling.
///
/// `default_branch` is the repo's default branch (e.g. "main" or "master"),
/// used as the rebase target instead of hardcoding "master".
pub fn build_qa_prompt(
    repo: &str,
    branch: &str,
    pr_number: u64,
    session: &str,
    dev_window: &str, // base name, e.g. "WIS-olive"
    default_branch: &str,
) -> String {
    // The QA agent runs inside the dev window itself (renamed to :qa) and uses
    // tmux rename-window (by index) to update its phase suffix on handoff/escalation.
    // We tell the agent to run `tmux list-windows` to find the window index,
    // then rename it — this avoids colon-in-target ambiguity.
    let rename_cmd = tmux::build_rename_cmd(session, dev_window);

    let handoff_rename = format!("{} '{base}:review'", rename_cmd, base = dev_window);
    let escalation_rename = format!("{} '{base}:blocked'", rename_cmd, base = dev_window);

    // Bodies use real newlines so the heredoc in the agent's shell command
    // produces proper line breaks in the GitHub comment.
    let handoff_body = "QA agent summary\n\nCompleted QA review. Here is what was done:\n\
         - [list fixes applied]\n\
         - [list comments resolved]\n\
         - [anything left for humans]\n\n\
         Ready for human review.";

    let escalation_body = "QA agent escalation\n\nAfter 3 iterations I was unable to fully \
         resolve all issues. Human input needed:\n\n\
         **Remaining CI failures:**\n\
         - [list each failing check and why you could not fix it]\n\n\
         **Remaining review comments needing human decision:**\n\
         - [list each comment]\n\n\
         **What I did fix:**\n\
         - [list]";

    format!(
        "You are a QA agent for PR #{pr} on branch '{branch}' in repo '{repo}'.\n\
         \n\
         Your job is to iterate (up to 3 times) until the PR is clean, then hand off to humans.\n\
         \n\
         LOOP PROCEDURE (repeat up to 3 times)\n\
         \n\
         Step 0 - Sync with base branch\n\
         Run: git fetch origin\n\
         Run: git rebase origin/{default_branch}\n\
         If the rebase SUCCEEDS (no conflicts) and produced new commits:\n\
         - Push: git push --force-with-lease\n\
         \n\
         If the rebase FAILS with conflicts, DO NOT abort immediately.\n\
         First, diagnose each conflicting file:\n\
           git diff HEAD...origin/{default_branch} -- <file>   (see what {default_branch} did)\n\
           git log --oneline origin/{default_branch} -- <file> (find the {default_branch} commit)\n\
         Then apply these resolution rules:\n\
         \n\
         RULE A - Take {default_branch}'s version when:\n\
           - {default_branch} moved/extracted/restructured the file and this branch only modified its content.\n\
             (The {default_branch} restructure already incorporates the intent of this branch's change.)\n\
           - The conflicting region in {default_branch} is a superset of what this branch added.\n\
           Resolution: git checkout --theirs <file> && git add <file>\n\
         \n\
         RULE B - Take the branch's version when:\n\
           - {default_branch}'s change to this file is purely structural (e.g. a rename/move side-effect)\n\
             and the branch contains the substantive logic change.\n\
           Resolution: git checkout --ours <file> && git add <file>\n\
         \n\
         RULE C - Escalate only when:\n\
           - Both sides made independent substantive changes to the SAME logic with contradictory outcomes.\n\
           - You cannot tell from commit messages which intent should win.\n\
           In this case: git rebase --abort, then skip to ESCALATION.\n\
         \n\
         After resolving all conflicts: git rebase --continue\n\
         Then push: git push --force-with-lease\n\
         \n\
         Step 1 - Self-review the diff\n\
         Run: gh pr diff {pr}\n\
          Look for: obvious bugs, missing error handling, unhandled edge cases, style issues, missing tests,\n\
          DRY violations (duplicated logic that could be extracted), magic numbers (literals that should be\n\
          named constants), missing barrel file exports for new modules, functions over 50 lines (consider\n\
          extracting helpers), and files over 100 lines (consider splitting). Use judgement on length limits —\n\
          some files are legitimately long. Flag concerns but don't refactor blindly.\n\
         Fix anything you can fix directly.\n\
         \n\
         Step 2 - Resolve bot/reviewer comments\n\
         First, fetch all open review threads and their comment text:\n\
           gh api graphql -f query='{{\n\
             repository(owner:\"{owner}\", name:\"{name}\") {{\n\
               pullRequest(number: {pr}) {{\n\
                 reviewThreads(first: 50) {{\n\
                   nodes {{\n\
                     id\n\
                     isResolved\n\
                     comments(first: 5) {{ nodes {{ body }} }}\n\
                   }}\n\
                 }}\n\
               }}\n\
             }}\n\
           }}'\n\
         For every thread where isResolved is false:\n\
         - Read the comment body from comments.nodes[0].body to understand what the reviewer asked.\n\
         - If the comment is actionable by a code change: apply the fix in the code.\n\
           Then mark the thread resolved:\n\
             gh api graphql -f query='mutation {{\n\
               resolveReviewThread(input: {{ threadId: \"<id>\" }}) {{\n\
                 thread {{ isResolved }}\n\
               }}\n\
             }}'\n\
           Replace <id> with the thread id from the fetch query above.\n\
         - If the comment is a question or requires human judgement: leave it unresolved.\n\
         \n\
         Step 3 - Check CI status\n\
         Run: gh pr checks {pr}\n\
         \n\
         STALE CHECK DETECTION (do this before treating any failure as current):\n\
         Get the current HEAD SHA: git rev-parse HEAD\n\
         For each failing check, get the SHA it ran against:\n\
           gh pr checks {pr} --json name,state,detailsUrl\n\
         If the failing checks were triggered by an earlier commit (detailsUrl or context\n\
         shows a different SHA), the checks are stale. Do not try to fix stale failures.\n\
         Instead: wait 2 minutes, then re-run `gh pr checks {pr}` to get fresh results.\n\
         Repeat up to 3 times. If checks are still stale after 3 polls, note it and\n\
         continue — do not escalate due to stale checks alone.\n\
         \n\
         For each check that is failing AND is on the current HEAD:\n\
         - Attempt to read the failure logs: gh run view <run-id> --log-failed\n\
         - If `gh run view` returns an error (e.g. 404 — this happens with CircleCI and\n\
           other non-GitHub-Actions CI systems), log that logs are inaccessible and\n\
           continue. Do NOT escalate just because logs cannot be read.\n\
         - If you can read the logs: fix the root cause in the code.\n\
         - If the failure is a flaky/infrastructure issue outside your control, note it.\n\
         \n\
         Step 4 - Commit and push fixes\n\
         If you made any changes:\n\
           git add -A\n\
           git commit -m 'qa: fix CI/review issues (iteration N)'\n\
           git push --force-with-lease\n\
         Then wait 90 seconds for CI to re-run before checking again.\n\
         \n\
         Step 5 - Evaluate\n\
         - All CI checks green AND no actionable unresolved threads -> proceed to Handoff.\n\
         - Otherwise -> go back to Step 0 (next iteration).\n\
         \n\
         HANDOFF (all checks green, no actionable comments)\n\
         \n\
         1. Rename the dev window to signal ready-for-review:\n\
              {handoff_rename}\n\
         2. Post a PR comment summarising what you did (write body to file to preserve newlines):\n\
              cat > /tmp/qa-comment-{pr}.txt <<'BODY'\n\
         {handoff_body}\n\
         BODY\n\
              gh pr comment {pr} --body-file /tmp/qa-comment-{pr}.txt\n\
         3. Remove the wip label:\n\
              gh pr edit {pr} --remove-label wip\n\
         \n\
         Then stop.\n\
         \n\
         ESCALATION (after 3 full iterations, still not clean)\n\
         \n\
         You MUST complete all 3 iterations before escalating. Do not escalate early\n\
         because CI logs are inaccessible, checks are stale, or a single iteration\n\
         produced no progress. Each iteration may unblock the next.\n\
         \n\
         1. Rename the dev window to signal it is blocked:\n\
              {escalation_rename}\n\
         2. Post a PR comment with a clear escalation summary (write body to file to preserve newlines):\n\
              cat > /tmp/qa-escalation-{pr}.txt <<'BODY'\n\
         {escalation_body}\n\
         BODY\n\
              gh pr comment {pr} --body-file /tmp/qa-escalation-{pr}.txt\n\
         \n\
         Then stop. Do NOT remove the wip label on escalation.\n\
         \n\
         IMPORTANT RULES\n\
         - Only push to branch '{branch}' - never create a new branch.\n\
         - Always use --force-with-lease when pushing (branch may have been rebased).\n\
         - Keep commit messages prefixed with 'qa:'.\n\
         - Do not approve the PR yourself.\n\
         - Do not merge the PR.\n\
         - Only resolve review threads where you have applied a code fix — never resolve questions or human-judgement items.\n\
         - If you are unsure whether a fix is correct, leave it for the human and note it in your comment.\n\
         - gh CLI is available. Use it for all GitHub interactions.",
        pr = pr_number,
        branch = branch,
        repo = repo,
        owner = repo.split('/').next().unwrap_or(""),
        name = repo.split('/').nth(1).unwrap_or(""),
        default_branch = default_branch,
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
pub fn cmd_qa(registry: &Registry, worktree_name: &str, pr_number: Option<u64>) -> Result<String> {
    let worktree = registry.require_worktree(worktree_name)?;

    let repo_slug = detect_repo_slug(&worktree.abs_path.to_string_lossy())?;
    let branch = detect_branch(&worktree.abs_path.to_string_lossy())?;
    let default_branch = detect_default_branch(&repo_slug);
    let session = tmux::current_session()?;

    // Resolve PR number: use the provided one or auto-detect from the branch.
    let pr_number = match pr_number {
        Some(n) => n,
        None => {
            info!(
                "[{}] No PR number given — detecting from branch '{}'",
                worktree_name, branch
            );
            detect_pr_number(&branch)?
        }
    };

    // The base window name (no phase suffix), e.g. "WIS-olive"
    let base_name = tmux::base_window_name(worktree_name).to_string();
    let abs_path_str = worktree.abs_path.to_string_lossy().to_string();

    let prompt = build_qa_prompt(
        &repo_slug,
        &branch,
        pr_number,
        &session,
        &base_name,
        &default_branch,
    );

    info!(
        "[{}] Starting QA for PR #{} in session '{}'",
        base_name, pr_number, session
    );

    let window_exists = tmux::find_window_index(&session, &base_name).is_some();

    if window_exists {
        // Transition: WIS-olive:dev -> WIS-olive:qa (or overwrite any existing phase)
        tmux::set_window_phase(&session, &base_name, Some("qa"))?;
        // Replace whatever is running with a fresh opencode TUI running the QA prompt.
        tmux::replace_window_process(&session, &base_name, &abs_path_str, &prompt, None)?;
    } else {
        // No window yet — create it directly in :qa phase.
        tmux::spawn_window(&session, &base_name, &abs_path_str, &prompt, Some("qa"))?;
    }

    Ok(format!(
        "QA agent started for '{}' (PR #{}) — window is now '{}:qa'.",
        base_name, pr_number, base_name
    ))
}

pub fn detect_repo_slug(worktree_dir: &str) -> Result<String> {
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

fn parse_github_slug(url: &str) -> Option<String> {
    let url = url.trim_end_matches(".git");

    // Normalise credential-embedded HTTPS URLs:
    // https://user:token@github.com/owner/repo  ->  https://github.com/owner/repo
    let stripped;
    let url = if url.starts_with("https://") {
        if let Some(at_pos) = url.find('@') {
            stripped = format!("https://{}", &url[at_pos + 1..]);
            stripped.as_str()
        } else {
            url
        }
    } else {
        url
    };

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

/// Detect the repo's default branch via `gh repo view`.
///
/// Falls back to `"master"` silently if `gh` is unavailable, the repo has no
/// remote, or the command fails for any other reason.
pub fn detect_default_branch(repo_slug: &str) -> String {
    let out = std::process::Command::new("gh")
        .args([
            "repo",
            "view",
            repo_slug,
            "--json",
            "defaultBranchRef",
            "--jq",
            ".defaultBranchRef.name",
        ])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if s.is_empty() {
                "master".to_string()
            } else {
                s
            }
        }
        _ => "master".to_string(),
    }
}

/// Validate the raw output of `git rev-parse --abbrev-ref HEAD`.
///
/// Returns the branch name if it looks like a real branch, or an error for
/// detached HEAD state (raw == "HEAD") or empty output.
fn validate_branch(raw: &str, worktree_dir: &str) -> Result<String> {
    if raw == "HEAD" {
        anyhow::bail!(
            "Worktree {} is in detached HEAD state — \
             check out a branch before running QA",
            worktree_dir
        );
    }
    if raw.is_empty() {
        anyhow::bail!("Could not detect current branch in {}", worktree_dir);
    }
    Ok(raw.to_string())
}

pub fn detect_branch(worktree_dir: &str) -> Result<String> {
    let output = std::process::Command::new("git")
        .args(["-C", worktree_dir, "rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .context("Failed to run git rev-parse")?;

    if !output.status.success() {
        anyhow::bail!("Could not detect current branch in {}", worktree_dir);
    }

    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    validate_branch(&raw, worktree_dir)
}

/// Detect the open PR number for the given branch via `gh pr list`.
///
/// Returns an error if no open PR is found or `gh` is unavailable.
pub fn detect_pr_number(branch: &str) -> Result<u64> {
    let output = std::process::Command::new("gh")
        .args([
            "pr",
            "list",
            "--head",
            branch,
            "--state",
            "open",
            "--json",
            "number",
            "--jq",
            ".[0].number",
        ])
        .output()
        .context("Failed to run gh pr list")?;

    if !output.status.success() {
        anyhow::bail!(
            "gh pr list failed for branch '{}': {}",
            branch,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if raw.is_empty() || raw == "null" {
        anyhow::bail!(
            "No open PR found for branch '{}'. Pass the PR number explicitly.",
            branch
        );
    }

    raw.parse::<u64>().with_context(|| {
        format!(
            "Unexpected output from gh pr list for branch '{}': {:?}",
            branch, raw
        )
    })
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
        let prompt = build_qa_prompt(
            "acme/repo",
            "feature/foo",
            42,
            "mysession",
            "WIS-olive",
            "master",
        );
        assert!(prompt.contains("PR #42"));
        assert!(prompt.contains("feature/foo"));
        assert!(prompt.contains("acme/repo"));
        assert!(prompt.contains("WIS-olive:review"));
        assert!(prompt.contains("WIS-olive:blocked"));
    }

    // --- new tests ---

    #[test]
    fn test_parse_github_slug_with_trailing_newline() {
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
    fn test_parse_github_slug_with_embedded_credentials() {
        // Regression: PAT token embedded in remote URL (the form git uses when
        // credentials are stored inline, e.g. via a credential helper or
        // `git remote set-url origin https://user:token@github.com/…`)
        assert_eq!(
            parse_github_slug(
                "https://skrwhiteaway:gho_TOKEN@github.com/whiteaway/fulfillment-service"
            ),
            Some("whiteaway/fulfillment-service".to_string())
        );
    }

    #[test]
    fn test_parse_github_slug_with_embedded_credentials_dot_git() {
        assert_eq!(
            parse_github_slug("https://user:token@github.com/owner/repo.git"),
            Some("owner/repo".to_string())
        );
    }

    #[test]
    fn test_build_qa_prompt_contains_rename_commands() {
        let prompt = build_qa_prompt(
            "acme/repo",
            "feat/x",
            7,
            "my-session",
            "PROJ-branch",
            "master",
        );
        assert!(prompt.contains("my-session"));
        assert!(prompt.contains("PROJ-branch"));
        assert!(prompt.contains("gh pr diff 7"));
        assert!(prompt.contains("gh pr checks 7"));
        assert!(prompt.contains("gh pr comment 7"));
        assert!(prompt.contains("gh pr edit 7"));
        assert!(prompt.contains("reviewThreads"));
    }

    #[test]
    fn test_build_qa_prompt_correct_branch_rule() {
        let prompt = build_qa_prompt("o/r", "my-branch", 1, "s", "W-w", "master");
        assert!(prompt.contains("'my-branch'"));
    }

    #[test]
    fn test_build_qa_prompt_qa_window_naming_convention() {
        let prompt = build_qa_prompt("o/r", "b", 99, "ses", "DEV-main", "master");
        assert!(prompt.contains("DEV-main:review"));
        assert!(prompt.contains("DEV-main:blocked"));
        assert!(!prompt.contains("DEV-main-qa"));
    }

    #[test]
    fn test_build_qa_prompt_contains_rebase_step() {
        let prompt = build_qa_prompt("acme/repo", "feat/foo", 55, "s", "W-w", "master");
        assert!(prompt.contains("git fetch origin"));
        // Branch-agnostic check — just verify "origin/" is present
        assert!(prompt.contains("git rebase origin/"));
        assert!(prompt.contains("--force-with-lease"));
        assert!(prompt.contains("git checkout --theirs"));
        assert!(prompt.contains("git checkout --ours"));
        assert!(prompt.contains("git rebase --abort"));
        assert!(prompt.contains("ESCALATION"));
    }

    #[test]
    fn test_build_qa_prompt_uses_provided_default_branch() {
        let prompt = build_qa_prompt("acme/repo", "feat/foo", 55, "s", "W-w", "main");
        assert!(
            prompt.contains("origin/main"),
            "should use provided default branch 'main'"
        );
        assert!(
            !prompt.contains("origin/master"),
            "should not contain hardcoded 'master'"
        );
    }

    #[test]
    fn test_build_qa_prompt_contains_resolve_thread() {
        let prompt = build_qa_prompt("acme/repo", "feat/foo", 55, "s", "W-w", "master");
        assert!(prompt.contains("resolveReviewThread"));
        assert!(prompt.contains("threadId"));
        assert!(prompt.contains("acme"));
        assert!(prompt.contains("isResolved"));
        assert!(prompt.contains("human judgement"));
    }

    #[test]
    fn test_build_qa_prompt_step2_fetch_includes_comments_field() {
        // The fetch query must request comment text so the agent can read
        // what the reviewer wrote. Inline diff thread bodies are in
        // comments.nodes.body, not the thread-level body field.
        let prompt = build_qa_prompt("acme/repo", "feat/foo", 55, "s", "W-w", "master");
        assert!(
            prompt.contains("comments(first: 5)"),
            "fetch query should request comments field to read reviewer text"
        );
        assert!(
            prompt.contains("comments.nodes[0].body"),
            "prompt should instruct agent to read comment text from comments.nodes[0].body"
        );
    }

    #[test]
    fn test_build_qa_prompt_step2_does_not_resolve_questions() {
        // Guard: questions and human-judgement comments must never be auto-resolved.
        let prompt = build_qa_prompt("acme/repo", "feat/foo", 55, "s", "W-w", "master");
        assert!(
            prompt.contains("human judgement") || prompt.contains("human-judgement"),
            "prompt must preserve the rule about not resolving human-judgement threads"
        );
        assert!(
            prompt.contains("leave it unresolved"),
            "prompt must explicitly say to leave human-judgement threads unresolved"
        );
    }

    // --- Bug 3: validate_branch ---

    #[test]
    fn test_validate_branch_rejects_head_literal() {
        let err = validate_branch("HEAD", "/tmp/wt").unwrap_err();
        assert!(
            err.to_string().contains("detached HEAD"),
            "error should mention detached HEAD, got: {}",
            err
        );
    }

    #[test]
    fn test_validate_branch_rejects_empty() {
        let err = validate_branch("", "/tmp/wt").unwrap_err();
        assert!(err.to_string().contains("Could not detect"));
    }

    #[test]
    fn test_validate_branch_accepts_normal_branch() {
        assert_eq!(
            validate_branch("feature/foo", "/tmp/wt").unwrap(),
            "feature/foo"
        );
    }

    #[test]
    fn test_validate_branch_accepts_main() {
        assert_eq!(validate_branch("main", "/tmp/wt").unwrap(), "main");
    }

    // --- Bug 4: PR comment bodies use --body-file + real newlines ---

    #[test]
    fn test_build_qa_prompt_comment_bodies_use_body_file() {
        let prompt = build_qa_prompt("o/r", "b", 42, "s", "W-w", "main");
        assert!(
            prompt.contains("--body-file"),
            "prompt should instruct agent to use --body-file"
        );
        assert!(
            !prompt.contains("--body \""),
            "prompt should not use inline --body with double quotes"
        );
    }

    #[test]
    fn test_build_qa_prompt_comment_bodies_have_real_newlines() {
        let prompt = build_qa_prompt("o/r", "b", 42, "s", "W-w", "main");
        // The handoff body is embedded in the prompt; find the section after HANDOFF
        let handoff_section = prompt
            .split("HANDOFF")
            .nth(1)
            .expect("prompt should contain HANDOFF section");
        // The body text "QA agent summary" should be followed by real newlines, not \n literals
        assert!(
            handoff_section.contains("QA agent summary\n"),
            "handoff body should contain actual newline after summary header"
        );
        // Verify no literal backslash-n in the body content
        assert!(
            !handoff_section.contains("summary\\n"),
            "handoff body should not contain literal \\n"
        );
    }

    // --- Stale CI check detection ---

    #[test]
    fn test_build_qa_prompt_contains_stale_check_detection() {
        let prompt = build_qa_prompt("o/r", "b", 42, "s", "W-w", "main");
        assert!(
            prompt.contains("STALE CHECK DETECTION"),
            "prompt should contain stale check detection section"
        );
        assert!(
            prompt.contains("git rev-parse HEAD"),
            "prompt should instruct agent to get HEAD SHA"
        );
        assert!(
            prompt.contains("wait 2 minutes"),
            "prompt should instruct agent to wait 2 minutes before re-polling"
        );
    }

    #[test]
    fn test_build_qa_prompt_circleci_fallback() {
        let prompt = build_qa_prompt("o/r", "b", 42, "s", "W-w", "main");
        assert!(
            prompt.contains("gh run view") && prompt.contains("404"),
            "prompt should mention that gh run view can return 404 for non-GitHub-Actions CI"
        );
        assert!(
            prompt.contains("Do NOT escalate just because logs cannot be read"),
            "prompt should explicitly say not to escalate due to inaccessible logs"
        );
    }

    #[test]
    fn test_build_qa_prompt_minimum_3_iterations_before_escalation() {
        let prompt = build_qa_prompt("o/r", "b", 42, "s", "W-w", "main");
        let escalation_section = prompt
            .split("ESCALATION (after 3 full iterations")
            .nth(1)
            .expect("prompt should contain ESCALATION (after 3 full iterations) section");
        assert!(
            escalation_section.contains("You MUST complete all 3 iterations before escalating"),
            "prompt should require all 3 iterations before escalation"
        );
        assert!(
            escalation_section.contains("Do not escalate early"),
            "prompt should explicitly prohibit early escalation"
        );
    }
}
