use std::path::PathBuf;

use rusqlite::Connection;

use crate::model::FeatureStatus;
use crate::tmux;

/// Find the global opencode database.
///
/// opencode uses XDG conventions (~/.local/share/opencode/opencode.db)
/// regardless of platform, so we check that first. Fall back to the
/// platform-native data dir (~/Library/Application Support on macOS)
/// in case a future version changes this.
fn opencode_db_path() -> Option<PathBuf> {
    let home = dirs::home_dir()?;

    // Primary: XDG location (where opencode actually stores it)
    let xdg = home.join(".local/share/opencode/opencode.db");
    if xdg.exists() {
        return Some(xdg);
    }

    // Fallback: platform-native data dir
    let native = dirs::data_local_dir()?.join("opencode").join("opencode.db");
    if native.exists() {
        return Some(native);
    }

    None
}

// ===========================================================================
// Public API
// ===========================================================================

/// Detect the current status of a feature by inspecting its tmux session
/// and opencode database.
///
/// Priority order:
///   1. Is the inner tmux session alive? If not -> Stopped.
///   2. Capture the pane — check for any UI that needs user attention
///      (permission dialog, question prompt) -> WaitingForInput.
///   3. Query the global opencode SQLite DB for the latest message in the
///      most recent session whose directory matches the worktree path.
///   4. If the DB query returns no data but session is alive, use pane
///      heuristics as a fallback.
///   5. Fall back to Working if session is alive but we can't determine more.
pub fn detect_status(repo_name: &str, feature_name: &str, worktree_path: &str) -> FeatureStatus {
    // 1. Session alive?
    if !tmux::inner_sessions_alive(repo_name, feature_name) {
        return FeatureStatus::Stopped;
    }

    // 2. Capture the pane text (reused for all pane-based checks)
    let pane_text = tmux::capture_opencode_pane(repo_name, feature_name).ok();

    // Check if opencode needs user input (permission dialog or question prompt).
    // This must happen BEFORE the DB check because the DB will show the agent
    // as "active" (no finish reason) in both "streaming" and "waiting for
    // answer" states — only the pane UI distinguishes them.
    if let Some(ref text) = pane_text {
        if needs_user_input(text) {
            return FeatureStatus::WaitingForInput;
        }
    }

    // 3. Query the global opencode SQLite database
    if let Some(db_path) = opencode_db_path() {
        if let Some(status) = query_opencode_db(&db_path, worktree_path) {
            return status;
        }
    }

    // 4. DB had no data — fall back to pane heuristics
    if let Some(ref text) = pane_text {
        return detect_from_pane(text);
    }

    // 5. Session is alive but no further info — assume working
    FeatureStatus::Working
}

// ===========================================================================
// Pane text analysis (tmux capture-pane)
// ===========================================================================

/// Check if opencode is showing any UI that requires user input.
///
/// This covers two cases:
///   - **Permission dialog**: opencode asks to "Allow" / "Deny" a tool execution.
///   - **Question prompt**: the agent asked the user a question, showing a
///     selection UI with "↑↓ select  enter submit  esc dismiss" at the bottom.
fn needs_user_input(pane_text: &str) -> bool {
    // Question prompt: the agent used the question tool and is waiting for
    // the user to select an answer. The selection UI footer is unique enough
    // to avoid false positives.
    if pane_text.contains("esc dismiss") {
        return true;
    }

    // Permission dialog: opencode wants to run a tool and needs approval.
    if pane_text.contains("Allow") && pane_text.contains("Deny") {
        return true;
    }

    false
}

/// Detect status from captured pane text when the DB has no data.
fn detect_from_pane(pane_text: &str) -> FeatureStatus {
    // The status bar at the very bottom contains "ctrl+p" when the TUI is
    // in its normal interactive mode (not during streaming).
    let has_command_hint = pane_text.contains("ctrl+p");

    // The filled square character (▣) appears after a completed assistant
    // turn (the model attribution line). During streaming it doesn't exist
    // yet for the current response.
    let has_completed_marker = pane_text.contains('\u{25A3}');

    if has_command_hint && has_completed_marker {
        return FeatureStatus::Idle;
    }

    // ctrl+p visible but no completed marker — opencode loaded, no conversation yet
    if has_command_hint {
        return FeatureStatus::Idle;
    }

    FeatureStatus::Working
}

// ===========================================================================
// SQLite database query
// ===========================================================================

/// Query the global opencode database for the latest message in the most
/// recent session whose directory matches the given worktree path.
fn query_opencode_db(db_path: &PathBuf, worktree_path: &str) -> Option<FeatureStatus> {
    let conn = Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;

    // Busy timeout so we don't fail if opencode is mid-write
    let _ = conn.busy_timeout(std::time::Duration::from_millis(100));

    let result: Option<(String, Option<String>, Option<i64>)> = conn
        .query_row(
            "SELECT
                json_extract(m.data, '$.role'),
                json_extract(m.data, '$.finish'),
                json_extract(m.data, '$.time.completed')
             FROM message m
             WHERE m.session_id = (
                SELECT s.id FROM session s
                WHERE s.directory = ?1
                ORDER BY s.time_updated DESC
                LIMIT 1
             )
             ORDER BY m.time_created DESC
             LIMIT 1",
            [worktree_path],
            |row| {
                let role: String = row.get(0)?;
                let finish: Option<String> = row.get(1)?;
                let completed: Option<i64> = row.get(2)?;
                Ok((role, finish, completed))
            },
        )
        .ok();

    let (role, finish, completed) = result?;

    match role.as_str() {
        "assistant" => {
            match (finish.as_deref(), completed) {
                (Some("stop"), Some(_)) | (Some("end_turn"), Some(_)) => {
                    Some(FeatureStatus::Idle)
                }
                (Some("tool-calls"), _) => {
                    Some(FeatureStatus::Working)
                }
                (None, None) => {
                    Some(FeatureStatus::Working)
                }
                (_, Some(_)) => {
                    Some(FeatureStatus::Idle)
                }
                (Some(_), None) => {
                    Some(FeatureStatus::Idle)
                }
            }
        }
        "user" => {
            Some(FeatureStatus::Working)
        }
        _ => None,
    }
}
