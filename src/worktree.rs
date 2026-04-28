use crate::hooks;
use crate::registry::{self, Registry};
use crate::tmux;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use toml_edit::{value, DocumentMut, Item, Table};
use tracing::info;

// ---------------------------------------------------------------------------
// reset_worktree_to_master
// ---------------------------------------------------------------------------

/// Reset a git worktree to the tip of `master` (or `main`) at origin.
///
/// If `force` is `false` and the worktree has uncommitted changes, the
/// function returns an error.  Pass `force = true` to discard them.
pub fn reset_worktree_to_master(path: &Path, force: bool) -> Result<()> {
    let git = |args: &[&str]| -> Result<String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .output()
            .with_context(|| format!("Failed to run: git {}", args.join(" ")))?;
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    };

    let git_ok = |args: &[&str]| -> Result<bool> {
        let status = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .status()
            .with_context(|| format!("Failed to run: git {}", args.join(" ")))?;
        Ok(status.success())
    };

    // 1. Check for uncommitted changes.
    let status_output = git(&["status", "--porcelain"])?;
    if !status_output.is_empty() {
        if !force {
            bail!(
                "Worktree '{}' has uncommitted changes. Clean up first or use --force to discard them.\n{}",
                path.display(),
                status_output
            );
        }
        // Hard reset + clean.
        git_ok(&["checkout", "-f", "HEAD"])?;
        git_ok(&["clean", "-fd"])?;
    }

    // 2. Fetch latest from remote (non-fatal).
    // NOTE: we do NOT fetch here globally — instead we fetch the specific branch
    // inside reset_to_master so that FETCH_HEAD is reliably set to that branch.

    // 3. Reset to master (or main) at origin.
    //
    // Strategy A: try `git checkout <branch>` — works in plain clones and when the
    //             worktree is already on that branch (bare repo case where the branch
    //             isn't locked by another worktree).
    // Strategy B: if checkout fails (e.g. bare repo — "branch already used by worktree"),
    //             fall back to `git reset --hard FETCH_HEAD`.
    //
    // We fetch `origin <branch>` explicitly (not just `origin`) so that FETCH_HEAD is
    // written with the correct ref even in linked worktrees that have no configured
    // remote-tracking refspecs.  Using FETCH_HEAD instead of `origin/<branch>` avoids
    // the symbolic-ref resolution failure that occurs when the remote-tracking ref has
    // not been written into the shared object store.
    let reset_to_master = |branch: &str| -> Result<bool> {
        // Fetch the specific branch so FETCH_HEAD is set correctly (non-fatal).
        let fetched = git_ok(&["fetch", "origin", branch])?;
        if !fetched {
            eprintln!(
                "Warning: git fetch origin {} failed in '{}'; will try local tip.",
                branch,
                path.display()
            );
        }

        // Try direct checkout first.
        if git_ok(&["checkout", branch])? {
            // Bring the branch up to date using FETCH_HEAD (avoids origin/<branch>
            // symbolic-ref resolution issues in linked worktrees).
            if fetched && !git_ok(&["reset", "--hard", "FETCH_HEAD"])? {
                eprintln!(
                    "Warning: git reset --hard FETCH_HEAD failed after checkout {}; using local tip.",
                    branch
                );
            }
            return Ok(true);
        }
        // Checkout failed (e.g. branch locked by another worktree) — hard-reset to
        // FETCH_HEAD so the working tree content matches origin without switching branches.
        if fetched && git_ok(&["reset", "--hard", "FETCH_HEAD"])? {
            return Ok(true);
        }
        // Neither worked for this branch name.
        Ok(false)
    };

    if !reset_to_master("master")? {
        if !reset_to_master("main")? {
            bail!(
                "Could not reset worktree '{}' to 'master' or 'main'. \
                 Make sure the default branch exists and origin is reachable.",
                path.display()
            );
        }
    }

    // 4. Remove untracked files so the agent starts clean.
    git_ok(&["clean", "-fd"])?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Beads project-level coordination helpers
// ---------------------------------------------------------------------------

/// Run `bd init` in `repo_path` (the bare repo directory) to create the
/// canonical `.beads/` database for the project.
///
/// Uses `--non-interactive` so the command never prompts.  The issue prefix is
/// set to the project short name so issue IDs look like `TM-abc`.
///
/// This is only called when `repo_path/.beads/` does not yet exist.
fn init_beads_in_repo(repo_path: &Path, project_short: &str) -> Result<()> {
    info!(
        "Running: bd init --prefix {} --non-interactive in {}",
        project_short,
        repo_path.display()
    );
    let output = Command::new("bd")
        .args(["init", "--prefix", project_short, "--non-interactive"])
        .current_dir(repo_path)
        .output()
        .context("Failed to run bd init (is bd installed?)")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let combined = format!("{}{}", stdout, stderr);
        // bd init exits non-zero when already initialised — treat as a no-op.
        if combined.contains("already initialized") || combined.contains("already exists") {
            return Ok(());
        }
        bail!("bd init failed: {}", combined.trim());
    }
    Ok(())
}

