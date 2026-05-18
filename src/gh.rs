//! GitHub account isolation helpers.
//!
//! `task-master setup-gh-accounts` creates a per-account gh config directory
//! at `~/.config/gh-<account>/` for each distinct `gh_account` value in
//! the registry.  The directory contains a `hosts.yml` that sets that account
//! as the active user, and a copy of the global `config.yml`.
//!
//! Agents launched with `GH_CONFIG_DIR=~/.config/gh-<account>` will then use
//! the correct GitHub identity without touching the global config and without
//! any token being written to disk or passed through environment variables.

use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use crate::registry::Registry;

// ---------------------------------------------------------------------------
// Public helpers
// ---------------------------------------------------------------------------

/// Return the path to the per-account gh config directory.
/// E.g. `gh_config_dir_for("skrwhiteaway")` → `~/.config/gh-skrwhiteaway`.
pub fn gh_config_dir_for(account: &str) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    PathBuf::from(home).join(".config").join(format!("gh-{}", account))
}

// ---------------------------------------------------------------------------
// Command
// ---------------------------------------------------------------------------

/// Create per-account gh config directories for every `gh_account` value
/// found in the registry.  Idempotent — safe to run multiple times.
pub fn cmd_setup_gh_accounts(registry: &Registry) -> Result<String> {
    let home = std::env::var("HOME").context("HOME env var not set")?;
    let global_config = PathBuf::from(&home).join(".config").join("gh");

    // Collect the global config.yml content (settings — not credentials).
    let config_yml_path = global_config.join("config.yml");
    let config_yml = fs::read_to_string(&config_yml_path)
        .with_context(|| format!("Failed to read {}", config_yml_path.display()))?;

    // Read the global hosts.yml so we can extract the full users block.
    let hosts_yml_path = global_config.join("hosts.yml");
    let hosts_yml = fs::read_to_string(&hosts_yml_path)
        .with_context(|| format!("Failed to read {}", hosts_yml_path.display()))?;

    // Collect distinct gh_account values from the registry.
    let accounts: BTreeSet<String> = registry
        .projects
        .iter()
        .filter_map(|p| p.gh_account.clone())
        .collect();

    if accounts.is_empty() {
        return Ok(
            "No gh_account values found in task-master.toml. \
             Add gh_account = \"<username>\" to your [[projects]] blocks, \
             then re-run setup-gh-accounts."
                .to_string(),
        );
    }

    let mut created = Vec::new();
    let mut skipped = Vec::new();

    for account in &accounts {
        let dir = gh_config_dir_for(account);
        fs::create_dir_all(&dir)
            .with_context(|| format!("Failed to create {}", dir.display()))?;

        // Write config.yml (shared settings, no credentials).
        let dest_config = dir.join("config.yml");
        fs::write(&dest_config, &config_yml)
            .with_context(|| format!("Failed to write {}", dest_config.display()))?;

        // Build a hosts.yml with this account set as active.
        // We keep the full users block from the global hosts.yml so the
        // keyring-backed tokens remain accessible, but override `user:`.
        let dest_hosts = dir.join("hosts.yml");
        let new_hosts = rewrite_active_user(&hosts_yml, account);
        let already_correct = dest_hosts
            .exists()
            .then(|| fs::read_to_string(&dest_hosts).ok())
            .flatten()
            .map(|existing| existing == new_hosts)
            .unwrap_or(false);

        if already_correct {
            skipped.push(account.clone());
        } else {
            fs::write(&dest_hosts, &new_hosts)
                .with_context(|| format!("Failed to write {}", dest_hosts.display()))?;
            created.push(account.clone());
        }
    }

    let mut lines = Vec::new();
    for a in &created {
        lines.push(format!(
            "  created  ~/.config/gh-{}/  (active user: {})",
            a, a
        ));
    }
    for a in &skipped {
        lines.push(format!("  ok       ~/.config/gh-{}/  (already up to date)", a));
    }
    lines.push(String::new());
    lines.push(
        "Done. Agents spawned into projects with gh_account set will use \
         GH_CONFIG_DIR=~/.config/gh-<account> automatically."
            .to_string(),
    );
    Ok(lines.join("\n"))
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Rewrite the `user:` line in a hosts.yml string to set `account` as active.
/// The full `users:` block is preserved so keyring tokens remain accessible.
fn rewrite_active_user(hosts_yml: &str, account: &str) -> String {
    // The hosts.yml format looks like:
    //   github.com:
    //       git_protocol: https
    //       users:
    //           skrwhiteaway:
    //           sumsar01:
    //       user: sumsar01
    //
    // We replace the `user: <anything>` line (with any leading whitespace) with
    // `    user: <account>`.  If there's no `user:` line we append one.
    let user_line_re = format!("user: {}", account);
    let mut found = false;
    let mut result_lines: Vec<&str> = Vec::new();

    for line in hosts_yml.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("user:") {
            found = true;
            // Preserve indentation of the original line.
            let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
            result_lines.push(Box::leak(
                format!("{}user: {}", indent, account).into_boxed_str(),
            ));
        } else {
            result_lines.push(line);
        }
    }

    if !found {
        result_lines.push(Box::leak(
            format!("    {}", user_line_re).into_boxed_str(),
        ));
    }

    result_lines.join("\n") + "\n"
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rewrite_active_user_replaces_existing() {
        let input = "github.com:\n    git_protocol: https\n    users:\n        skrwhiteaway:\n        sumsar01:\n    user: sumsar01\n";
        let output = rewrite_active_user(input, "skrwhiteaway");
        assert!(output.contains("user: skrwhiteaway"));
        assert!(!output.contains("user: sumsar01"));
        // users block preserved
        assert!(output.contains("skrwhiteaway:"));
        assert!(output.contains("sumsar01:"));
    }

    #[test]
    fn test_rewrite_active_user_appends_when_missing() {
        let input = "github.com:\n    git_protocol: https\n    users:\n        sumsar01:\n";
        let output = rewrite_active_user(input, "sumsar01");
        assert!(output.contains("user: sumsar01"));
    }

    #[test]
    fn test_gh_config_dir_for() {
        let dir = gh_config_dir_for("skrwhiteaway");
        assert!(dir.to_string_lossy().contains("gh-skrwhiteaway"));
    }
}
