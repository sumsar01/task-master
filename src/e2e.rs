use crate::qa::{detect_branch, detect_repo_slug};
use crate::registry::Registry;
use crate::templates;
use crate::tmux;
use anyhow::Result;
use std::path::Path;
use tracing::info;

/// Build the inline e2e validation prompt passed to the opencode agent.
///
/// Tries to load a custom template from `<base_dir>/.opencode/agents/e2e.md`
/// first. If the file exists its body (with YAML frontmatter stripped) is used
/// as the template with `{{token}}` placeholders substituted at runtime.
/// Falls back to the built-in string constant when the file is absent.
///
/// The agent is told upfront that AWS SSO and kubectl are already configured —
/// it auto-detects identity at runtime rather than requiring config.
pub fn build_e2e_prompt(
    base_dir: &Path,
    repo: &str,
    branch: &str,
    pr_number: u64,
    session: &str,
    base_name: &str, // e.g. "WIS-olive"
) -> String {
    let rename_cmd = tmux::build_rename_cmd(session, base_name);

    let done_rename = format!("{} '{base}:e2e-done'", rename_cmd, base = base_name);
    let blocked_rename = format!("{} '{base}:e2e-blocked'", rename_cmd, base = base_name);

    let pr_str = pr_number.to_string();

    let vars: &[(&str, &str)] = &[
        ("pr", &pr_str),
        ("branch", branch),
        ("repo", repo),
        ("done_rename", &done_rename),
        ("blocked_rename", &blocked_rename),
    ];

    if let Some(raw) = templates::load(base_dir, "e2e") {
        let body = templates::strip_frontmatter(&raw);
        return templates::render(body, vars);
    }

    format!(
        "You are an e2e validation agent for PR #{pr} on branch '{branch}' in repo '{repo}'.\n\
         \n\
         Your job is to validate that the code deployed to the staging environment is working\n\
         correctly. You have up to 3 iterations to fix problems and re-validate before escalating.\n\
         \n\
         ENVIRONMENT ASSUMPTIONS\n\
         \n\
         You are running in a terminal that is already:\n\
         - Authenticated via AWS SSO (aws commands will work without credential setup)\n\
         - Pointed at the correct kubectl context for staging\n\
         Do NOT attempt to configure credentials or switch contexts. Confirm identity first:\n\
         \n\
         ```bash\n\
         aws sts get-caller-identity\n\
         kubectl config current-context\n\
         ```\n\
         \n\
         If either command fails, stop immediately and report the error — do not proceed\n\
         with validation if you are not authenticated.\n\
         \n\
         LOOP PROCEDURE (repeat up to 3 times)\n\
         \n\
         Step 1 — Understand what changed\n\
         Run: gh pr diff {pr}\n\
         Read the diff carefully. Note:\n\
         - New or changed infrastructure (Terraform, k8s manifests, DynamoDB, S3, SNS, SQS, etc.)\n\
         - New or changed environment variables / secrets\n\
         - New or changed service endpoints or API routes\n\
         - New or changed background jobs or event processors\n\
         - Anything deleted or renamed\n\
         \n\
         Also read the PR title and description:\n\
           gh pr view {pr} --json title,body\n\
         \n\
         Step 2 — Explore relevant codebase context\n\
         Based on what changed, explore the relevant parts of the repo:\n\
         - Infrastructure: check infrastructure/ and any Terraform .tf files touched by the PR\n\
         - K8s: check any Kubernetes manifest files or Helm charts touched\n\
         - Service config: check environment variable declarations, secrets references\n\
         - Application code: understand what the changed code is supposed to do at runtime\n\
         \n\
         Step 3 — Generate a targeted validation plan\n\
         Based on steps 1 and 2, write out a concrete validation plan BEFORE executing it.\n\
         The plan should be specific to this PR's changes, not a generic checklist.\n\
         Examples of what to include based on what changed:\n\
         \n\
         If a new DynamoDB table was added:\n\
           - aws dynamodb describe-table --table-name <name> --region <region>\n\
           - Verify table exists, has correct key schema and billing mode\n\
         \n\
         If pods were changed:\n\
           - kubectl get pods -n <namespace> (check all pods Running/Ready)\n\
           - kubectl describe pod <pod> -n <namespace> (look for crash loops or errors)\n\
           - kubectl logs <pod> -n <namespace> --tail=100 (look for startup errors)\n\
         \n\
         If environment variables or secrets changed:\n\
           - kubectl exec -n <namespace> <pod> -- env | grep <VAR_NAME>\n\
           - Verify secret exists: kubectl get secret <name> -n <namespace>\n\
         \n\
         If new API routes were added:\n\
           - curl or kubectl exec to hit the endpoint and verify it responds correctly\n\
         \n\
         If event processors / jobs changed:\n\
           - Check CloudWatch logs or kubectl logs for recent invocations\n\
           - Verify the processor is consuming from the correct queue/topic\n\
         \n\
         If infrastructure was deleted or renamed:\n\
           - Verify the old resource is gone and the new one is present\n\
           - Verify nothing still references the old name\n\
         \n\
         Step 4 — Execute the validation plan\n\
         Run each check from step 3. For each check, note: PASS, FAIL, or SKIP (with reason).\n\
         \n\
         If a check FAILS:\n\
         - Diagnose the root cause\n\
         - If it is a fixable code/config bug: fix it in the source code, then:\n\
             git add -A\n\
             git commit -m 'e2e: fix <description of what was wrong>'\n\
             git push --force-with-lease\n\
           Wait for the deployment to complete before re-checking. Use kubectl rollout status\n\
           or CloudWatch to confirm the new version is running.\n\
         - If it is an infrastructure issue (Terraform not applied, deployment not triggered,\n\
           etc.) that requires human action: note it and continue with remaining checks.\n\
         - If it is a flaky/transient issue: retry the check once before marking as FAIL.\n\
         \n\
         Step 5 — Evaluate\n\
         - All checks PASS → proceed to DONE.\n\
         - Any checks FAIL with fixable code issues → go back to Step 1 (next iteration).\n\
         - Any checks FAIL with infrastructure/human-action issues → note them and continue\n\
           to DONE (report them as requiring human follow-up, not as blocking failures).\n\
         \n\
         DONE (all fixable issues resolved)\n\
         \n\
         1. Rename the window to signal e2e complete:\n\
              {done_rename}\n\
         2. Print a clear summary in the TUI:\n\
              - Total checks: N\n\
              - Passed: N\n\
              - Fixed during e2e: N (list each fix with a one-line description)\n\
              - Requires human follow-up: N (list each item)\n\
         \n\
         Then stop.\n\
         \n\
         ESCALATION (after 3 full iterations, still failing)\n\
         \n\
         You MUST complete all 3 iterations before escalating.\n\
         \n\
         1. Rename the window to signal it is blocked:\n\
              {blocked_rename}\n\
         2. Print a clear escalation summary in the TUI:\n\
              - What you checked\n\
              - What failed and why you could not fix it\n\
              - What human action is needed\n\
         \n\
         Then stop.\n\
         \n\
         IMPORTANT RULES\n\
         - Only push to branch '{branch}' - never create a new branch.\n\
         - Always use --force-with-lease when pushing.\n\
         - Keep commit messages prefixed with 'e2e:'.\n\
         - Do not modify infrastructure that requires Terraform apply — flag it for humans.\n\
         - Do not approve or merge the PR.\n\
         - Do not switch kubectl contexts or AWS profiles — work with what is already configured.\n\
         - If aws or kubectl commands return permission errors, report them and stop.\n\
         - gh CLI is available for GitHub interactions.",
        pr = pr_number,
        branch = branch,
        repo = repo,
        done_rename = done_rename,
        blocked_rename = blocked_rename,
    )
}

