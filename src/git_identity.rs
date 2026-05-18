//! Per-project git identity management.
//!
//! Writes `user.name`, `user.email`, signing key, and credential username
//! overrides into bare repo git configs so that all linked worktrees inherit
//! the correct identity without being affected by `includeIf` rules in
//! `~/.gitconfig`.

use crate::registry::Registry;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::info;

/// Write `user.name`, `user.email`, and `credential.https://github.com.username`
/// into a bare repo's git config.
///
/// This is the canonical place to set a per-project git identity — all linked
/// worktrees inherit the bare repo's "local" config, so a single write here
/// covers every worktree without needing per-worktree overrides.
///
/// Both `name` and `email` must be `Some`; if either is `None` the function is
/// a no-op (returns `Ok(())`).
pub fn write_git_identity_to_repo(
    repo_path: &Path,
    name: Option<&str>,
    email: Option<&str>,
    signing_key: Option<&str>,
) -> Result<()> {
    let (name, email) = match (name, email) {
        (Some(n), Some(e)) => (n, e),
        _ => return Ok(()),
    };

    let git_config = |key: &str, val: &str| -> Result<()> {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo_path)
            .args(["config", key, val])
            .output()
            .with_context(|| {
                format!(
                    "Failed to run: git -C {} config {} {}",
                    repo_path.display(),
                    key,
                    val
                )
            })?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            bail!(
                "git config {} failed in '{}': {}",
                key,
                repo_path.display(),
                stderr.trim()
            );
        }
        Ok(())
    };

    git_config("user.name", name)?;
    git_config("user.email", email)?;
    // Override the HTTPS credential username so that the gh credential helper is
    // invoked with the correct GitHub account name. Without this, an `includeIf`
    // rule in ~/.gitconfig (e.g. for a personal account) can inject a different
    // username, causing the credential helper to return nothing and git to fall
    // back to an interactive password prompt — which hangs any non-interactive
    // agent session (QA, plan, e2e).
    git_config("credential.https://github.com.username", name)?;

    // Apply SSH commit-signing config if a signing key is specified.
    if let Some(key_path) = signing_key {
        // Expand a leading `~/` to the user's home directory so the path works
        // in git config even when git doesn't expand tildes in this field.
        let expanded = if key_path.starts_with("~/") {
            if let Some(home) = std::env::var_os("HOME") {
                format!("{}/{}", home.to_string_lossy(), &key_path[2..])
            } else {
                key_path.to_string()
            }
        } else {
            key_path.to_string()
        };
        git_config("gpg.format", "ssh")?;
        git_config("gpg.ssh.signingKey", &expanded)?;
        git_config("commit.gpgsign", "true")?;
        git_config("tag.gpgsign", "true")?;
        info!(
            "Set SSH signing key in '{}': signingKey={}",
            repo_path.display(),
            expanded,
        );
    }

    info!(
        "Set git identity in '{}': name={}, email={}, credential_username={}",
        repo_path.display(),
        name,
        email,
        name,
    );
    Ok(())
}

