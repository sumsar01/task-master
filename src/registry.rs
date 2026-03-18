use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Deserialize, Clone)]
pub struct WorktreeConfig {
    pub name: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ProjectConfig {
    pub name: String,
    pub short: String,
    pub repo: String,
    #[serde(default)]
    pub worktrees: Vec<WorktreeConfig>,
}

#[derive(Debug, Deserialize)]
struct RawConfig {
    projects: Vec<ProjectConfig>,
}

/// A fully-resolved worktree ready for use.
#[derive(Debug, Clone)]
pub struct Worktree {
    /// e.g. "WIS-olive"
    pub window_name: String,
    /// e.g. "projects/warehouse-integration-service/olive" (relative to base_dir)
    pub rel_path: String,
    /// Absolute path
    pub abs_path: PathBuf,
    /// Short name of the parent project, e.g. "WIS"
    pub project_short: String,
    /// Full name of the parent project, e.g. "warehouse-integration-service"
    pub project_name: String,
}

#[derive(Debug)]
pub struct Registry {
    pub projects: Vec<ProjectConfig>,
    pub worktrees: Vec<Worktree>,
    pub base_dir: PathBuf,
}

impl Registry {
    pub fn load(base_dir: PathBuf) -> Result<Self> {
        let config_path = base_dir.join("task-master.toml");
        let contents = fs::read_to_string(&config_path)
            .with_context(|| format!("Failed to read config: {}", config_path.display()))?;
        Self::load_from_str(&contents, base_dir)
    }

    /// Parse a registry from a TOML string with the given base directory.
    /// Used in tests and by `load`.
    pub fn load_from_str(contents: &str, base_dir: PathBuf) -> Result<Self> {
        let raw: RawConfig =
            toml::from_str(contents).context("Failed to parse task-master.toml")?;

        let mut worktrees = Vec::new();
        for project in &raw.projects {
            for wt in &project.worktrees {
                let rel_path = format!("{}/{}", project.repo, wt.name);
                let abs_path = base_dir.join(&rel_path);
                worktrees.push(Worktree {
                    window_name: format!("{}-{}", project.short, wt.name),
                    rel_path,
                    abs_path,
                    project_short: project.short.clone(),
                    project_name: project.name.clone(),
                });
            }
        }

        // Validate no duplicate window names across all projects
        let mut seen = std::collections::HashSet::new();
        for wt in &worktrees {
            if !seen.insert(wt.window_name.clone()) {
                bail!(
                    "Duplicate window name '{}' in task-master.toml",
                    wt.window_name
                );
            }
        }

        Ok(Registry {
            projects: raw.projects,
            worktrees,
            base_dir,
        })
    }

    /// Find a worktree by window name (e.g. "WIS-olive").
    pub fn find_worktree(&self, window_name: &str) -> Option<&Worktree> {
        self.worktrees.iter().find(|w| w.window_name == window_name)
    }

    /// Find a project by short name (case-insensitive).
    pub fn find_project(&self, short: &str) -> Option<&ProjectConfig> {
        self.projects
            .iter()
            .find(|p| p.short.eq_ignore_ascii_case(short))
    }