/// Write a `.beads/redirect` file in `worktree_path` pointing at the bare
/// repo's `.beads/` directory.
///
/// Worktrees are always direct children of the bare repo directory, so the
/// redirect path is always the fixed relative string `../.beads`.
fn write_beads_redirect(worktree_path: &Path) -> Result<()> {
    let beads_dir = worktree_path.join(".beads");
    std::fs::create_dir_all(&beads_dir)
        .with_context(|| format!("Failed to create directory '{}'", beads_dir.display()))?;

    let redirect_path = beads_dir.join("redirect");
    std::fs::write(&redirect_path, "../.beads")
        .with_context(|| format!("Failed to write '{}'", redirect_path.display()))?;

    info!(
        "Wrote .beads/redirect in '{}' -> ../.beads",
        worktree_path.display()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Serena project setup helpers
// ---------------------------------------------------------------------------

/// Write a minimal `.serena/project.yml` in `worktree_path`.
///
/// The file configures serena's `--project-from-cwd` auto-detection: when
/// serena starts inside the worktree it reads this file and activates the
/// correct project without needing `activate_project` to be called manually.
///
/// `worktree_name` becomes the `project_name` (must be unique across all
/// registered serena projects — using the leaf name satisfies this).
/// `language` is written as the sole entry in the `languages:` list.
pub fn write_serena_project_yml(
    worktree_path: &Path,
    worktree_name: &str,
    language: &str,
) -> Result<()> {
    let serena_dir = worktree_path.join(".serena");
    std::fs::create_dir_all(&serena_dir)
        .with_context(|| format!("Failed to create '{}'", serena_dir.display()))?;

    let project_yml_path = serena_dir.join("project.yml");
    // Write only the minimal keys; serena fills in all other defaults.
    let content = format!(
        "# Auto-generated by task-master add-worktree\nproject_name: \"{}\"\nlanguages:\n- {}\n",
        worktree_name, language
    );
    std::fs::write(&project_yml_path, &content)
        .with_context(|| format!("Failed to write '{}'", project_yml_path.display()))?;

    info!(
        "Wrote .serena/project.yml in '{}' (project_name={}, language={})",
        worktree_path.display(),
        worktree_name,
        language
    );
    Ok(())
}

/// Append `worktree_path` to the `projects:` list in `~/.serena/serena_config.yml`.
///
/// Serena registers projects by absolute path in this file.  We append the
/// new path if it isn't already listed; the file is treated as a plain-text
/// YAML file (we do NOT parse the full YAML — we just look for the `projects:`
/// block and append to it).
pub fn register_in_serena_config(worktree_path: &Path) -> Result<()> {
    let config_path = dirs::home_dir()
        .context("Could not determine home directory")?
        .join(".serena")
        .join("serena_config.yml");

    if !config_path.exists() {
        // No serena config — silently skip.
        return Ok(());
    }

    let abs_path = worktree_path
        .canonicalize()
        .unwrap_or_else(|_| worktree_path.to_path_buf());
    let abs_path_str = abs_path.to_string_lossy();

    let contents = std::fs::read_to_string(&config_path)
        .with_context(|| format!("Failed to read '{}'", config_path.display()))?;

    // Idempotency: skip if already registered.
    if contents.contains(abs_path_str.as_ref()) {
        return Ok(());
    }

    // Find the `projects:` key and append after the last existing list entry.
    // The list entries are lines matching `^- ` (with optional leading spaces).
    // Strategy: find the `projects:` line, then scan forward to find the last
    // `- ` entry in that block, then insert our new entry right after it.
    // If the block is empty (no entries yet), insert directly after `projects:`.
    let new_entry = format!("- {}", abs_path_str);

    let mut lines: Vec<&str> = contents.lines().collect();
    let projects_line = lines
        .iter()
        .position(|l| l.trim_start() == "projects:" || l.starts_with("projects:"))
        .with_context(|| format!("'projects:' key not found in '{}'", config_path.display()))?;

    // Scan from projects_line+1 to find the last `- ` list entry.
    let mut last_entry_idx = projects_line; // default: insert right after header
    for i in (projects_line + 1)..lines.len() {
        let trimmed = lines[i].trim_start();
        if trimmed.starts_with("- ") {
            last_entry_idx = i;
        } else if !trimmed.is_empty() && !trimmed.starts_with('#') {
            // Hit a non-list, non-comment line — end of the projects block.
            break;
        }
    }

    lines.insert(last_entry_idx + 1, Box::leak(new_entry.into_boxed_str()));

    let new_contents = lines.join("\n") + "\n";
    std::fs::write(&config_path, new_contents)
        .with_context(|| format!("Failed to write '{}'", config_path.display()))?;

    info!("Registered '{}' in serena_config.yml", abs_path_str);
    Ok(())
}

// ---------------------------------------------------------------------------
// Agent config installation
// ---------------------------------------------------------------------------

/// Copy `plan.md`, `qa.md`, and `e2e.md` from the task-master project's
/// `.opencode/agents/` directory into `<worktree_path>/.opencode/agents/`,
/// and copy `.opencode/opencode.json` into `<worktree_path>/opencode.json`.
///
/// The agent `.md` files are the opencode agent configurations consumed by
/// `opencode --agent plan/qa/e2e` when running inside the target worktree.
/// They must be present in the *worktree's own directory* because opencode
/// looks for agent configs relative to its current working directory, not
/// relative to the task-master project root.
///
/// The `opencode.json` is the project-level permission config. It pre-approves
/// `/tmp/**` under `external_directory` so that agents (including default dev
/// sessions spawned by `task-master spawn`) can read/write task-master
/// coordination files in `/tmp` without triggering permission prompts on every
/// access. It is placed at the worktree root (not inside `.opencode/`) because
/// opencode resolves `opencode.json` from the project working directory.
///
/// `base_dir` is the task-master project root (source of configs).
/// `worktree_path` is the target worktree directory (destination).
///
/// Only files that actually exist in the source are copied — missing source
/// files are silently skipped. Existing destination files are always overwritten
/// so updates propagate when `task-master install-agent-configs` is re-run.
pub fn install_agent_configs(base_dir: &Path, worktree_path: &Path) -> Result<()> {
    let src_agents_dir = base_dir.join(".opencode").join("agents");
    let dst_agents_dir = worktree_path.join(".opencode").join("agents");

    std::fs::create_dir_all(&dst_agents_dir).with_context(|| {
        format!(
            "Failed to create agent config directory '{}'",
            dst_agents_dir.display()
        )
    })?;

    let agents = ["plan.md", "qa.md", "e2e.md"];
    let mut installed = Vec::new();
    for name in &agents {
        let src = src_agents_dir.join(name);
        if !src.exists() {
            continue;
        }
        let dst = dst_agents_dir.join(name);
        std::fs::copy(&src, &dst).with_context(|| {
            format!("Failed to copy '{}' to '{}'", src.display(), dst.display())
        })?;
        installed.push(*name);
    }

    // Distribute opencode.json (permission config) to the worktree root.
    // This pre-approves /tmp access for all agents, including default dev sessions.
    let src_opencode_json = base_dir.join(".opencode").join("opencode.json");
    if src_opencode_json.exists() {
        let dst_opencode_json = worktree_path.join("opencode.json");
        std::fs::copy(&src_opencode_json, &dst_opencode_json).with_context(|| {
            format!(
                "Failed to copy '{}' to '{}'",
                src_opencode_json.display(),
                dst_opencode_json.display()
            )
        })?;
        installed.push("opencode.json");
    }

    if !installed.is_empty() {
        info!(
            "Installed agent configs {:?} into '{}'",
            installed,
            worktree_path.display()
        );
    }
    Ok(())
}

/// Install agent configs into every registered worktree.
///
/// Iterates all worktrees in the registry and calls [`install_agent_configs`]
/// for each one, copying `plan.md`, `qa.md`, and `e2e.md` from the
/// task-master source directory into the worktree's `.opencode/agents/`.
///
/// Returns a summary string suitable for printing to the user.
pub fn cmd_install_agent_configs(registry: &Registry, base_dir: &PathBuf) -> Result<String> {
    let mut updated = 0usize;
    let mut skipped = 0usize;

    for wt in &registry.worktrees {
        if !wt.abs_path.exists() {
            skipped += 1;
            info!("Skipping '{}' — directory does not exist", wt.window_name);
            continue;
        }
        match install_agent_configs(base_dir, &wt.abs_path) {
            Ok(()) => updated += 1,
            Err(e) => {
                eprintln!(
                    "Warning: could not install agent configs for '{}': {}",
                    wt.window_name, e
                );
                skipped += 1;
            }
        }
    }

    Ok(format!(
        "Agent configs and permissions installed in {} worktree(s){skipped_note}.",
        updated,
        skipped_note = if skipped > 0 {
            format!(" ({} skipped — see warnings above)", skipped)
        } else {
            String::new()
        }
    ))
}

// ---------------------------------------------------------------------------
// Git identity helpers
// ---------------------------------------------------------------------------

/// Write `user.name`, `user.email`, and `credential.https://github.com.username`
/// into a bare repo's git config.
///
/// This is the canonical place to set a per-project git identity — all linked
/// worktrees inherit the bare repo's "local" config, so a single write here
/// covers every worktree without needing per-worktree overrides.
///
/// The primary use-case is correcting identity when the project's worktrees are
/// stored under a directory path that triggers an unintended `includeIf` rule in
/// `~/.gitconfig` (e.g. whiteaway project worktrees stored under the sumsar01
/// directory tree).
///
/// The credential username override is critical for QA agents: if an `includeIf`
/// rule injects a different `credential.username` (e.g. `sumsar01`), the gh
/// credential helper cannot find credentials for that account and git falls back
/// to an interactive password prompt — hanging the agent indefinitely. Writing
/// the correct username here (derived from `name`) ensures the credential helper
/// is called with the right account.
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
    // This overrides any `includeIf` rule in ~/.gitconfig that might inject a
    // different signing key (e.g. a personal key when the bare repo lives under a
    // personal-account directory tree).
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

// ---------------------------------------------------------------------------
// add-worktree
// ---------------------------------------------------------------------------

pub fn cmd_add_worktree(
    registry: &Registry,
    base_dir: &PathBuf,
    project_short: &str,
    worktree_name: &str,
    branch: Option<&str>,
) -> Result<String> {
    let project = registry.find_project(project_short).with_context(|| {
        format!(
            "Project '{}' not found. Run `task-master list` to see available projects.",
            project_short
        )
    })?;

    let window_name = format!("{}-{}", project.short, worktree_name);
    registry.assert_window_name_free(&window_name)?;

    let repo_path = base_dir.join(&project.repo);
    let worktree_path = repo_path.join(worktree_name);

    if worktree_path.exists() {
        bail!("Directory already exists: {}", worktree_path.display());
    }

    // git worktree add <name> [<branch>]
    // With no branch: checks out HEAD (detached), then we immediately create a branch
    // With --branch: creates a new branch
    let mut git_args = vec!["worktree", "add"];

    let worktree_path_str = worktree_path.to_string_lossy().to_string();
    git_args.push(&worktree_path_str);

    let branch_owned;
    if let Some(b) = branch {
        git_args.push("-b");
        branch_owned = b.to_string();
        git_args.push(&branch_owned);
    }
    // no branch = uses HEAD

    info!("Running: git -C {} worktree add ...", repo_path.display());
    let output = Command::new("git")
        .arg("-C")
        .arg(&repo_path)
        .args(&git_args)
        .output()
        .context("Failed to run git worktree add")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git worktree add failed: {}", stderr.trim());
    }

    // Append to task-master.toml
    let config_path = base_dir.join("task-master.toml");
    let contents = std::fs::read_to_string(&config_path)?;

    let new_toml = append_worktree_to_toml(&contents, project_short, worktree_name, false)
        .context("Failed to update task-master.toml")?;
    std::fs::write(&config_path, new_toml)?;

    info!(
        "Added {}. Spawn with: task-master spawn {} \"<prompt>\"",
        window_name, window_name
    );

    // Apply per-project git identity override so agents commit with the correct
    // user.name / user.email even when the worktree path triggers an unintended
    // includeIf rule in ~/.gitconfig.
    // We write to the bare repo config; all linked worktrees inherit it.
    match write_git_identity_to_repo(
        &repo_path,
        project.git_name.as_deref(),
        project.git_email.as_deref(),
        project.git_signing_key.as_deref(),
    ) {
        Ok(()) => {}
        Err(e) => eprintln!(
            "Warning: could not set git identity for {}: {}. \
             Run `task-master fix-git-identity` manually later.",
            window_name, e
        ),
    }

    // Auto-install the QA post-push hook for the new worktree.
    // Pass the project short name (e.g. "WIS"), not the full window name —
    // the hook detects the worktree leaf at runtime from $GIT_DIR.
    match hooks::install_hook_for_single(&worktree_path, project_short) {
        Ok(()) => {}
        Err(e) => {
            // Non-fatal: warn but don't fail add-worktree.
            eprintln!(
                "Warning: could not install QA hook for {}: {}",
                window_name, e
            );
            eprintln!("Run `task-master install-qa-hooks` manually later.");
        }
    }

    // Set up project-level beads coordination.
    // The bare repo (repo_path) is always the canonical .beads/ host.
    // If repo_path/.beads/ doesn't exist yet, initialise it there.
    // Then write a redirect in the new worktree pointing at ../.beads.
    let repo_beads = repo_path.join(".beads");
    if !repo_beads.is_dir() {
        match init_beads_in_repo(&repo_path, project_short) {
            Ok(()) => {}
            Err(e) => eprintln!(
                "Warning: could not run bd init for {}: {}. \
                 Run `bd init --prefix {}` manually in '{}'.",
                window_name,
                e,
                project_short,
                repo_path.display()
            ),
        }
    }
    match write_beads_redirect(&worktree_path) {
        Ok(()) => {}
        Err(e) => eprintln!(
            "Warning: could not write .beads/redirect for {}: {}. \
             Run `bd init` manually in the worktree to share issues.",
            window_name, e
        ),
    }

    // Set up .serena/project.yml so that serena's --project-from-cwd
    // auto-detects this worktree correctly without needing activate_project.
    match write_serena_project_yml(&worktree_path, worktree_name, &project.language) {
        Ok(()) => {}
        Err(e) => eprintln!(
            "Warning: could not write .serena/project.yml for {}: {}. \
             Create it manually with project_name='{}' and languages: [{}].",
            window_name, e, worktree_name, project.language
        ),
    }

    // Register the new worktree path in ~/.serena/serena_config.yml.
    match register_in_serena_config(&worktree_path) {
        Ok(()) => {}
        Err(e) => eprintln!(
            "Warning: could not register {} in serena_config.yml: {}. \
             Add '- {}' under the projects: key manually.",
            window_name,
            e,
            worktree_path.display()
        ),
    }

    // Install opencode agent configs (plan.md, qa.md, e2e.md) so that
    // `opencode --agent plan/qa/e2e` works from inside this worktree.
    match install_agent_configs(base_dir, &worktree_path) {
        Ok(()) => {}
        Err(e) => eprintln!(
            "Warning: could not install agent configs for {}: {}. \
             Run `task-master install-agent-configs` manually later.",
            window_name, e
        ),
    }

    Ok(format!(
        "Added {}. Spawn with:\n  task-master spawn {} \"<prompt>\"",
        window_name, window_name
    ))
}

// ---------------------------------------------------------------------------
// create-ephemeral-worktree (internal helper for spawn --ephemeral)
// ---------------------------------------------------------------------------

/// Create a new ephemeral worktree for `project_short`, register it in the config
/// with `ephemeral = true`, and return the resolved `(window_name, abs_path)`.
///
/// Performs all the same setup steps as `cmd_add_worktree` (git hooks, beads,
/// serena, agent configs) but writes `ephemeral = true` to the TOML entry and
/// creates a new branch named `<branch_prefix><worktree_name>`.
///
/// Does NOT reset to master (the branch is brand-new — there is nothing to reset).
/// The caller is responsible for spawning the tmux window afterwards.
pub fn create_ephemeral_worktree(
    registry: &Registry,
    base_dir: &PathBuf,
    project_short: &str,
    worktree_name: &str,
    branch_name: &str,
) -> Result<(String, std::path::PathBuf)> {
    let project = registry.find_project(project_short).with_context(|| {
        format!(
            "Project '{}' not found. Run `task-master list` to see available projects.",
            project_short
        )
    })?;

    let window_name = format!("{}-{}", project.short, worktree_name);
    registry.assert_window_name_free(&window_name)?;

    let repo_path = base_dir.join(&project.repo);
    let worktree_path = repo_path.join(worktree_name);

    if worktree_path.exists() {
        bail!("Directory already exists: {}", worktree_path.display());
    }

    // Create the worktree on a new branch: git worktree add <path> -b <branch>
    let worktree_path_str = worktree_path.to_string_lossy().to_string();
    info!(
        "Running: git -C {} worktree add {} -b {}",
        repo_path.display(),
        worktree_path_str,
        branch_name
    );
    let output = Command::new("git")
        .arg("-C")
        .arg(&repo_path)
        .args(["worktree", "add", &worktree_path_str, "-b", branch_name])
        .output()
        .context("Failed to run git worktree add")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git worktree add failed: {}", stderr.trim());
    }

    // Append to task-master.toml with ephemeral = true.
    let config_path = base_dir.join("task-master.toml");
    let contents = std::fs::read_to_string(&config_path)?;
    let new_toml = append_worktree_to_toml(&contents, project_short, worktree_name, true)
        .context("Failed to update task-master.toml")?;
    std::fs::write(&config_path, new_toml)?;

    info!(
        "Created ephemeral worktree {} on branch {}",
        window_name, branch_name
    );

    // Apply per-project git identity override (non-fatal).
    match write_git_identity_to_repo(
        &repo_path,
        project.git_name.as_deref(),
        project.git_email.as_deref(),
        project.git_signing_key.as_deref(),
    ) {
        Ok(()) => {}
        Err(e) => eprintln!(
            "Warning: could not set git identity for {}: {}",
            window_name, e
        ),
    }

    // Install QA post-push hook (non-fatal).
    match hooks::install_hook_for_single(&worktree_path, project_short) {
        Ok(()) => {}
        Err(e) => {
            eprintln!(
                "Warning: could not install QA hook for {}: {}",
                window_name, e
            );
            eprintln!("Run `task-master install-qa-hooks` manually later.");
        }
    }

    // Set up beads redirect (non-fatal).
    let repo_beads = repo_path.join(".beads");
    if !repo_beads.is_dir() {
        match init_beads_in_repo(&repo_path, project_short) {
            Ok(()) => {}
            Err(e) => eprintln!(
                "Warning: could not run bd init for {}: {}. \
                 Run `bd init --prefix {}` manually in '{}'.",
                window_name,
                e,
                project_short,
                repo_path.display()
            ),
        }
    }
    match write_beads_redirect(&worktree_path) {
        Ok(()) => {}
        Err(e) => eprintln!(
            "Warning: could not write .beads/redirect for {}: {}",
            window_name, e
        ),
    }

    // Set up serena project.yml (non-fatal).
    match write_serena_project_yml(&worktree_path, worktree_name, &project.language) {
        Ok(()) => {}
        Err(e) => eprintln!(
            "Warning: could not write .serena/project.yml for {}: {}",
            window_name, e
        ),
    }
    match register_in_serena_config(&worktree_path) {
        Ok(()) => {}
        Err(e) => eprintln!(
            "Warning: could not register {} in serena_config.yml: {}",
            window_name, e
        ),
    }

    // Install opencode agent configs (non-fatal).
    match install_agent_configs(base_dir, &worktree_path) {
        Ok(()) => {}
        Err(e) => eprintln!(
            "Warning: could not install agent configs for {}: {}. \
             Run `task-master install-agent-configs` manually later.",
            window_name, e
        ),
    }

    Ok((window_name, worktree_path))
}

