mod e2e;
mod hooks;
mod plan;
mod qa;
mod registry;
mod status;
mod supervise;
mod tmux;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use registry::Registry;
use std::path::PathBuf;
use std::process::Command;
use toml_edit::{value, DocumentMut, Item, Table};
use tracing::info;

#[derive(Parser)]
#[command(name = "task-master", about = "AI agent orchestrator")]
struct Cli {
    /// Override the base directory (defaults to current directory or $TASK_MASTER_DIR)
    #[arg(long, env = "TASK_MASTER_DIR")]
    dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Spawn an opencode agent in a new tmux window for the given worktree
    Spawn {
        /// Worktree window name, e.g. WIS-olive
        worktree: String,
        /// Prompt / task description to pass to the agent
        prompt: String,
    },
    /// Spawn a planning agent to decompose a task into beads issues
    Plan {
        /// Worktree window name, e.g. WIS-olive
        worktree: String,
        /// Task description to plan
        prompt: String,
    },
    /// List all configured projects and worktrees
    List,
    /// Add a new worktree to an existing project
    AddWorktree {
        /// Project short name, e.g. WIS
        project: String,
        /// Worktree name, e.g. cedar
        name: String,
        /// Branch to check out (defaults to HEAD)
        #[arg(long)]
        branch: Option<String>,
    },
    /// Add a new project by cloning a bare repo
    AddProject {
        /// Full project name, e.g. warehouse-integration-service
        name: String,
        /// Short name used as window prefix, e.g. WIS
        short: String,
        /// Git repo URL to clone
        url: String,
    },
    /// Spawn a QA agent for a worktree's open PR
    Qa {
        /// Worktree window name, e.g. WIS-olive
        worktree: String,
        /// GitHub PR number to review
        pr_number: u64,
    },
    /// Spawn an e2e validation agent for a worktree's deployed PR
    E2e {
        /// Worktree window name, e.g. WIS-olive
        worktree: String,
        /// GitHub PR number to validate against staging
        pr_number: u64,
    },
    /// Install QA post-push git hooks into all registered worktrees
    InstallQaHooks,
    /// Reset a worktree window's phase indicator back to idle
    Reset {
        /// Worktree window name, e.g. WIS-olive (with or without phase suffix)
        worktree: String,
    },
    /// Start the supervisor agent that monitors all worktree windows
    Supervise,
    /// Show status of all registered worktrees with their live tmux phase
    Status,
    /// Remove a worktree from the registry and from git
    RemoveWorktree {
        /// Worktree window name, e.g. WIS-olive
        worktree: String,
        /// Force removal even if a tmux window is active
        #[arg(long)]
        force: bool,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    let base_dir = cli
        .dir
        .unwrap_or_else(|| PathBuf::from("."))
        .canonicalize()
        .context("Failed to resolve base directory")?;

    match cli.command {
        // add-project doesn't need an existing registry
        Commands::AddProject { name, short, url } => {
            cmd_add_project(&base_dir, &name, &short, &url)
        }
        _ => {
            let registry = Registry::load(base_dir.clone()).context("Failed to load registry")?;
            match cli.command {
                Commands::Spawn { worktree, prompt } => cmd_spawn(&registry, &worktree, &prompt),
                Commands::Plan { worktree, prompt } => {
                    plan::cmd_plan(&registry, &worktree, &prompt)
                }
                Commands::List => cmd_list(&registry),
                Commands::AddWorktree {
                    project,
                    name,
                    branch,
                } => cmd_add_worktree(&registry, &base_dir, &project, &name, branch.as_deref()),
                Commands::Qa {
                    worktree,
                    pr_number,
                } => qa::cmd_qa(&registry, &worktree, pr_number),
                Commands::E2e {
                    worktree,
                    pr_number,
                } => e2e::cmd_e2e(&registry, &worktree, pr_number),
                Commands::InstallQaHooks => hooks::cmd_install_qa_hooks(&registry),
                Commands::Reset { worktree } => cmd_reset(&worktree),
                Commands::Supervise => supervise::cmd_supervise(&registry),
                Commands::Status => status::cmd_status(&registry),
                Commands::RemoveWorktree { worktree, force } => {
                    cmd_remove_worktree(&registry, &base_dir, &worktree, force)
                }
                Commands::AddProject { .. } => unreachable!(),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// spawn
// ---------------------------------------------------------------------------

fn cmd_spawn(registry: &Registry, window_name: &str, prompt: &str) -> Result<()> {
    let worktree = registry.require_worktree(window_name)?;

    let session = tmux::current_session()?;
    let abs_path_str = worktree.abs_path.to_string_lossy().to_string();

    info!(
        "[{}] Spawning in session '{}', dir {}",
        window_name, session, abs_path_str
    );

    let base_name = tmux::base_window_name(window_name);

    let is_new = if tmux::find_window_index(&session, base_name).is_none() {
        // No window yet — create it fresh.
        tmux::spawn_window(&session, window_name, &abs_path_str, prompt, None)?;
        true
    } else {
        // Window already exists (possibly in :plan, :qa, :review, or :dev phase).
        // Always replace the running process with a fresh opencode dev session so
        // we don't accidentally send prompts into a plan/qa agent's chat input.
        tmux::set_window_phase(&session, base_name, Some("dev"))?;
        tmux::replace_window_process(&session, base_name, &abs_path_str, prompt, None)?;
        false
    };

    // Ensure :dev phase on new windows too (spawn_window sets it but be explicit).
    tmux::set_window_phase(&session, base_name, Some("dev"))?;

    if is_new {
        println!("Spawned '{}:dev' in a new window.", base_name);
    } else {
        println!(
            "Replaced existing '{}' window with fresh dev session (now '{}:dev').",
            base_name, base_name
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

fn cmd_list(registry: &Registry) -> Result<()> {
    for project in &registry.projects {
        println!("{} ({})", project.name, project.short);
        for wt in &project.worktrees {
            let window_name = format!("{}-{}", project.short, wt.name);
            let abs = registry.base_dir.join(&project.repo).join(&wt.name);
            println!("  {:<20} {}", window_name, abs.display());
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// add-worktree
// ---------------------------------------------------------------------------

fn cmd_add_worktree(
    registry: &Registry,
    base_dir: &PathBuf,
    project_short: &str,
    worktree_name: &str,
    branch: Option<&str>,
) -> Result<()> {
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
    let status = Command::new("git")
        .arg("-C")
        .arg(&repo_path)
        .args(&git_args)
        .status()
        .context("Failed to run git worktree add")?;

    if !status.success() {
        bail!("git worktree add failed");
    }

    // Append to task-master.toml
    let config_path = base_dir.join("task-master.toml");
    let contents = std::fs::read_to_string(&config_path)?;

    let new_toml = append_worktree_to_toml(&contents, project_short, worktree_name)
        .context("Failed to update task-master.toml")?;
    std::fs::write(&config_path, new_toml)?;

    println!(
        "Added {}. Spawn with:\n  task-master spawn {} \"<prompt>\"",
        window_name, window_name
    );

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

    Ok(())
}

// ---------------------------------------------------------------------------
// add-project
// ---------------------------------------------------------------------------

fn cmd_add_project(base_dir: &PathBuf, name: &str, short: &str, url: &str) -> Result<()> {
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

    info!("Cloning bare repo {} -> {}", url, repo_path.display());
    let status = Command::new("git")
        .args(["clone", "--bare", url])
        .arg(&repo_path)
        .status()
        .context("Failed to run git clone")?;

    if !status.success() {
        bail!("git clone failed");
    }

    // Append [[projects]] block to task-master.toml
    let repo_rel = format!("projects/{}", name);

    let new_block = format!(
        "\n[[projects]]\nname = \"{}\"\nshort = \"{}\"\nrepo = \"{}\"\n",
        name, short, repo_rel
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

// ---------------------------------------------------------------------------
// reset
// ---------------------------------------------------------------------------

fn cmd_reset(worktree: &str) -> Result<()> {
    let session = tmux::current_session()?;
    let base = tmux::base_window_name(worktree);
    tmux::set_window_phase(&session, base, None)?;
    println!("Reset '{}' to idle.", base);
    Ok(())
}

// ---------------------------------------------------------------------------
// remove-worktree
// ---------------------------------------------------------------------------

fn cmd_remove_worktree(
    registry: &Registry,
    base_dir: &PathBuf,
    window_name: &str,
    force: bool,
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

    // Run `git worktree remove [--force] <path>` from the bare repo root.
    // The bare repo is `base_dir/<project.repo>`.
    let project = registry
        .find_project(&worktree.project_short)
        .with_context(|| format!("Project '{}' not found", worktree.project_short))?;
    let repo_path = base_dir.join(&project.repo);

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
    let git_status = Command::new("git")
        .arg("-C")
        .arg(&repo_path)
        .args(&git_args)
        .status()
        .context("Failed to run git worktree remove")?;

    if !git_status.success() {
        bail!("git worktree remove failed");
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
fn append_worktree_to_toml(
    toml_str: &str,
    project_short: &str,
    worktree_name: &str,
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
    worktrees.push(new_wt);

    Ok(doc.to_string())
}

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
        let result = append_worktree_to_toml(BASE_TOML, "WIS", "cedar").unwrap();
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
        let result = append_worktree_to_toml(BASE_TOML, "WIS", "cedar").unwrap();
        // Round-trip: must parse without error and contain both worktrees.
        let reg = registry::Registry::load_from_str(&result, PathBuf::from("/base")).unwrap();
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
        let result = append_worktree_to_toml(BASE_TOML, "wis", "birch").unwrap();
        assert!(result.contains("birch"));
    }

    #[test]
    fn test_append_worktree_unknown_project_returns_error() {
        let err = append_worktree_to_toml(BASE_TOML, "XYZ", "branch").unwrap_err();
        assert!(err.to_string().contains("XYZ"));
    }

    #[test]
    fn test_append_worktree_to_project_with_no_prior_worktrees() {
        let toml = r#"[[projects]]
name = "fresh-service"
short = "FS"
repo = "projects/fresh-service"
"#;
        let result = append_worktree_to_toml(toml, "FS", "main").unwrap();
        assert!(result.contains("main"));
        // Validate it is parseable.
        let reg = registry::Registry::load_from_str(&result, PathBuf::from("/base")).unwrap();
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
        let result = append_worktree_to_toml(toml, "B", "new-branch").unwrap();
        let reg = registry::Registry::load_from_str(&result, PathBuf::from("/base")).unwrap();
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
        let result = append_worktree_to_toml(&toml, "WIS", "branch").unwrap();
        assert!(result.starts_with("# top comment\n"), "comment was lost");
    }

    // -------------------------------------------------------------------------
    // Registry::load from a real file (tempdir)
    // -------------------------------------------------------------------------

    #[test]
    fn test_registry_load_from_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("task-master.toml");
        std::fs::write(&config_path, BASE_TOML).unwrap();

        let reg = Registry::load(dir.path().to_path_buf()).unwrap();
        assert_eq!(reg.projects.len(), 1);
        assert_eq!(reg.worktrees.len(), 1);
        assert_eq!(reg.worktrees[0].window_name, "WIS-olive");
        assert_eq!(
            reg.worktrees[0].abs_path,
            dir.path()
                .join("projects/warehouse-integration-service/olive")
        );
    }

    #[test]
    fn test_registry_load_missing_file_returns_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        // No task-master.toml written.
        let err = Registry::load(dir.path().to_path_buf()).unwrap_err();
        assert!(err.to_string().contains("Failed to read config"));
    }

    #[test]
    fn test_registry_load_invalid_toml_returns_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("task-master.toml"), "not valid toml }{").unwrap();
        let err = Registry::load(dir.path().to_path_buf()).unwrap_err();
        assert!(err.to_string().contains("parse") || err.to_string().contains("TOML"));
    }

    #[test]
    fn test_registry_load_duplicate_window_names_returns_error() {
        let toml = r#"
[[projects]]
name = "svc"
short = "S"
repo = "projects/svc"
[[projects.worktrees]]
name = "dup"

[[projects]]
name = "other"
short = "S"
repo = "projects/other"
[[projects.worktrees]]
name = "dup"
"#;
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("task-master.toml"), toml).unwrap();
        let err = Registry::load(dir.path().to_path_buf()).unwrap_err();
        assert!(err.to_string().contains("Duplicate"));
    }
}
