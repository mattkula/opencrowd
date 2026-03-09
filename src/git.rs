use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

/// Verify we're inside a git repository. Returns the repo root path.
pub fn ensure_git_repo() -> Result<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("Failed to run git. Is git installed?")?;

    if !output.status.success() {
        bail!(
            "Not inside a git repository.\n\
             opencrowd must be run from within a git repository.\n\
             Please cd into a git repo and try again."
        );
    }

    let path = String::from_utf8(output.stdout)?.trim().to_string();
    Ok(path)
}

/// Get the repository directory name (last component of the path).
pub fn repo_name(repo_path: &str) -> String {
    Path::new(repo_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "repo".to_string())
}

/// Get the parent directory of the repo (where worktree dirs will be created).
pub fn repo_parent(repo_path: &str) -> String {
    Path::new(repo_path)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".to_string())
}

/// Create a new git worktree with a new branch.
pub fn create_worktree(repo_path: &str, worktree_path: &str, branch: &str) -> Result<()> {
    // First check if branch already exists
    let check = Command::new("git")
        .current_dir(repo_path)
        .args(["branch", "--list", branch])
        .output()?;

    let branch_exists = !String::from_utf8_lossy(&check.stdout).trim().is_empty();

    let output = if branch_exists {
        Command::new("git")
            .current_dir(repo_path)
            .args(["worktree", "add", worktree_path, branch])
            .output()
            .context("Failed to create git worktree")?
    } else {
        Command::new("git")
            .current_dir(repo_path)
            .args(["worktree", "add", "-b", branch, worktree_path])
            .output()
            .context("Failed to create git worktree")?
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Failed to create worktree: {}", stderr.trim());
    }

    Ok(())
}

/// Remove a git worktree.
pub fn remove_worktree(repo_path: &str, worktree_path: &str) -> Result<()> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(["worktree", "remove", "--force", worktree_path])
        .output()
        .context("Failed to remove git worktree")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Failed to remove worktree: {}", stderr.trim());
    }

    Ok(())
}

/// Delete a git branch.
pub fn delete_branch(repo_path: &str, branch: &str) -> Result<()> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(["branch", "-D", branch])
        .output()
        .context("Failed to delete branch")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Failed to delete branch: {}", stderr.trim());
    }

    Ok(())
}

/// List existing worktrees to reconcile state on startup.
#[allow(dead_code)]
pub fn list_worktrees(repo_path: &str) -> Result<Vec<String>> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .context("Failed to list worktrees")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let paths: Vec<String> = stdout
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(|s| s.to_string())
        .collect();

    Ok(paths)
}