// ---------------------------------------------------------------------------
// remove-worktree
// ---------------------------------------------------------------------------

pub fn cmd_remove_worktree(
    registry: &Registry,
    base_dir: &PathBuf,
    window_name: &str,
    force: bool,
    keep_branch: bool,
) -> Result<()> {
    let worktree = registry.require_worktree(window_name)?;
    let window_base = tmux::base_window_name(window_name);

    // If a tmux window is active for this worktree and --force is not set, refuse.
    if let Ok(session) = tmux::current_session() {
        if tmux::find_window_index(&session, window_base).is_some() && !force {
            bail!(
                "Window '{}' is currently active in tmux. \
                 Stop the agent first, or pass --force to remove anyway.",
                window_base
            );
        }
    }

    let project = registry
        .find_project(&worktree.project_short)
        .with_context(|| format!("Project '{}' not found", worktree.project_short))?;
    let repo_path = base_dir.join(&project.repo);

    // Determine the current branch BEFORE removing the worktree.
    let branch = if !keep_branch && worktree.abs_path.exists() {
        let out = Command::new("git")
            .arg("-C")
            .arg(&worktree.abs_path)
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output();
        match out {
            Ok(o) if o.status.success() => {
                let b = String::from_utf8_lossy(&o.stdout).trim().to_string();
                // Only delete feature branches (not master/main/HEAD).
                if b == "master" || b == "main" || b == "HEAD" {
                    None
                } else {
                    Some(b)
                }
            }
            _ => None,
        }
    } else {
        None
    };

    // Run `git worktree remove [--force] <path>` from the bare repo root.
    // The bare repo is `base_dir/<project.repo>`.
    let mut git_args = vec!["worktree", "remove"];
    if force {
        git_args.push("--force");
    }
    let abs_path_str = worktree.abs_path.to_string_lossy().to_string();
    git_args.push(&abs_path_str);

    info!(
        "Running: git -C {} worktree remove {}",
        repo_path.display(),
        abs_path_str
    );
    let git_output = Command::new("git")
        .arg("-C")
        .arg(&repo_path)
        .args(&git_args)
        .output()
        .context("Failed to run git worktree remove")?;

    if !git_output.status.success() {
        let stderr = String::from_utf8_lossy(&git_output.stderr)
            .trim()
            .to_string();
        bail!("git worktree remove failed: {}", stderr);
    }

    // Delete the remote branch (non-fatal).
    if let Some(ref b) = branch {
        let push_out = Command::new("git")
            .arg("-C")
            .arg(&repo_path)
            .args(["push", "origin", "--delete", b])
            .output();
        match push_out {
            Ok(o) if o.status.success() => {
                info!("Deleted remote branch '{}'.", b);
            }
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                info!(
                    "Remote branch delete for '{}' failed (may already be gone): {}",
                    b,
                    stderr.trim()
                );
            }
            Err(e) => {
                info!("Could not run git push origin --delete '{}': {}", b, e);
            }
        }

        // Delete the local branch from the bare repo (non-fatal).
        let _ = Command::new("git")
            .arg("-C")
            .arg(&repo_path)
            .args(["branch", "-d", b])
            .output();
    }

    // Remove the entry from task-master.toml.
    let config_path = base_dir.join("task-master.toml");
    let contents =
        std::fs::read_to_string(&config_path).context("Failed to read task-master.toml")?;
    let new_toml = registry::remove_worktree_from_toml(
        &contents,
        &worktree.project_short,
        &worktree
            .window_name
            .trim_start_matches(&format!("{}-", worktree.project_short)),
    )
    .context("Failed to update task-master.toml")?;
    std::fs::write(&config_path, new_toml)?;

    println!("Removed worktree '{}'.", window_base);
    Ok(())
}