    /// Check that a window name (short-worktree) is not already taken.
    pub fn assert_window_name_free(&self, window_name: &str) -> Result<()> {
        if self.find_worktree(window_name).is_some() {
            bail!(
                "Worktree '{}' already exists. Use a different name.",
                window_name
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Build a Registry directly from a TOML string without touching the filesystem.
    fn registry_from_toml(toml: &str) -> Result<Registry> {
        Registry::load_from_str(toml, PathBuf::from("/fake/base"))
    }

    const SIMPLE_TOML: &str = r#"
[[projects]]
name = "warehouse-integration-service"
short = "WIS"
repo = "projects/warehouse-integration-service"

[[projects.worktrees]]
name = "olive"

[[projects.worktrees]]
name = "cedar"

[[projects]]
name = "other-service"
short = "OTH"
repo = "projects/other-service"

[[projects.worktrees]]
name = "main"
"#;

    #[test]
    fn test_find_worktree_existing() {
        let reg = registry_from_toml(SIMPLE_TOML).unwrap();
        let wt = reg.find_worktree("WIS-olive");
        assert!(wt.is_some());
        let wt = wt.unwrap();
        assert_eq!(wt.window_name, "WIS-olive");
        assert_eq!(wt.project_short, "WIS");
        assert_eq!(wt.project_name, "warehouse-integration-service");
    }

    #[test]
    fn test_find_worktree_missing() {
        let reg = registry_from_toml(SIMPLE_TOML).unwrap();
        assert!(reg.find_worktree("WIS-doesnotexist").is_none());
    }

    #[test]
    fn test_find_worktree_is_case_sensitive() {
        // Window names are stored as constructed — lookup must be exact.
        let reg = registry_from_toml(SIMPLE_TOML).unwrap();
        assert!(reg.find_worktree("wis-olive").is_none());
        assert!(reg.find_worktree("WIS-Olive").is_none());
    }

    #[test]
    fn test_find_project_case_insensitive() {
        let reg = registry_from_toml(SIMPLE_TOML).unwrap();
        assert!(reg.find_project("WIS").is_some());
        assert!(reg.find_project("wis").is_some());
        assert!(reg.find_project("Wis").is_some());
    }

    #[test]
    fn test_find_project_missing() {
        let reg = registry_from_toml(SIMPLE_TOML).unwrap();
        assert!(reg.find_project("XYZ").is_none());
    }

    #[test]
    fn test_assert_window_name_free_when_taken() {
        let reg = registry_from_toml(SIMPLE_TOML).unwrap();
        let err = reg.assert_window_name_free("WIS-olive").unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn test_assert_window_name_free_when_available() {
        let reg = registry_from_toml(SIMPLE_TOML).unwrap();
        assert!(reg.assert_window_name_free("WIS-newbranch").is_ok());
    }

    #[test]
    fn test_worktree_abs_path_constructed_correctly() {
        let reg = registry_from_toml(SIMPLE_TOML).unwrap();
        let wt = reg.find_worktree("WIS-cedar").unwrap();
        assert_eq!(
            wt.abs_path,
            PathBuf::from("/fake/base/projects/warehouse-integration-service/cedar")
        );
        assert_eq!(wt.rel_path, "projects/warehouse-integration-service/cedar");
    }

    #[test]
    fn test_duplicate_window_name_is_rejected() {
        // Two projects with the same short name produce duplicate window names.
        let toml = r#"
[[projects]]
name = "alpha"
short = "A"
repo = "projects/alpha"
[[projects.worktrees]]
name = "foo"

[[projects]]
name = "beta"
short = "A"
repo = "projects/beta"
[[projects.worktrees]]
name = "foo"
"#;
        let err = registry_from_toml(toml).unwrap_err();
        assert!(err.to_string().contains("Duplicate window name"));
    }

    #[test]
    fn test_project_with_no_worktrees() {
        let toml = r#"
[[projects]]
name = "empty-service"
short = "EMP"
repo = "projects/empty-service"
"#;
        let reg = registry_from_toml(toml).unwrap();
        assert_eq!(reg.projects.len(), 1);
        assert!(reg.worktrees.is_empty());
    }

    #[test]
    fn test_all_worktrees_enumerated() {
        let reg = registry_from_toml(SIMPLE_TOML).unwrap();
        let names: Vec<&str> = reg
            .worktrees
            .iter()
            .map(|w| w.window_name.as_str())
            .collect();
        assert!(names.contains(&"WIS-olive"));
        assert!(names.contains(&"WIS-cedar"));
        assert!(names.contains(&"OTH-main"));
        assert_eq!(names.len(), 3);
    }
}