/// Apply the `git_name`/`git_email`/`git_signing_key` identity overrides for every
/// project in the registry that has them configured.
///
/// This is the one-shot repair for bare repos that were created before the
/// per-project identity feature existed.  It is idempotent — running it multiple
/// times produces the same result.
///
/// Returns a human-readable summary string.
pub fn cmd_fix_git_identity(registry: &Registry, base_dir: &PathBuf) -> Result<String> {
    let mut updated = 0usize;
    let mut skipped = 0usize;
    let mut missing = 0usize;

    for proj in &registry.projects {
        let name = proj.git_name.as_deref();
        let email = proj.git_email.as_deref();
        let signing_key = proj.git_signing_key.as_deref();

        if name.is_none() && email.is_none() && signing_key.is_none() {
            skipped += 1;
            info!(
                "Skipping '{}' — no git_name/git_email/git_signing_key configured",
                proj.short
            );
            continue;
        }

        let repo_path = base_dir.join(&proj.repo);
        if !repo_path.exists() {
            missing += 1;
            eprintln!(
                "Warning: bare repo for '{}' not found at '{}' — skipping.",
                proj.short,
                repo_path.display()
            );
            continue;
        }

        match write_git_identity_to_repo(&repo_path, name, email, signing_key) {
            Ok(()) => {
                updated += 1;
                println!(
                    "  {} → name={}, email={}, signing_key={}",
                    proj.short,
                    name.unwrap_or("(unchanged)"),
                    email.unwrap_or("(unchanged)"),
                    signing_key.unwrap_or("(unchanged)"),
                );
            }
            Err(e) => {
                missing += 1;
                eprintln!(
                    "Warning: could not set git identity for '{}': {}",
                    proj.short, e
                );
            }
        }
    }

    Ok(format!(
        "Git identity applied to {} project(s){skipped_note}{missing_note}.",
        updated,
        skipped_note = if skipped > 0 {
            format!(" ({} had no identity configured, skipped)", skipped)
        } else {
            String::new()
        },
        missing_note = if missing > 0 {
            format!(" ({} failed — see warnings above)", missing)
        } else {
            String::new()
        }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// Helper: create a minimal bare git repo at `path`.
    fn make_bare_repo(path: &Path) {
        Command::new("git")
            .args(["init", "--bare"])
            .arg(path)
            .status()
            .expect("git init --bare failed");
    }

    #[test]
    fn test_write_git_identity_writes_name_and_email() {
        let root = tempfile::tempdir().expect("tempdir");
        let bare = root.path().join("myrepo.git");
        make_bare_repo(&bare);

        write_git_identity_to_repo(&bare, Some("Alice"), Some("alice@example.com"), None).unwrap();

        let name = Command::new("git")
            .args(["-C"])
            .arg(&bare)
            .args(["config", "user.name"])
            .output()
            .unwrap();
        let email = Command::new("git")
            .args(["-C"])
            .arg(&bare)
            .args(["config", "user.email"])
            .output()
            .unwrap();

        assert_eq!(
            String::from_utf8_lossy(&name.stdout).trim(),
            "Alice",
            "user.name should be Alice"
        );
        assert_eq!(
            String::from_utf8_lossy(&email.stdout).trim(),
            "alice@example.com",
            "user.email should be alice@example.com"
        );
    }

    #[test]
    fn test_write_git_identity_also_sets_credential_username() {
        let root = tempfile::tempdir().expect("tempdir");
        let bare = root.path().join("cred.git");
        make_bare_repo(&bare);

        write_git_identity_to_repo(
            &bare,
            Some("skrwhiteaway"),
            Some("98815660+skrwhiteaway@users.noreply.github.com"),
            None,
        )
        .unwrap();

        let cred_user = Command::new("git")
            .args(["-C"])
            .arg(&bare)
            .args([
                "config",
                "--local",
                "credential.https://github.com.username",
            ])
            .output()
            .unwrap();

        assert!(
            cred_user.status.success(),
            "credential.https://github.com.username should be set in local repo config"
        );
        assert_eq!(
            String::from_utf8_lossy(&cred_user.stdout).trim(),
            "skrwhiteaway",
            "credential username should match git_name (skrwhiteaway)"
        );
    }

    #[test]
    fn test_write_git_identity_credential_username_not_set_when_noop() {
        let root = tempfile::tempdir().expect("tempdir");
        let bare = root.path().join("cred_noop.git");
        make_bare_repo(&bare);

        write_git_identity_to_repo(&bare, None, None, None).unwrap();

        let out = Command::new("git")
            .args(["-C"])
            .arg(&bare)
            .args([
                "config",
                "--local",
                "credential.https://github.com.username",
            ])
            .output()
            .unwrap();
        assert!(
            !out.status.success(),
            "credential username should not be set when name/email are both None"
        );
    }

    #[test]
    fn test_write_git_identity_noop_when_both_none() {
        let root = tempfile::tempdir().expect("tempdir");
        let bare = root.path().join("noop.git");
        make_bare_repo(&bare);

        write_git_identity_to_repo(&bare, None, None, None).unwrap();

        let out = Command::new("git")
            .args(["-C"])
            .arg(&bare)
            .args(["config", "--local", "user.name"])
            .output()
            .unwrap();
        assert!(
            !out.status.success(),
            "user.name should not be set locally when both are None"
        );
    }

    #[test]
    fn test_write_git_identity_noop_when_name_none() {
        let root = tempfile::tempdir().expect("tempdir");
        let bare = root.path().join("partial.git");
        make_bare_repo(&bare);

        write_git_identity_to_repo(&bare, None, Some("bob@example.com"), None).unwrap();

        let out = Command::new("git")
            .args(["-C"])
            .arg(&bare)
            .args(["config", "--local", "user.email"])
            .output()
            .unwrap();
        assert!(
            !out.status.success(),
            "user.email should not be set locally when name is None"
        );
    }

    #[test]
    fn test_write_git_identity_is_idempotent() {
        let root = tempfile::tempdir().expect("tempdir");
        let bare = root.path().join("idem.git");
        make_bare_repo(&bare);

        write_git_identity_to_repo(&bare, Some("Carol"), Some("carol@example.com"), None).unwrap();
        write_git_identity_to_repo(&bare, Some("Carol"), Some("carol@example.com"), None).unwrap();

        let email = Command::new("git")
            .args(["-C"])
            .arg(&bare)
            .args(["config", "user.email"])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&email.stdout).trim(),
            "carol@example.com"
        );
    }

    #[test]
    fn test_write_git_identity_overwrites_existing() {
        let root = tempfile::tempdir().expect("tempdir");
        let bare = root.path().join("overwrite.git");
        make_bare_repo(&bare);

        write_git_identity_to_repo(&bare, Some("Old Name"), Some("old@example.com"), None)
            .unwrap();
        write_git_identity_to_repo(&bare, Some("New Name"), Some("new@example.com"), None)
            .unwrap();

        let name = Command::new("git")
            .args(["-C"])
            .arg(&bare)
            .args(["config", "user.name"])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&name.stdout).trim(),
            "New Name",
            "user.name should be overwritten"
        );
    }
}