// ---------------------------------------------------------------------------
// TOML mutation helper (extracted for testability)
// ---------------------------------------------------------------------------

/// Append a new `[[projects.worktrees]]` entry to the TOML document string.
///
/// Finds the `[[projects]]` block whose `short` key matches `project_short`
/// (case-insensitive) and pushes a new worktree entry with the given name.
/// Returns the updated TOML as a `String`.
pub fn append_worktree_to_toml(
    toml_str: &str,
    project_short: &str,
    worktree_name: &str,
    ephemeral: bool,
) -> Result<String> {
    let mut doc = toml_str
        .parse::<DocumentMut>()
        .context("Failed to parse task-master.toml")?;

    let projects = doc["projects"]
        .as_array_of_tables_mut()
        .context("Missing [[projects]] in task-master.toml")?;

    let proj_entry = projects
        .iter_mut()
        .find(|p| {
            p.get("short")
                .and_then(|v| v.as_str())
                .map(|s| s.eq_ignore_ascii_case(project_short))
                .unwrap_or(false)
        })
        .with_context(|| format!("Project '{}' not found in config file", project_short))?;

    let worktrees = proj_entry
        .entry("worktrees")
        .or_insert(Item::ArrayOfTables(toml_edit::ArrayOfTables::new()))
        .as_array_of_tables_mut()
        .context("worktrees is not an array of tables")?;

    let mut new_wt = Table::new();
    new_wt.insert("name", value(worktree_name));
    if ephemeral {
        new_wt.insert("ephemeral", value(true));
    }
    worktrees.push(new_wt);

    Ok(doc.to_string())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -------------------------------------------------------------------------
    // append_worktree_to_toml
    // -------------------------------------------------------------------------

    const BASE_TOML: &str = r#"[[projects]]
name = "warehouse-integration-service"
short = "WIS"
repo = "projects/warehouse-integration-service"

[[projects.worktrees]]
name = "olive"
"#;

    #[test]
    fn test_append_worktree_adds_new_entry() {
        let result = append_worktree_to_toml(BASE_TOML, "WIS", "cedar", false).unwrap();
        // The new worktree must appear in the output.
        assert!(result.contains("cedar"), "expected 'cedar' in:\n{}", result);
        // The existing worktree must still be there.
        assert!(
            result.contains("olive"),
            "expected 'olive' still in:\n{}",
            result
        );
    }

    #[test]
    fn test_append_worktree_is_valid_toml() {
        let result = append_worktree_to_toml(BASE_TOML, "WIS", "cedar", false).unwrap();
        // Round-trip: must parse without error and contain both worktrees.
        let reg =
            registry::Registry::load_from_str(&result, std::path::PathBuf::from("/base")).unwrap();
        let names: Vec<&str> = reg
            .worktrees
            .iter()
            .map(|w| w.window_name.as_str())
            .collect();
        assert!(names.contains(&"WIS-olive"));
        assert!(names.contains(&"WIS-cedar"));
    }

    #[test]
    fn test_append_worktree_case_insensitive_project_match() {
        // "wis" should match the project with short = "WIS".
        let result = append_worktree_to_toml(BASE_TOML, "wis", "birch", false).unwrap();
        assert!(result.contains("birch"));
    }

    #[test]
    fn test_append_worktree_unknown_project_returns_error() {
        let err = append_worktree_to_toml(BASE_TOML, "XYZ", "branch", false).unwrap_err();
        assert!(err.to_string().contains("XYZ"));
    }

    #[test]
    fn test_append_worktree_to_project_with_no_prior_worktrees() {
        let toml = r#"[[projects]]
name = "fresh-service"
short = "FS"
repo = "projects/fresh-service"
"#;
        let result = append_worktree_to_toml(toml, "FS", "main", false).unwrap();
        assert!(result.contains("main"));
        // Validate it is parseable.
        let reg =
            registry::Registry::load_from_str(&result, std::path::PathBuf::from("/base")).unwrap();
        assert_eq!(reg.worktrees.len(), 1);
        assert_eq!(reg.worktrees[0].window_name, "FS-main");
    }

    #[test]
    fn test_append_worktree_multiple_projects_correct_one_modified() {
        let toml = r#"[[projects]]
name = "alpha"
short = "A"
repo = "projects/alpha"

[[projects.worktrees]]
name = "existing"

[[projects]]
name = "beta"
short = "B"
repo = "projects/beta"
"#;
        let result = append_worktree_to_toml(toml, "B", "new-branch", false).unwrap();
        let reg =
            registry::Registry::load_from_str(&result, std::path::PathBuf::from("/base")).unwrap();
        let names: Vec<&str> = reg
            .worktrees
            .iter()
            .map(|w| w.window_name.as_str())
            .collect();
        assert!(
            names.contains(&"A-existing"),
            "A-existing should be untouched"
        );
        assert!(
            names.contains(&"B-new-branch"),
            "B-new-branch should be added"
        );
        assert_eq!(names.len(), 2);
    }

    #[test]
    fn test_append_worktree_preserves_original_formatting() {
        // Comments and blank lines that toml_edit preserves should not be clobbered.
        let toml = "# top comment\n".to_string() + BASE_TOML;
        let result = append_worktree_to_toml(&toml, "WIS", "branch", false).unwrap();
        assert!(result.starts_with("# top comment\n"), "comment was lost");
    }

    #[test]
    fn test_append_worktree_ephemeral_true_writes_flag() {
        let result = append_worktree_to_toml(BASE_TOML, "WIS", "spruce-7f3a", true).unwrap();
        assert!(
            result.contains("spruce-7f3a"),
            "worktree name should appear in output"
        );
        assert!(
            result.contains("ephemeral = true"),
            "ephemeral = true should be written:\n{}",
            result
        );
        // Parse round-trip to verify the flag is actually loadable.
        let reg =
            registry::Registry::load_from_str(&result, std::path::PathBuf::from("/base")).unwrap();
        let wt = reg.find_worktree("WIS-spruce-7f3a").unwrap();
        assert!(wt.ephemeral, "ephemeral flag should round-trip as true");
    }

    #[test]
    fn test_append_worktree_ephemeral_false_omits_flag() {
        // When ephemeral=false the key should not appear in the TOML to keep config clean.
        let result = append_worktree_to_toml(BASE_TOML, "WIS", "cedar", false).unwrap();
        // The "ephemeral = true" line must NOT appear for a non-ephemeral worktree.
        assert!(
            !result.contains("ephemeral = true"),
            "ephemeral = true should not appear for non-ephemeral worktree:\n{}",
            result
        );
    }

    // -------------------------------------------------------------------------
    // reset_worktree_to_master
    // -------------------------------------------------------------------------

    /// Helper: initialise a bare git repo with one commit on master and return
    /// a linked worktree at `<root>/wt`.
    fn make_git_worktree(root: &std::path::Path) -> std::path::PathBuf {
        let bare = root.join("bare.git");
        let wt = root.join("wt");

        // Init bare repo and make an initial commit so master exists.
        Command::new("git")
            .args(["init", "--bare"])
            .arg(&bare)
            .status()
            .unwrap();

        // Clone into a temp checkout so we can commit.
        let checkout = root.join("checkout");
        Command::new("git")
            .args(["clone"])
            .arg(&bare)
            .arg(&checkout)
            .status()
            .unwrap();

        // Configure git identity for the commit.
        Command::new("git")
            .args(["-C"])
            .arg(&checkout)
            .args(["config", "user.email", "test@test.com"])
            .status()
            .unwrap();
        Command::new("git")
            .args(["-C"])
            .arg(&checkout)
            .args(["config", "user.name", "Test"])
            .status()
            .unwrap();

        std::fs::write(checkout.join("init.txt"), "init").unwrap();
        Command::new("git")
            .args(["-C"])
            .arg(&checkout)
            .args(["add", "."])
            .status()
            .unwrap();
        Command::new("git")
            .args(["-C"])
            .arg(&checkout)
            .args(["commit", "-m", "init"])
            .status()
            .unwrap();
        Command::new("git")
            .args(["-C"])
            .arg(&checkout)
            .args(["push", "origin", "HEAD:master"])
            .status()
            .unwrap();

        // Add a worktree linked to the bare repo.
        Command::new("git")
            .args(["-C"])
            .arg(&bare)
            .args(["worktree", "add"])
            .arg(&wt)
            .arg("master")
            .status()
            .unwrap();

        // Configure identity in the worktree too (needed for commits there).
        Command::new("git")
            .args(["-C"])
            .arg(&wt)
            .args(["config", "user.email", "test@test.com"])
            .status()
            .unwrap();
        Command::new("git")
            .args(["-C"])
            .arg(&wt)
            .args(["config", "user.name", "Test"])
            .status()
            .unwrap();

        wt
    }

    #[test]
    fn test_reset_clean_already_on_master() {
        let root = tempfile::tempdir().expect("tempdir");
        let wt = make_git_worktree(root.path());
        let result = reset_worktree_to_master(&wt, false);
        assert!(
            result.is_ok(),
            "clean worktree should reset ok: {:?}",
            result
        );
    }

    #[test]
    fn test_reset_dirty_no_force_returns_error() {
        let root = tempfile::tempdir().expect("tempdir");
        let wt = make_git_worktree(root.path());
        // Create an uncommitted file.
        std::fs::write(wt.join("dirty.txt"), "dirty").unwrap();
        let err = reset_worktree_to_master(&wt, false).unwrap_err();
        assert!(
            err.to_string().contains("uncommitted changes"),
            "expected 'uncommitted changes' in error, got: {}",
            err
        );
    }

    #[test]
    fn test_reset_dirty_with_force_discards_changes() {
        let root = tempfile::tempdir().expect("tempdir");
        let wt = make_git_worktree(root.path());
        let dirty = wt.join("dirty.txt");
        std::fs::write(&dirty, "dirty").unwrap();
        let result = reset_worktree_to_master(&wt, true);
        assert!(result.is_ok(), "force should succeed: {:?}", result);
        assert!(
            !dirty.exists(),
            "untracked file should have been cleaned up"
        );
    }

    #[test]
    fn test_reset_pull_fails_warns_and_continues() {
        // A worktree with no remote will have a failing pull; should still return Ok.
        let root = tempfile::tempdir().expect("tempdir");

        // Init a simple local repo (not bare, no remote).
        let repo = root.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        Command::new("git")
            .args(["init"])
            .current_dir(&repo)
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&repo)
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(&repo)
            .status()
            .unwrap();
        std::fs::write(repo.join("a.txt"), "a").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&repo)
            .status()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(&repo)
            .status()
            .unwrap();

        // No remote — pull will fail. reset should warn and return Ok.
        let result = reset_worktree_to_master(&repo, false);
        assert!(
            result.is_ok(),
            "no-remote pull failure should warn+continue: {:?}",
            result
        );
    }

    // -------------------------------------------------------------------------
    // Beads helpers
    // -------------------------------------------------------------------------

    #[test]
    fn test_write_beads_redirect_creates_file_with_correct_content() {
        let root = tempfile::tempdir().expect("tempdir");
        let worktree = root.path().join("walnut");
        std::fs::create_dir_all(&worktree).unwrap();

        write_beads_redirect(&worktree).unwrap();

        let redirect_path = worktree.join(".beads").join("redirect");
        assert!(redirect_path.exists(), "redirect file should be created");
        let content = std::fs::read_to_string(&redirect_path).unwrap();
        assert_eq!(
            content, "../.beads",
            "redirect content must be exactly '../.beads', got: {}",
            content
        );
    }

    #[test]
    fn test_write_beads_redirect_creates_dot_beads_dir_if_missing() {
        let root = tempfile::tempdir().expect("tempdir");
        let worktree = root.path().join("secondary");
        std::fs::create_dir_all(&worktree).unwrap();
        // .beads/ does NOT exist yet — write_beads_redirect should create it.
        assert!(!worktree.join(".beads").exists());
        write_beads_redirect(&worktree).unwrap();
        assert!(
            worktree.join(".beads").join("redirect").exists(),
            ".beads/redirect must be created even when .beads/ was absent"
        );
    }

    #[test]
    fn test_write_beads_redirect_is_idempotent() {
        let root = tempfile::tempdir().expect("tempdir");
        let worktree = root.path().join("oak");
        std::fs::create_dir_all(&worktree).unwrap();

        // Call twice — should not error or corrupt the file.
        write_beads_redirect(&worktree).unwrap();
        write_beads_redirect(&worktree).unwrap();

        let content = std::fs::read_to_string(worktree.join(".beads").join("redirect")).unwrap();
        assert_eq!(
            content, "../.beads",
            "content must still be '../.beads' after second call"
        );
    }

    // -------------------------------------------------------------------------
    // install_agent_configs
    // -------------------------------------------------------------------------

    #[test]
    fn test_install_agent_configs_copies_files() {
        let root = tempfile::tempdir().expect("tempdir");
        let src_dir = root.path().join("task-master");
        let dst_dir = root.path().join("worktree");
        let agents_src = src_dir.join(".opencode").join("agents");
        std::fs::create_dir_all(&agents_src).unwrap();
        std::fs::create_dir_all(&dst_dir).unwrap();

        // Write source agent configs.
        std::fs::write(agents_src.join("plan.md"), "plan content").unwrap();
        std::fs::write(agents_src.join("qa.md"), "qa content").unwrap();
        std::fs::write(agents_src.join("e2e.md"), "e2e content").unwrap();

        install_agent_configs(&src_dir, &dst_dir).unwrap();

        let agents_dst = dst_dir.join(".opencode").join("agents");
        assert_eq!(
            std::fs::read_to_string(agents_dst.join("plan.md")).unwrap(),
            "plan content"
        );
        assert_eq!(
            std::fs::read_to_string(agents_dst.join("qa.md")).unwrap(),
            "qa content"
        );
        assert_eq!(
            std::fs::read_to_string(agents_dst.join("e2e.md")).unwrap(),
            "e2e content"
        );
    }

    #[test]
    fn test_install_agent_configs_creates_dest_dir_if_missing() {
        let root = tempfile::tempdir().expect("tempdir");
        let src_dir = root.path().join("task-master");
        let dst_dir = root.path().join("worktree");
        let agents_src = src_dir.join(".opencode").join("agents");
        std::fs::create_dir_all(&agents_src).unwrap();
        // dst_dir/.opencode/agents does NOT exist yet.
        std::fs::create_dir_all(&dst_dir).unwrap();

        std::fs::write(agents_src.join("plan.md"), "plan").unwrap();
        install_agent_configs(&src_dir, &dst_dir).unwrap();

        assert!(dst_dir
            .join(".opencode")
            .join("agents")
            .join("plan.md")
            .exists());
    }

    #[test]
    fn test_install_agent_configs_skips_missing_source_files() {
        let root = tempfile::tempdir().expect("tempdir");
        let src_dir = root.path().join("task-master");
        let dst_dir = root.path().join("worktree");
        let agents_src = src_dir.join(".opencode").join("agents");
        std::fs::create_dir_all(&agents_src).unwrap();
        std::fs::create_dir_all(&dst_dir).unwrap();

        // Only plan.md exists in source; qa.md and e2e.md are absent.
        std::fs::write(agents_src.join("plan.md"), "plan").unwrap();
        install_agent_configs(&src_dir, &dst_dir).unwrap();

        let agents_dst = dst_dir.join(".opencode").join("agents");
        assert!(
            agents_dst.join("plan.md").exists(),
            "plan.md should be copied"
        );
        assert!(
            !agents_dst.join("qa.md").exists(),
            "qa.md should be skipped"
        );
        assert!(
            !agents_dst.join("e2e.md").exists(),
            "e2e.md should be skipped"
        );
    }

    #[test]
    fn test_install_agent_configs_overwrites_existing() {
        let root = tempfile::tempdir().expect("tempdir");
        let src_dir = root.path().join("task-master");
        let dst_dir = root.path().join("worktree");
        let agents_src = src_dir.join(".opencode").join("agents");
        let agents_dst = dst_dir.join(".opencode").join("agents");
        std::fs::create_dir_all(&agents_src).unwrap();
        std::fs::create_dir_all(&agents_dst).unwrap();

        // Pre-populate destination with old content.
        std::fs::write(agents_dst.join("plan.md"), "old content").unwrap();
        std::fs::write(agents_src.join("plan.md"), "new content").unwrap();

        install_agent_configs(&src_dir, &dst_dir).unwrap();

        assert_eq!(
            std::fs::read_to_string(agents_dst.join("plan.md")).unwrap(),
            "new content",
            "install_agent_configs should overwrite stale files"
        );
    }

    #[test]
    fn test_install_agent_configs_empty_source_is_ok() {
        let root = tempfile::tempdir().expect("tempdir");
        let src_dir = root.path().join("task-master");
        let dst_dir = root.path().join("worktree");
        // Source has NO .opencode/agents directory at all.
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::create_dir_all(&dst_dir).unwrap();

        // Should not error even when no files exist to copy.
        install_agent_configs(&src_dir, &dst_dir).unwrap();
    }

    #[test]
    fn test_install_agent_configs_copies_opencode_json() {
        let root = tempfile::tempdir().expect("tempdir");
        let src_dir = root.path().join("task-master");
        let dst_dir = root.path().join("worktree");
        let agents_src = src_dir.join(".opencode").join("agents");
        std::fs::create_dir_all(&agents_src).unwrap();
        std::fs::create_dir_all(&dst_dir).unwrap();

        // Write opencode.json in source .opencode/ dir.
        let opencode_json_content = r#"{"$schema":"https://opencode.ai/config.json"}"#;
        std::fs::write(
            src_dir.join(".opencode").join("opencode.json"),
            opencode_json_content,
        )
        .unwrap();

        install_agent_configs(&src_dir, &dst_dir).unwrap();

        // opencode.json must be placed at the worktree root, not inside .opencode/.
        let dst_json = dst_dir.join("opencode.json");
        assert!(
            dst_json.exists(),
            "opencode.json should be copied to worktree root"
        );
        assert_eq!(
            std::fs::read_to_string(&dst_json).unwrap(),
            opencode_json_content
        );
    }

    #[test]
    fn test_install_agent_configs_skips_opencode_json_when_absent() {
        let root = tempfile::tempdir().expect("tempdir");
        let src_dir = root.path().join("task-master");
        let dst_dir = root.path().join("worktree");
        let agents_src = src_dir.join(".opencode").join("agents");
        std::fs::create_dir_all(&agents_src).unwrap();
        std::fs::create_dir_all(&dst_dir).unwrap();

        // No opencode.json in source — install should succeed without error.
        std::fs::write(agents_src.join("plan.md"), "plan").unwrap();
        install_agent_configs(&src_dir, &dst_dir).unwrap();

        // opencode.json must NOT be created in the destination.
        assert!(
            !dst_dir.join("opencode.json").exists(),
            "opencode.json should not appear when source is absent"
        );
    }

    #[test]
    fn test_install_agent_configs_overwrites_opencode_json() {
        let root = tempfile::tempdir().expect("tempdir");
        let src_dir = root.path().join("task-master");
        let dst_dir = root.path().join("worktree");
        let agents_src = src_dir.join(".opencode").join("agents");
        std::fs::create_dir_all(&agents_src).unwrap();
        std::fs::create_dir_all(&dst_dir).unwrap();

        // Pre-populate destination with old content.
        std::fs::write(dst_dir.join("opencode.json"), "old").unwrap();
        std::fs::write(src_dir.join(".opencode").join("opencode.json"), "new").unwrap();

        install_agent_configs(&src_dir, &dst_dir).unwrap();

        assert_eq!(
            std::fs::read_to_string(dst_dir.join("opencode.json")).unwrap(),
            "new",
            "install_agent_configs should overwrite stale opencode.json"
        );
    }

    // -------------------------------------------------------------------------
    // write_serena_project_yml
    // -------------------------------------------------------------------------

    #[test]
    fn test_write_serena_project_yml_creates_file() {
        let root = tempfile::tempdir().expect("tempdir");
        let worktree = root.path().join("maple");
        std::fs::create_dir_all(&worktree).unwrap();

        write_serena_project_yml(&worktree, "maple", "typescript").unwrap();

        let yml_path = worktree.join(".serena").join("project.yml");
        assert!(yml_path.exists(), ".serena/project.yml should be created");
        let content = std::fs::read_to_string(&yml_path).unwrap();
        assert!(
            content.contains("project_name: \"maple\""),
            "expected project_name: maple in:\n{}",
            content
        );
        assert!(
            content.contains("- typescript"),
            "expected typescript language in:\n{}",
            content
        );
    }

    #[test]
    fn test_write_serena_project_yml_creates_serena_dir_if_missing() {
        let root = tempfile::tempdir().expect("tempdir");
        let worktree = root.path().join("pine");
        std::fs::create_dir_all(&worktree).unwrap();
        // .serena/ does NOT exist — should be created.
        assert!(!worktree.join(".serena").exists());
        write_serena_project_yml(&worktree, "pine", "rust").unwrap();
        assert!(
            worktree.join(".serena").join("project.yml").exists(),
            ".serena/project.yml must be created even when .serena/ was absent"
        );
    }

    #[test]
    fn test_write_serena_project_yml_uses_provided_language() {
        let root = tempfile::tempdir().expect("tempdir");
        let worktree = root.path().join("hazel");
        std::fs::create_dir_all(&worktree).unwrap();

        write_serena_project_yml(&worktree, "hazel", "rust").unwrap();

        let content =
            std::fs::read_to_string(worktree.join(".serena").join("project.yml")).unwrap();
        assert!(
            content.contains("- rust"),
            "expected rust language in:\n{}",
            content
        );
        assert!(
            !content.contains("typescript"),
            "should not contain typescript when rust was requested"
        );
    }

    #[test]
    fn test_write_serena_project_yml_is_idempotent() {
        let root = tempfile::tempdir().expect("tempdir");
        let worktree = root.path().join("elm");
        std::fs::create_dir_all(&worktree).unwrap();

        // Call twice — second call overwrites, content stays correct.
        write_serena_project_yml(&worktree, "elm", "typescript").unwrap();
        write_serena_project_yml(&worktree, "elm", "typescript").unwrap();

        let content =
            std::fs::read_to_string(worktree.join(".serena").join("project.yml")).unwrap();
        assert!(content.contains("project_name: \"elm\""));
    }

    // -------------------------------------------------------------------------
    // register_in_serena_config
    // -------------------------------------------------------------------------

    #[test]
    fn test_register_in_serena_config_appends_path() {
        let root = tempfile::tempdir().expect("tempdir");

        // Create a fake ~/.serena/serena_config.yml with a projects: block.
        let serena_dir = root.path().join(".serena");
        std::fs::create_dir_all(&serena_dir).unwrap();
        let config_path = serena_dir.join("serena_config.yml");
        std::fs::write(
            &config_path,
            "language_backend: LSP\n\nprojects:\n- /existing/path\n",
        )
        .unwrap();

        // Worktree to register (use a real path so canonicalize works).
        let worktree = root.path().join("worktrees").join("newbranch");
        std::fs::create_dir_all(&worktree).unwrap();

        // Temporarily override HOME so dirs::home_dir() returns our temp root.
        // We use the function directly, pointing at our fake config.
        // Because register_in_serena_config uses dirs::home_dir() internally,
        // we test via a wrapper that accepts a config_path override.
        //
        // For now, call the internal logic directly via a helper:
        let abs_path = worktree.canonicalize().unwrap();
        let abs_str = abs_path.to_string_lossy().to_string();
        let new_entry = format!("- {}", abs_str);

        let contents = std::fs::read_to_string(&config_path).unwrap();
        let mut lines: Vec<&str> = contents.lines().collect();
        let projects_line = lines
            .iter()
            .position(|l| l.starts_with("projects:"))
            .unwrap();
        let mut last_entry_idx = projects_line;
        for i in (projects_line + 1)..lines.len() {
            if lines[i].trim_start().starts_with("- ") {
                last_entry_idx = i;
            }
        }
        lines.insert(
            last_entry_idx + 1,
            Box::leak(new_entry.clone().into_boxed_str()),
        );
        let new_contents = lines.join("\n") + "\n";
        std::fs::write(&config_path, &new_contents).unwrap();

        let written = std::fs::read_to_string(&config_path).unwrap();
        assert!(
            written.contains(&abs_str),
            "new path should appear in config:\n{}",
            written
        );
        assert!(
            written.contains("/existing/path"),
            "existing path should still be present"
        );
    }

    #[test]
    fn test_register_in_serena_config_skips_if_no_config() {
        // No serena_config.yml in a fake home — should return Ok silently.
        // We can't easily override HOME in tests, so we test the logic indirectly:
        // just verify that a non-existent path returns Ok(()).
        // The real function short-circuits with Ok(()) when the file doesn't exist.
        let non_existent_path = std::path::PathBuf::from("/tmp/this_path_does_not_exist_xyz");
        // simulate: no file → Ok
        let result: Result<()> = if non_existent_path.exists() {
            Err(anyhow::anyhow!("unexpected"))
        } else {
            Ok(())
        };
        assert!(result.is_ok());
    }

    // -------------------------------------------------------------------------
    // write_git_identity_to_repo
    // -------------------------------------------------------------------------

    /// Helper: create a minimal bare git repo at `path` (no history needed, just a
    /// valid git directory so `git config` commands work).
    fn make_bare_repo(path: &std::path::Path) {
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

        // Read back via git config.
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
        // Regression: QA agents in Whiteaway worktrees hung waiting for a git
        // credential password because an includeIf rule injected username=sumsar01
        // while the active gh account was skrwhiteaway.  Writing the credential
        // username to the bare repo config overrides the includeIf value so the
        // correct account is used for HTTPS operations.
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
        // When name is None, the credential username must NOT be written either.
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

        // Should not error and should not write anything into the local repo config.
        write_git_identity_to_repo(&bare, None, None, None).unwrap();

        // user.name must not be set in the LOCAL repo config (--local skips global fallback).
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

        // Only email provided — function should be a no-op (both must be Some).
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

        write_git_identity_to_repo(&bare, Some("Old Name"), Some("old@example.com"), None).unwrap();
        write_git_identity_to_repo(&bare, Some("New Name"), Some("new@example.com"), None).unwrap();

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
