use anyhow::{bail, Context, Result};
use std::process::Command;

// ===========================================================================
// Basic tmux utilities
// ===========================================================================

pub fn ensure_tmux() -> Result<()> {
    let output = Command::new("tmux")
        .arg("-V")
        .output()
        .context("tmux is not installed. Please install tmux and try again.")?;

    if !output.status.success() {
        bail!("tmux is not working properly.");
    }
    Ok(())
}

pub fn inside_tmux() -> bool {
    std::env::var("TMUX").is_ok()
}

pub fn current_session_name() -> Option<String> {
    Command::new("tmux")
        .args(["display-message", "-p", "#S"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        })
}

pub fn session_exists(session_name: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", session_name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn current_pane_id() -> Result<String> {
    let output = Command::new("tmux")
        .args(["display-message", "-p", "#{pane_id}"])
        .output()
        .context("Failed to get current pane ID")?;

    if !output.status.success() {
        bail!("Failed to get current pane ID");
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Check if the given pane is currently the active (focused) pane.
pub fn is_pane_active(pane_id: &str) -> bool {
    Command::new("tmux")
        .args(["display-message", "-t", pane_id, "-p", "#{pane_active}"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "1")
        .unwrap_or(false)
}

pub fn detach_client() -> Result<()> {
    let _ = Command::new("tmux")
        .args(["detach-client"])
        .output();
    Ok(())
}

// ===========================================================================
// Outer session bootstrap (auto-launch into tmux)
// ===========================================================================

pub fn create_session_and_attach(session_name: &str, working_dir: &str, command: &str) -> Result<()> {
    let output = Command::new("tmux")
        .args([
            "new-session", "-d",
            "-s", session_name,
            "-c", working_dir,
            command,
        ])
        .output()
        .context("Failed to create tmux session")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Failed to create tmux session: {}", stderr.trim());
    }

    let status = Command::new("tmux")
        .args(["attach-session", "-t", session_name])
        .status()
        .context("Failed to attach to tmux session")?;

    if !status.success() {
        bail!("tmux session ended");
    }

    Ok(())
}

pub fn attach_session(session_name: &str) -> Result<()> {
    let status = Command::new("tmux")
        .args(["attach-session", "-t", session_name])
        .status()
        .context("Failed to attach to tmux session")?;

    if !status.success() {
        bail!("tmux session ended");
    }

    Ok(())
}

pub fn switch_client(session_name: &str) -> Result<()> {
    let output = Command::new("tmux")
        .args(["switch-client", "-t", session_name])
        .output()
        .context("Failed to switch tmux client")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Failed to switch to session: {}", stderr.trim());
    }

    Ok(())
}

// ===========================================================================
// Outer layout: TUI (left), opencode (top-right), CLI (bottom-right)
// ===========================================================================

#[derive(Debug, Clone)]
pub struct PaneLayout {
    pub tui_pane: String,
    pub opencode_pane: Option<String>,
    pub cli_pane: Option<String>,
}

/// Record the TUI pane. Right-side panes are created lazily via `ensure_right_panes`.
pub fn create_layout() -> Result<PaneLayout> {
    let tui_pane = current_pane_id()?;
    Ok(PaneLayout { tui_pane, opencode_pane: None, cli_pane: None })
}

/// Create the right-side panes if they don't exist yet.
/// Returns the (opencode_pane, cli_pane) IDs.
pub fn ensure_right_panes(layout: &mut PaneLayout) -> Result<()> {
    if layout.opencode_pane.is_some() && layout.cli_pane.is_some() {
        return Ok(());
    }

    // Right pane for opencode (70% width)
    let output = Command::new("tmux")
        .args([
            "split-window", "-h", "-d",
            "-p", "70",
            "-P", "-F", "#{pane_id}",
            "-t", &layout.tui_pane,
        ])
        .output()
        .context("Failed to create opencode pane")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Failed to create opencode pane: {}", stderr.trim());
    }

    let opencode_pane = String::from_utf8_lossy(&output.stdout).trim().to_string();

    // Bottom-right pane for CLI (30% of the right side)
    let output = Command::new("tmux")
        .args([
            "split-window", "-v", "-d",
            "-p", "30",
            "-P", "-F", "#{pane_id}",
            "-t", &opencode_pane,
        ])
        .output()
        .context("Failed to create CLI pane")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Failed to create CLI pane: {}", stderr.trim());
    }

    let cli_pane = String::from_utf8_lossy(&output.stdout).trim().to_string();

    // Focus on TUI
    let _ = Command::new("tmux")
        .args(["select-pane", "-t", &layout.tui_pane])
        .output();

    layout.opencode_pane = Some(opencode_pane);
    layout.cli_pane = Some(cli_pane);

    Ok(())
}

// ===========================================================================
// Inner sessions: TWO per feature (opencode + CLI)
// ===========================================================================
//
// Each feature gets two independent tmux sessions:
//   oc-<name>-opencode : runs opencode in the worktree
//   oc-<name>-cli      : shell in the worktree
//
// The outer right panes each run `tmux attach -t <session>` to display them.
// Navigate between all three outer panes with Ctrl-b + arrows.
// Switching features respawns both right panes with new attach commands.
// The old inner sessions detach and keep running in the background.

