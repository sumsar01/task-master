use std::process::Command;

fn main() {
    // Tell Cargo to rerun this script if the git HEAD or tags change.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/tags");

    // Try to get the exact git tag for the current commit (e.g. "v0.2.0").
    // If this fails (no tag, dirty tree, no git), fall back to CARGO_PKG_VERSION.
    let version = git_exact_tag()
        .map(|tag| tag.trim_start_matches('v').to_string())
        .unwrap_or_else(|| {
            std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string())
        });

    println!("cargo:rustc-env=TASK_MASTER_VERSION={}", version);
}

fn git_exact_tag() -> Option<String> {
    let out = Command::new("git")
        .args(["describe", "--tags", "--exact-match", "HEAD"])
        .output()
        .ok()?;

    if out.status.success() {
        let tag = String::from_utf8(out.stdout).ok()?.trim().to_string();
        if !tag.is_empty() {
            return Some(tag);
        }
    }
    None
}
