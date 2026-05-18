//! GitHub PR information model and fetching.
//!
//! `PrInfo` and `fetch_pr_info` are intentionally kept separate from the main
//! `App` state so they can be called from background threads without pulling in
//! the full ratatui dependency tree.

/// Information about a GitHub pull request associated with a worktree branch.
#[derive(Debug, Clone)]
pub struct PrInfo {
    pub number: u32,
    pub title: String,
    /// "OPEN" | "MERGED" | "CLOSED"
    pub state: String,
    pub url: String,
    /// true when the PR is in draft state (state will be "OPEN")
    pub draft: bool,
    /// "APPROVED" | "CHANGES_REQUESTED" | "REVIEW_REQUIRED" | "" | None
    pub review_decision: Option<String>,
    /// "SUCCESS" | "FAILURE" | "PENDING" | None
    pub checks: Option<String>,
}

/// Fetch PR info for the worktree at `path` by running `gh pr view`.
///
/// Returns `None` when:
/// - `gh` is not available
/// - no open (or recent) PR exists for the current branch
/// - the output cannot be parsed
///
/// This function is intentionally **synchronous** — it is meant to be called
/// from a background thread, not the TUI event loop.
pub fn fetch_pr_info(path: &str) -> Option<PrInfo> {
    let output = std::process::Command::new("gh")
        .args([
            "pr",
            "view",
            "--json",
            "number,title,state,url,isDraft,reviewDecision,statusCheckRollup",
        ])
        .current_dir(path)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let text = text.trim();
    if text.is_empty() {
        return None;
    }

    let v: serde_json::Value = serde_json::from_str(text).ok()?;

    let number = v["number"].as_u64()? as u32;
    let title = v["title"].as_str().unwrap_or("").to_string();
    let state = v["state"].as_str().unwrap_or("").to_string();
    let url = v["url"].as_str().unwrap_or("").to_string();
    let draft = v["isDraft"].as_bool().unwrap_or(false);

    let review_decision = match v["reviewDecision"].as_str() {
        Some(s) if !s.is_empty() => Some(s.to_string()),
        _ => None,
    };

    // statusCheckRollup is an array; take the first element's `state` field.
    // gh aggregates all checks; pick the first which is the rolled-up result.
    let checks = v["statusCheckRollup"]
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|entry| entry["state"].as_str())
        .map(|s| s.to_string());

    Some(PrInfo {
        number,
        title,
        state,
        url,
        draft,
        review_decision,
        checks,
    })
}