fn opencode_session_name(repo_name: &str, feature_name: &str) -> String {
    format!("oc-{}-{}-opencode", repo_name, feature_name)
}

fn cli_session_name(repo_name: &str, feature_name: &str) -> String {
    format!("oc-{}-{}-cli", repo_name, feature_name)
}

/// Create a single-pane tmux session running a command in the given directory.
fn create_single_session(name: &str, working_dir: &str, cmd: Option<&str>) -> Result<()> {
    if session_exists(name) {
        return Ok(());
    }

    let mut args = vec![
        "new-session", "-d",
        "-s", name,
        "-c", working_dir,
    ];

    if let Some(c) = cmd {
        args.push(c);
    }

    let output = Command::new("tmux")
        .env_remove("TMUX")
        .args(&args)
        .output()
        .context(format!("Failed to create session '{}'", name))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Failed to create session '{}': {}", name, stderr.trim());
    }

    Ok(())
}

/// Create both inner sessions for a feature.
pub fn create_inner_sessions(repo_name: &str, feature_name: &str, worktree_path: &str) -> Result<()> {
    let oc_name = opencode_session_name(repo_name, feature_name);
    let cli_name = cli_session_name(repo_name, feature_name);

    create_single_session(&oc_name, worktree_path, Some("opencode"))?;
    create_single_session(&cli_name, worktree_path, None)?;

    Ok(())
}

/// Respawn an outer pane to attach to an inner session.
fn attach_pane_to_session(pane_id: &str, session_name: &str) -> Result<()> {
    let cmd = format!("unset TMUX && exec tmux attach-session -t {}", session_name);
    let output = Command::new("tmux")
        .args([
            "respawn-pane", "-k",
            "-t", pane_id,
            "sh", "-c", &cmd,
        ])
        .output()
        .context("Failed to attach pane to session")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Failed to attach pane: {}", stderr.trim());
    }

    Ok(())
}

/// Show a feature in the outer right panes.
/// The layout must have right panes created (call `ensure_right_panes` first).
pub fn show_feature(layout: &PaneLayout, repo_name: &str, feature_name: &str) -> Result<()> {
    let oc_session = opencode_session_name(repo_name, feature_name);
    let cli_session = cli_session_name(repo_name, feature_name);

    let opencode_pane = layout.opencode_pane.as_ref()
        .context("Right panes not created yet")?;
    let cli_pane = layout.cli_pane.as_ref()
        .context("Right panes not created yet")?;

    attach_pane_to_session(opencode_pane, &oc_session)?;
    attach_pane_to_session(cli_pane, &cli_session)?;

    // Focus the opencode pane so the user can start interacting
    let _ = Command::new("tmux")
        .args(["select-pane", "-t", opencode_pane])
        .output();

    Ok(())
}

/// Clear the right panes (empty shells).
pub fn clear_feature(layout: &PaneLayout) -> Result<()> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    if let Some(ref pane) = layout.opencode_pane {
        let _ = Command::new("tmux")
            .args(["respawn-pane", "-k", "-t", pane, "-c", &home])
            .output();
    }
    if let Some(ref pane) = layout.cli_pane {
        let _ = Command::new("tmux")
            .args(["respawn-pane", "-k", "-t", pane, "-c", &home])
            .output();
    }
    Ok(())
}

/// Kill both inner sessions for a feature.
pub fn kill_inner_sessions(repo_name: &str, feature_name: &str) -> Result<()> {
    for name in [opencode_session_name(repo_name, feature_name), cli_session_name(repo_name, feature_name)] {
        if session_exists(&name) {
            let _ = Command::new("tmux")
                .args(["kill-session", "-t", &name])
                .output();
        }
    }
    Ok(())
}

/// Check if a feature's inner sessions are alive (at least the opencode one).
pub fn inner_sessions_alive(repo_name: &str, feature_name: &str) -> bool {
    session_exists(&opencode_session_name(repo_name, feature_name))
}

/// Kill all inner sessions whose names start with the repo prefix.
pub fn kill_all_inner_sessions(repo_name: &str) -> Result<()> {
    let prefix = format!("oc-{}-", repo_name);
    let output = Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_name}"])
        .output();

    if let Ok(output) = output {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for session in stdout.lines() {
            if session.starts_with(&prefix) {
                let _ = Command::new("tmux")
                    .args(["kill-session", "-t", session])
                    .output();
            }
        }
    }
    Ok(())
}

/// Capture the visible text of a feature's opencode inner session pane.
pub fn capture_opencode_pane(repo_name: &str, feature_name: &str) -> Result<String> {
    let session = opencode_session_name(repo_name, feature_name);
    let output = Command::new("tmux")
        .args(["capture-pane", "-t", &session, "-p"])
        .output()
        .context("Failed to capture pane")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Failed to capture pane: {}", stderr.trim());
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Kill the outer opencrowd tmux session (the one we're running in).
/// This will terminate our own process, so call this last.
pub fn kill_outer_session(repo_name: &str) -> Result<()> {
    let session_name = format!("opencrowd-{}", repo_name);
    if session_exists(&session_name) {
        let _ = Command::new("tmux")
            .args(["kill-session", "-t", &session_name])
            .output();
    }
    Ok(())
}