/// Spawn an e2e validation agent for the given worktree and PR number.
///
/// The window is renamed to `<base>:e2e` and the running process is replaced
/// with a fresh opencode TUI running the e2e validation prompt.
pub fn cmd_e2e(registry: &Registry, worktree_name: &str, pr_number: u64) -> Result<()> {
    let worktree = registry.require_worktree(worktree_name)?;

    let abs_path_str = worktree.abs_path.to_string_lossy().to_string();
    let repo_slug = detect_repo_slug(&abs_path_str)?;
    let branch = detect_branch(&abs_path_str)?;
    let session = tmux::current_session()?;

    let base_name = tmux::base_window_name(worktree_name).to_string();

    let prompt = build_e2e_prompt(
        &registry.base_dir,
        &repo_slug,
        &branch,
        pr_number,
        &session,
        &base_name,
    );

    info!(
        "[{}] Starting e2e validation for PR #{} in session '{}'",
        base_name, pr_number, session
    );

    // Transition: <base>:* -> <base>:e2e
    tmux::set_window_phase(&session, &base_name, Some("e2e"))?;

    // Replace whatever is running with a fresh opencode TUI running the e2e prompt.
    tmux::replace_window_process(&session, &base_name, &abs_path_str, &prompt, None)?;

    println!(
        "E2e agent started for '{}' (PR #{}) — window is now '{}:e2e'.",
        base_name, pr_number, base_name
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn no_template() -> &'static Path {
        Path::new("/tmp/no-such-dir-for-e2e-tests")
    }

    fn make_prompt() -> String {
        build_e2e_prompt(
            no_template(),
            "acme/repo",
            "feat/foo",
            42,
            "mysession",
            "WIS-olive",
        )
    }

    #[test]
    fn test_prompt_contains_pr_branch_repo() {
        let p = make_prompt();
        assert!(p.contains("PR #42"));
        assert!(p.contains("feat/foo"));
        assert!(p.contains("acme/repo"));
    }

    #[test]
    fn test_prompt_contains_identity_check() {
        let p = make_prompt();
        assert!(p.contains("aws sts get-caller-identity"));
        assert!(p.contains("kubectl config current-context"));
    }

    #[test]
    fn test_prompt_tells_agent_already_authenticated() {
        let p = make_prompt();
        assert!(p.contains("already"));
        assert!(p.contains("AWS SSO"));
        assert!(p.contains("Do NOT attempt to configure credentials"));
    }

    #[test]
    fn test_prompt_contains_done_and_blocked_renames() {
        let p = make_prompt();
        assert!(p.contains("WIS-olive:e2e-done"));
        assert!(p.contains("WIS-olive:e2e-blocked"));
    }

    #[test]
    fn test_prompt_contains_session_and_base_name() {
        let p = make_prompt();
        assert!(p.contains("mysession"));
        assert!(p.contains("WIS-olive"));
    }

    #[test]
    fn test_prompt_contains_up_to_3_iterations() {
        let p = make_prompt();
        assert!(p.contains("up to 3 iterations") || p.contains("3 times"));
        assert!(p.contains("You MUST complete all 3 iterations before escalating"));
    }

    #[test]
    fn test_prompt_branch_rule() {
        let p = make_prompt();
        assert!(p.contains("'feat/foo'"));
        assert!(p.contains("never create a new branch"));
    }

    #[test]
    fn test_prompt_force_with_lease() {
        let p = make_prompt();
        assert!(p.contains("--force-with-lease"));
    }

    #[test]
    fn test_prompt_commit_prefix() {
        let p = make_prompt();
        assert!(p.contains("'e2e:'"));
    }

    #[test]
    fn test_prompt_gh_pr_diff() {
        let p = make_prompt();
        assert!(p.contains("gh pr diff 42"));
    }

    #[test]
    fn test_prompt_gh_pr_view() {
        let p = make_prompt();
        assert!(p.contains("gh pr view 42"));
    }

    #[test]
    fn test_prompt_window_naming_convention() {
        let p = build_e2e_prompt(no_template(), "o/r", "b", 1, "s", "WIS-cedar");
        assert!(p.contains("WIS-cedar:e2e-done"));
        assert!(p.contains("WIS-cedar:e2e-blocked"));
        assert!(!p.contains("WIS-cedar:review"));
        assert!(!p.contains("WIS-cedar:blocked\n"));
    }

    #[test]
    fn test_prompt_does_not_switch_contexts() {
        let p = make_prompt();
        assert!(p.contains("Do not switch kubectl contexts or AWS profiles"));
    }

    #[test]
    fn test_prompt_infrastructure_terraform_flag() {
        let p = make_prompt();
        assert!(p.contains("Terraform apply"));
        assert!(p.contains("flag it for humans"));
    }
}
