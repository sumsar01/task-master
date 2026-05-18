//! `add-project` command — clone a bare repo and register it in task-master.toml.

use crate::registry::Registry;
use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::process::Command;
use tracing::info;

/// Clone a bare repo and append a `[[projects]]` entry to `task-master.toml`.
///
/// * `account` — optional gh account name; when set, a per-account OAuth token
///   is fetched and embedded in the clone URL so the correct identity is used.
/// * `group` / `context` — optional TOML fields written into the new entry.
pub fn cmd_add_project(
    base_dir: &PathBuf,
    name: &str,
    short: &str,
    url: &str,
    account: Option<&str>,
    group: Option<&str>,
    context: Option<&str>,
) -> Result<()> {
    // Check short name not already taken
    let config_path = base_dir.join("task-master.toml");
    if config_path.exists() {
        let contents = std::fs::read_to_string(&config_path)?;
        if let Ok(existing) = Registry::load(base_dir.clone()) {
            if existing.find_project(short).is_some() {
                bail!("Project short name '{}' is already in use.", short);
            }
        }
        let _ = contents; // suppress unused warning
    }

    let projects_dir = base_dir.join("projects");
    std::fs::create_dir_all(&projects_dir).context("Failed to create projects/ directory")?;

    let repo_path = projects_dir.join(name);
    if repo_path.exists() {
        bail!("Directory already exists: {}", repo_path.display());
    }

    // Resolve token for the requested account (if any) so git clone can authenticate.
    // If no account is specified, fall back to whatever `gh auth git-credential` provides
    // (the active account) by running a plain clone with no explicit credentials.
    let token: Option<String> = if let Some(acct) = account {
        let out = Command::new("gh")
            .args(["auth", "token", "--user", acct])
            .output();
        match out {
            Ok(o) if o.status.success() => {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            }
            _ => bail!("Could not retrieve token for gh account '{}'", acct),
        }
    } else {
        None
    };

    info!("Cloning bare repo {} -> {}", url, repo_path.display());

    // Build the clone URL. When we have a token, embed it as Basic auth using
    // the `x-access-token:<token>` scheme, which GitHub accepts for OAuth/PAT
    // tokens. This avoids the credential-helper entirely and works regardless
    // of which account is currently active in `gh`.
    //
    // We pass GIT_TERMINAL_PROMPT=0 so git fails cleanly instead of hanging on
    // a password prompt if the token is wrong or the repo doesn't exist.
    let clone_url: String = if let Some(ref tok) = token {
        // Insert credentials into the URL: https://x-access-token:TOKEN@host/path
        if let Some(rest) = url.strip_prefix("https://") {
            format!("https://x-access-token:{}@{}", tok, rest)
        } else {
            // Non-HTTPS URL (SSH, etc.) — pass through unchanged; token unused.
            url.to_string()
        }
    } else {
        url.to_string()
    };

    let mut cmd = Command::new("git");
    cmd.args(["clone", "--bare", &clone_url])
        .arg(&repo_path)
        .env("GIT_TERMINAL_PROMPT", "0");
    let output = cmd.output().context("Failed to run git clone")?;

    if !output.status.success() {
        let stderr_raw = String::from_utf8_lossy(&output.stderr);
        // Redact any embedded token from error output so credentials are never
        // surfaced in the status bar or logs.
        let stderr = if let Some(ref tok) = token {
            stderr_raw.replace(tok.as_str(), "<token>")
        } else {
            stderr_raw.to_string()
        };
        // Strip git progress/info lines (start with "Cloning into", "remote:", etc.)
        // and keep only the fatal/error lines that explain what went wrong.
        let error_lines: Vec<&str> = stderr
            .lines()
            .filter(|l| {
                let l = l.trim();
                l.starts_with("fatal:") || l.starts_with("error:") || l.starts_with("ERROR:")
            })
            .collect();
        let detail = if error_lines.is_empty() {
            stderr.trim().to_string()
        } else {
            error_lines.join("; ")
        };
        if detail.is_empty() {
            bail!("git clone failed (exit {})", output.status);
        } else {
            bail!("git clone failed: {}", detail);
        }
    }

    // Append [[projects]] block to task-master.toml
    let repo_rel = format!("projects/{}", name);

    let group_line = group
        .map(|g| format!("group = \"{}\"\n", g))
        .unwrap_or_default();
    let context_line = context
        .map(|c| format!("context = \"{}\"\n", c))
        .unwrap_or_default();
    let new_block = format!(
        "\n[[projects]]\nname = \"{}\"\nshort = \"{}\"\nrepo = \"{}\"\n{}{}",
        name, short, repo_rel, group_line, context_line
    );

    let mut contents = if config_path.exists() {
        std::fs::read_to_string(&config_path)?
    } else {
        String::new()
    };
    contents.push_str(&new_block);
    std::fs::write(&config_path, &contents)?;

    println!(
        "Added project {} ({}). Add a worktree with:\n  task-master add-worktree {} olive",
        name, short, short
    );

    Ok(())
}
