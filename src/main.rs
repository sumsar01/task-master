mod hooks;
mod qa;
mod registry;
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
    /// Install QA post-push git hooks into all registered worktrees
    InstallQaHooks,
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
                Commands::InstallQaHooks => hooks::cmd_install_qa_hooks(&registry),
                Commands::AddProject { .. } => unreachable!(),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// spawn
// ---------------------------------------------------------------------------

fn cmd_spawn(registry: &Registry, window_name: &str, prompt: &str) -> Result<()> {
    let worktree = registry.find_worktree(window_name).with_context(|| {
        format!(
            "Worktree '{}' not found. Run `task-master list` to see available worktrees.",
            window_name
        )
    })?;

    let session = tmux::current_session()?;
    let abs_path_str = worktree.abs_path.to_string_lossy().to_string();

    info!(
        "[{}] Spawning in session '{}', dir {}",
        window_name, session, abs_path_str
    );

    let is_new = tmux::spawn_window(&session, window_name, &abs_path_str, prompt)?;

    if is_new {
        println!("Spawned '{}' in a new window.", window_name);
    } else {
        println!("Sent task to existing '{}' window.", window_name);
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
    let mut doc = contents
        .parse::<DocumentMut>()
        .context("Failed to parse task-master.toml")?;

    // Find the right project array entry and push a new worktree
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

    std::fs::write(&config_path, doc.to_string())?;

    println!(
        "Added {}. Spawn with:\n  task-master spawn {} \"<prompt>\"",
        window_name, window_name
    );

    // Auto-install the QA post-push hook for the new worktree.
    match hooks::install_hook_for_single(&worktree_path, &window_name) {
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
