mod e2e;
mod hooks;
mod notify;
mod plan;
mod qa;
mod registry;
mod spawn;
mod stats;
mod status;
mod supervise;
mod templates;
mod tmux;
mod tui;
mod ui;
mod worktree;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use registry::Registry;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
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
        /// Reset the worktree even if it has uncommitted changes (discards all local modifications)
        #[arg(long, default_value_t = false)]
        force: bool,
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
        /// GitHub PR number to review (auto-detected from current branch if omitted)
        pr_number: Option<u64>,
    },
    /// Notify the supervisor that a PR is ready for QA (safe to call from inside an agent)
    Notify {
        /// Worktree window name, e.g. WIS-olive
        worktree: String,
        /// GitHub PR number that is ready for QA
        pr_number: u64,
    },
    /// Send a prompt directly to the running opencode session in a worktree window
    Send {
        /// Worktree window name, e.g. WIS-olive
        worktree: String,
        /// Prompt text to send to the running opencode TUI
        prompt: String,
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
    /// Install opencode agent configs (plan.md, qa.md, e2e.md) into all registered worktrees
    ///
    /// Copies the agent config files from the task-master project into each worktree's
    /// .opencode/agents/ directory so that `opencode --agent plan/qa/e2e` works when
    /// running inside those worktrees. Run this once for existing worktrees; new
    /// worktrees receive the configs automatically via `add-worktree`.
    InstallAgentConfigs,
    /// Reset a worktree window's phase indicator back to idle
    Reset {
        /// Worktree window name, e.g. WIS-olive (with or without phase suffix)
        worktree: String,
    },
    /// Start the supervisor agent that monitors all worktree windows
    Supervise,
    /// Show status of all registered worktrees with their live tmux phase
    Status,
    /// Show token usage and cost statistics for all registered worktrees
    Stats {
        /// Show stats for the last N days (default: all time)
        #[arg(long)]
        days: Option<u32>,
    },
    /// Remove a worktree from the registry and from git
    RemoveWorktree {
        /// Worktree window name, e.g. WIS-olive
        worktree: String,
        /// Force removal even if a tmux window is active
        #[arg(long)]
        force: bool,
    },
    /// Open the interactive TUI dashboard
    Tui,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // When running the TUI, redirect tracing output to /tmp/task-master.log so
    // log lines don't corrupt the ratatui alternate-screen buffer.
    let is_tui = matches!(cli.command, Commands::Tui);
    if is_tui {
        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("/tmp/task-master.log")
            .context("Failed to open /tmp/task-master.log")?;
        let log_file = Arc::new(Mutex::new(log_file));
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
            )
            .with_writer(move || {
                // Return a clone of the Arc-wrapped file each time a writer is needed.
                log_file
                    .lock()
                    .expect("log file lock poisoned")
                    .try_clone()
                    .expect("failed to clone log file handle")
            })
            .with_ansi(false)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
            )
            .init();
    }

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
                Commands::Spawn {
                    worktree,
                    prompt,
                    force,
                } => spawn::cmd_spawn(&registry, &worktree, &prompt, force)
                    .map(|msg| println!("{}", msg)),
                Commands::Plan { worktree, prompt } => {
                    plan::cmd_plan(&registry, &worktree, &prompt).map(|msg| println!("{}", msg))
                }
                Commands::List => cmd_list(&registry),
                Commands::AddWorktree {
                    project,
                    name,
                    branch,
                } => worktree::cmd_add_worktree(
                    &registry,
                    &base_dir,
                    &project,
                    &name,
                    branch.as_deref(),
                )
                .map(|msg| println!("{}", msg)),
                Commands::Qa {
                    worktree,
                    pr_number,
                } => qa::cmd_qa(&registry, &worktree, pr_number).map(|msg| println!("{}", msg)),
                Commands::Notify {
                    worktree,
                    pr_number,
                } => notify::cmd_notify(&registry, &worktree, pr_number),
                Commands::Send { worktree, prompt } => cmd_send(&registry, &worktree, &prompt),
                Commands::E2e {
                    worktree,
                    pr_number,
                } => e2e::cmd_e2e(&registry, &worktree, pr_number),
                Commands::InstallQaHooks => hooks::cmd_install_qa_hooks(&registry),
                Commands::InstallAgentConfigs => {
                    worktree::cmd_install_agent_configs(&registry, &base_dir)
                        .map(|msg| println!("{}", msg))
                }
                Commands::Reset { worktree } => cmd_reset(&worktree),
                Commands::Supervise => supervise::cmd_supervise(&registry),
                Commands::Status => status::cmd_status(&registry),
                Commands::Stats { days } => stats::cmd_stats(&registry, days),
                Commands::RemoveWorktree { worktree, force } => {
                    worktree::cmd_remove_worktree(&registry, &base_dir, &worktree, force)
                }
                Commands::Tui => tui::cmd_tui(&registry),
                Commands::AddProject { .. } => unreachable!(),
            }
        }
    }
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

pub fn cmd_reset(worktree: &str) -> Result<()> {
    let session = tmux::current_session()?;
    let base = tmux::base_window_name(worktree);
    tmux::set_window_phase(&session, base, None)?;
    println!("Reset '{}' to idle.", base);
    Ok(())
}

/// Close (kill) the tmux window for a worktree.
///
/// If the window is running an agent it will be killed immediately.
pub fn cmd_close(session: &str, worktree: &str) -> Result<()> {
    let base = tmux::base_window_name(worktree);
    if let Some(idx) = tmux::find_window_index(session, base) {
        let target = format!("{}:{}", session, idx);
        Command::new("tmux")
            .args(["kill-window", "-t", &target])
            .status()
            .with_context(|| format!("Failed to kill tmux window '{}'", target))?;
        println!("Closed window '{}'.", base);
    } else {
        bail!("Window '{}' not found in session '{}'", base, session);
    }
    Ok(())
}

/// Send a prompt directly to the running opencode session in a worktree window.
///
/// Unlike `cmd_spawn`, this does **not** reset the branch, does not kill the
/// existing session, and does not append any PR-workflow boilerplate. It is the
/// equivalent of the user typing the prompt into the opencode TUI by hand.
pub fn cmd_send(registry: &Registry, worktree_name: &str, prompt: &str) -> Result<()> {
    let wt = registry.require_worktree(worktree_name)?;
    let session = tmux::current_session()?;
    tmux::send_to_window(&session, &wt.window_name, prompt)?;
    println!("Sent to '{}': {}", wt.window_name, prompt);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE_TOML: &str = r#"[[projects]]
name = "warehouse-integration-service"
short = "WIS"
repo = "projects/warehouse-integration-service"

[[projects.worktrees]]
name = "olive"
"#;

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
