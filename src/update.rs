/// Information about an available update fetched from GitHub Releases.
#[derive(Debug, Clone, PartialEq)]
pub struct UpdateInfo {
    /// Semver version string without the leading "v", e.g. "0.3.0".
    pub latest_version: String,
    /// HTML URL to the GitHub release page (for displaying to the user).
    pub release_url: String,
    /// Direct download URL for the platform-appropriate binary asset.
    pub download_url: String,
}

/// Check whether a newer version of task-master is available on GitHub Releases.
///
/// Returns `Some(UpdateInfo)` when the latest release is strictly newer than the
/// currently running [`crate::VERSION`].  Returns `None` on any failure (network
/// error, rate-limit, parse error, up to date) — this function must never panic
/// or propagate an error to the caller.
///
/// The check hits `https://api.github.com/repos/sumsar01/task-master/releases/latest`
/// with a 3-second timeout.  It requires the `ureq` crate (added in Cargo.toml).
pub fn check_for_update() -> Option<UpdateInfo> {
    let response = ureq::get("https://api.github.com/repos/sumsar01/task-master/releases/latest")
        .timeout(std::time::Duration::from_secs(3))
        .set("User-Agent", &format!("task-master/{}", crate::VERSION))
        .set("Accept", "application/vnd.github+json")
        .call()
        .ok()?;

    let json: serde_json::Value = response.into_json().ok()?;

    // tag_name is e.g. "v0.3.0"
    let tag_name = json["tag_name"].as_str()?;
    let latest_version = tag_name.trim_start_matches('v').to_string();
    let release_url = json["html_url"].as_str()?.to_string();

    // Only suggest an update when latest is strictly newer than current.
    if !is_newer(&latest_version, crate::VERSION) {
        return None;
    }

    // Find the asset matching the current platform.
    let os = std::env::consts::OS; // "linux", "macos", "windows"
    let arch = std::env::consts::ARCH; // "x86_64", "aarch64", …

    // Map Rust's OS names to the naming convention used in the release workflow.
    let os_str = match os {
        "macos" => "darwin",
        other => other,
    };
    let expected_name = format!("task-master-{}-{}", os_str, arch);

    let download_url = json["assets"]
        .as_array()?
        .iter()
        .find(|a| a["name"].as_str().unwrap_or("") == expected_name)?["browser_download_url"]
        .as_str()?
        .to_string();

    Some(UpdateInfo {
        latest_version,
        release_url,
        download_url,
    })
}

/// Returns true if `candidate` is strictly greater than `current` using simple
/// semver comparison (major.minor.patch integers).  Non-parseable versions
/// return false so we never suggest a bogus update.
fn is_newer(candidate: &str, current: &str) -> bool {
    parse_semver(candidate)
        .zip(parse_semver(current))
        .map(|(c, cur)| c > cur)
        .unwrap_or(false)
}

fn parse_semver(v: &str) -> Option<(u64, u64, u64)> {
    let v = v.trim_start_matches('v');
    let mut parts = v.splitn(3, '.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts
        .next()
        // Strip any pre-release suffix (e.g. "1-beta.1" -> "1")
        .and_then(|s| s.split('-').next())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    Some((major, minor, patch))
}

/// Download `url` to `dest_path`, replacing the file if it already exists.
/// Sets the executable bit on Unix after writing.
/// Returns an error string on failure (not anyhow — callers handle display).
pub fn download_binary(url: &str, dest_path: &std::path::Path) -> Result<(), String> {
    let response = ureq::get(url)
        .timeout(std::time::Duration::from_secs(120))
        .set("User-Agent", &format!("task-master/{}", crate::VERSION))
        .call()
        .map_err(|e| format!("HTTP request failed: {e}"))?;

    let mut reader = response.into_reader();
    let mut data = Vec::new();
    std::io::Read::read_to_end(&mut reader, &mut data)
        .map_err(|e| format!("Failed to read response: {e}"))?;

    std::fs::write(dest_path, &data).map_err(|e| format!("Failed to write binary: {e}"))?;

    // Make executable on Unix.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(dest_path)
            .map_err(|e| format!("Failed to read metadata: {e}"))?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(dest_path, perms)
            .map_err(|e| format!("Failed to set permissions: {e}"))?;
    }

    Ok(())
}

/// Replace the currently running binary with `new_binary_path`.
///
/// Uses a rename-over trick: atomically moves the new binary to the current
/// exe path.  On most Unix filesystems this is atomic and works even while the
/// old binary is running (the inode is kept alive until the process exits).
pub fn replace_current_binary(new_binary_path: &std::path::Path) -> Result<(), String> {
    let current_exe =
        std::env::current_exe().map_err(|e| format!("Cannot determine current exe path: {e}"))?;

    std::fs::rename(new_binary_path, &current_exe)
        .map_err(|e| format!("Failed to replace binary at {}: {e}", current_exe.display()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_newer_true() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(is_newer("0.1.1", "0.1.0"));
    }

    #[test]
    fn test_is_newer_false_equal() {
        assert!(!is_newer("0.1.0", "0.1.0"));
    }

    #[test]
    fn test_is_newer_false_older() {
        assert!(!is_newer("0.1.0", "0.2.0"));
    }

    #[test]
    fn test_is_newer_strips_v_prefix() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("0.2.0", "v0.1.0"));
    }

    #[test]
    fn test_is_newer_bogus_returns_false() {
        assert!(!is_newer("not-a-version", "0.1.0"));
        assert!(!is_newer("0.1.0", "not-a-version"));
    }
}
