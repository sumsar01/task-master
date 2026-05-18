mod add_project;
mod agent_configs;
mod beads;
mod cleanup;
mod e2e;
mod gh;
mod git_identity;
mod hooks;
mod notify;
mod orchestrate;
mod plan;
mod qa;
mod registry;
mod serena;
mod slug;
mod spawn;
mod stats;
mod status;
mod supervise;
mod tmux;
mod tui;
mod ui;
mod worktree;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use registry::Registry;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

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
        /// Worktree window name (e.g. WIS-olive) or project short name when --ephemeral is set (e.g. WIS)
        worktree: String,
        /// Prompt / task description to pass to the agent
        prompt: String,
        /// Reset the worktree even if it has uncommitted changes (discards all local modifications)
        #[arg(long, default_value_t = false)]
        force: bool,
        /// Automatically create a temporary worktree, spawn the agent in it, and register it for
        /// cleanup once the branch is merged. Pass a project short name (e.g. WIS) instead of a
        /// full window name. A unique name like WIS-spruce-7f3a is generated automatically.
        #[arg(long, default_value_t = false)]
        ephemeral: bool,
    },
    /// Spawn an orchestrator agent to delegate a cross-repo task across multiple projects.
    ///
    /// The orchestrator lives in its own dedicated tmux window ('orchestrate') and is
    /// responsible for decomposing the task, identifying relevant projects, spawning
    /// sub-agents (using idle worktrees or ephemeral ones), and monitoring progress.
    /// It never writes code itself — it only delegates and coordinates.
    ///
    /// Window lifecycle phases:
    ///   orchestrate:active  — orchestrator is running
    ///   orchestrate:done    — all sub-tasks complete
    ///   orchestrate:blocked — stalled, needs human input
    Orchestrate {
        /// High-level task description to delegate across projects
        task: String,
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
        /// Skip deleting the remote branch (default: delete it for feature branches)
        #[arg(long)]
        keep_branch: bool,
    },
    /// Remove ephemeral worktrees whose branch has been merged or PR closed.
    ///
    /// Scans all worktrees marked `ephemeral = true` in task-master.toml, checks
    /// whether their branch is merged (via gh CLI), and removes the ones that are done.
    /// Also deletes the remote branch for each removed worktree.
    ///
    /// Use --all to remove all ephemeral worktrees regardless of merge status (requires
    /// --force to skip the confirmation prompt).
    Cleanup {
        /// Only remove worktrees whose branch is merged or PR is closed (default behaviour)
        #[arg(long, default_value_t = false)]
        merged: bool,
        /// Remove all ephemeral worktrees regardless of merge status
        #[arg(long, default_value_t = false)]
        all: bool,
        /// Skip confirmation prompts (required when using --all non-interactively)
        #[arg(long, default_value_t = false)]
        force: bool,
    },
    /// Open the interactive TUI dashboard
    Tui,
    /// Apply per-project git identity overrides to all configured bare repos
    ///
    /// Reads the `git_name` and `git_email` fields from each [[projects]] entry in
    /// task-master.toml and writes them into the corresponding bare repo's git config.
    /// This ensures agents commit with the correct identity even when the worktree path
    /// triggers an unintended includeIf rule in ~/.gitconfig.
    ///
    /// Safe to run multiple times — it is fully idempotent.
    FixGitIdentity,

    /// Create per-account gh config directories so agents can use the right
    /// GitHub account without mutating the global ~/.config/gh/hosts.yml.
    ///
    /// For each distinct gh_account value in task-master.toml, this command
    /// creates ~/.config/gh-<account>/ and writes a hosts.yml that sets that
    /// account as active.  The keyring-backed tokens are shared (not copied),
    /// so no credentials are duplicated on disk.
    ///
    /// Safe to run multiple times — it is fully idempotent.
    SetupGhAccounts,
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
            add_project::cmd_add_project(&base_dir, &name, &short, &url, None, None, None)
        }
        _ => {
            let registry = Registry::load(base_dir.clone()).context("Failed to load registry")?;
            match cli.command {
                Commands::Spawn {
                    worktree,
                    prompt,
                    force,
                    ephemeral,
                } => {
                    if ephemeral {
                        spawn::cmd_spawn_ephemeral(&registry, &base_dir, &worktree, &prompt)
                            .map(|msg| println!("{}", msg))
                    } else {
                        spawn::cmd_spawn(&registry, &worktree, &prompt, force)
                            .map(|msg| println!("{}", msg))
                    }
                }
                Commands::Orchestrate { task } => {
                    orchestrate::cmd_orchestrate(&registry, &task)
                        .map(|msg| println!("{}", msg))
                }
                Commands::Plan { worktree, prompt } => {
                    plan::cmd_plan(&registry, &worktree, &prompt).map(|msg| println!("{}", msg))
                }
                Commands::List => registry::cmd_list(&registry),
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
                Commands::Send { worktree, prompt } => spawn::cmd_send(&registry, &worktree, &prompt),
                Commands::E2e {
                    worktree,
                    pr_number,
                } => e2e::cmd_e2e(&registry, &worktree, pr_number),
                Commands::InstallQaHooks => hooks::cmd_install_qa_hooks(&registry),
                Commands::InstallAgentConfigs => {
                    worktree::cmd_install_agent_configs(&registry, &base_dir)
                        .map(|msg| println!("{}", msg))
                }
                Commands::Reset { worktree } => tmux::cmd_reset(&worktree),
                Commands::Supervise => supervise::cmd_supervise(&registry),
                Commands::Status => status::cmd_status(&registry),
                Commands::Stats { days } => stats::cmd_stats(&registry, days),
                Commands::RemoveWorktree {
                    worktree,
                    force,
                    keep_branch,
                } => worktree::cmd_remove_worktree(
                    &registry,
                    &base_dir,
                    &worktree,
                    force,
                    keep_branch,
                ),
                Commands::Cleanup { merged, all, force } => {
                    cleanup::cmd_cleanup(&registry, &base_dir, merged, all, force)
                }
                Commands::Tui => tui::cmd_tui(&registry),
                Commands::FixGitIdentity => worktree::cmd_fix_git_identity(&registry, &base_dir)
                    .map(|msg| println!("{}", msg)),
                Commands::SetupGhAccounts => gh::cmd_setup_gh_accounts(&registry)
                    .map(|msg| println!("{}", msg)),
                Commands::AddProject { .. } => unreachable!(),
            }
        }
    }
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
