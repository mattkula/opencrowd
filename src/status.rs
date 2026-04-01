use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Instant;

use rusqlite::Connection;

use crate::model::FeatureStatus;
use crate::tmux;

/// How often to refresh the worktree→session-ID mapping from the database.
/// Session IDs rarely change, so 30 seconds is plenty.
const SESSION_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(30);

// ===========================================================================
// Persistent status-detection context
// ===========================================================================

/// Holds a reusable SQLite connection and a cache of worktree→session-ID
/// mappings so that status polling doesn't re-open the database or re-scan
/// the session table on every cycle.
pub struct StatusContext {
    /// Persistent read-only connection to the opencode database.
    conn: Option<Connection>,
    /// Path we opened the connection from (so we can detect if it changes).
    db_path: Option<PathBuf>,
    /// Map from worktree path → opencode session ID.
    session_cache: HashMap<String, String>,
    /// When the session cache was last refreshed.
    cache_refreshed: Instant,
}

impl StatusContext {
    pub fn new() -> Self {
        let db_path = opencode_db_path();
        let conn = db_path.as_ref().and_then(|p| open_readonly(p));

        StatusContext {
            conn,
            db_path,
            session_cache: HashMap::new(),
            cache_refreshed: Instant::now() - SESSION_CACHE_TTL, // force initial refresh
        }
    }

    /// Refresh the session-ID cache if the TTL has elapsed.
    /// Call this once per poll cycle, *before* calling `detect_status` for
    /// each feature.
    pub fn refresh_session_cache(&mut self, worktree_paths: &[&str]) {
        if self.cache_refreshed.elapsed() < SESSION_CACHE_TTL {
            return;
        }

        // Make sure the connection is alive. If the DB didn't exist at
        // startup but does now, try opening it.
        self.ensure_connection();

        if let Some(ref conn) = self.conn {
            self.session_cache = build_session_cache(conn, worktree_paths);
        }
        self.cache_refreshed = Instant::now();
    }

    /// Force-invalidate a single worktree's cached session ID (e.g. after a
    /// new opencode session is created for that worktree).
    #[allow(dead_code)]
    pub fn invalidate(&mut self, worktree_path: &str) {
        self.session_cache.remove(worktree_path);
    }

    /// Ensure the SQLite connection is open, re-opening if necessary.
    fn ensure_connection(&mut self) {
        // If we already have a connection, check it's still valid.
        if self.conn.is_some() {
            return;
        }

        // Try (re)opening.
        let path = opencode_db_path();
        if let Some(ref p) = path {
            self.conn = open_readonly(p);
            self.db_path = path;
        }
    }
}

// ===========================================================================
// Database helpers
// ===========================================================================

/// Find the global opencode database.
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

/// Open the database read-only so we never block opencode's writes.
fn open_readonly(path: &PathBuf) -> Option<Connection> {
    let conn = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;

    // Busy timeout in case of WAL checkpoints.
    let _ = conn.busy_timeout(std::time::Duration::from_millis(50));

    Some(conn)
}

/// Build a map from worktree path → most-recent opencode session ID for all
/// given worktree paths in a **single query**.
fn build_session_cache(conn: &Connection, worktree_paths: &[&str]) -> HashMap<String, String> {
    let mut cache = HashMap::new();

    if worktree_paths.is_empty() {
        return cache;
    }

    // Build a single query with placeholders for all paths.  SQLite handles
    // up to 999 bind parameters by default; we're unlikely to exceed that.
    let placeholders: Vec<&str> = worktree_paths.iter().map(|_| "?").collect();
    let sql = format!(
        "SELECT s.directory, s.id
         FROM session s
         INNER JOIN (
             SELECT directory, MAX(time_updated) AS max_tu
             FROM session
             WHERE directory IN ({})
             GROUP BY directory
         ) latest ON s.directory = latest.directory AND s.time_updated = latest.max_tu",
        placeholders.join(",")
    );

    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(_) => return cache,
    };

    let params: Vec<&dyn rusqlite::types::ToSql> = worktree_paths
        .iter()
        .map(|p| p as &dyn rusqlite::types::ToSql)
        .collect();

    if let Ok(mut rows) = stmt.query(params.as_slice()) {
        while let Ok(Some(row)) = rows.next() {
            if let (Ok(dir), Ok(id)) = (row.get::<_, String>(0), row.get::<_, String>(1)) {
                cache.insert(dir, id);
            }
        }
    }

    cache
}

// ===========================================================================
// Public API
// ===========================================================================

/// Detect the current status of a feature.
///
/// Accepts pre-fetched data to avoid redundant work:
///   - `live_sessions`: set of currently alive tmux session names (from a
///     single `tmux list-sessions` call).
///   - `ctx`: persistent status context with DB connection and session cache.
pub fn detect_status(
    repo_name: &str,
    feature_name: &str,
    worktree_path: &str,
    live_sessions: &HashSet<String>,
    ctx: &StatusContext,
) -> FeatureStatus {
    // 1. Session alive?  O(1) lookup in the pre-fetched set.
    let oc_session = tmux::opencode_session_name(repo_name, feature_name);
    if !live_sessions.contains(&oc_session) {
        return FeatureStatus::Stopped;
    }

    // 2. Capture the pane text (single subprocess per feature — unavoidable).
    let pane_text = tmux::capture_opencode_pane(repo_name, feature_name).ok();

    // Check if opencode needs user input (must happen before DB check).
    if let Some(ref text) = pane_text {
        if needs_user_input(text) {
            return FeatureStatus::WaitingForInput;
        }
    }

    // 3. Query the database using the cached session ID.
    if let Some(ref conn) = ctx.conn {
        if let Some(session_id) = ctx.session_cache.get(worktree_path) {
            if let Some(status) = query_latest_message(conn, session_id) {
                return status;
            }
        }
    }

    // 4. DB had no data — fall back to pane heuristics.
    if let Some(ref text) = pane_text {
        return detect_from_pane(text);
    }

    // 5. Session is alive but no further info — assume working.
    FeatureStatus::Working
}

// ===========================================================================
// Pane text analysis
// ===========================================================================

fn needs_user_input(pane_text: &str) -> bool {
    if pane_text.contains("esc dismiss") {
        return true;
    }
    if pane_text.contains("Allow") && pane_text.contains("Deny") {
        return true;
    }
    false
}

fn detect_from_pane(pane_text: &str) -> FeatureStatus {
    let has_command_hint = pane_text.contains("ctrl+p");
    let has_completed_marker = pane_text.contains('\u{25A3}');

    if has_command_hint && has_completed_marker {
        return FeatureStatus::Idle;
    }
    if has_command_hint {
        return FeatureStatus::Idle;
    }
    FeatureStatus::Working
}

// ===========================================================================
// SQLite: query latest message for a known session ID
// ===========================================================================

/// Query the latest message for a specific session ID.  This is a simple
/// indexed lookup (by session_id) + ORDER BY + LIMIT 1 — fast even on large
/// databases.
fn query_latest_message(conn: &Connection, session_id: &str) -> Option<FeatureStatus> {
    let result: Option<(String, Option<String>, Option<i64>)> = conn
        .query_row(
            "SELECT
                json_extract(m.data, '$.role'),
                json_extract(m.data, '$.finish'),
                json_extract(m.data, '$.time.completed')
             FROM message m
             WHERE m.session_id = ?1
             ORDER BY m.time_created DESC
             LIMIT 1",
            [session_id],
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
        "assistant" => match (finish.as_deref(), completed) {
            (Some("stop"), Some(_)) | (Some("end_turn"), Some(_)) => Some(FeatureStatus::Idle),
            (Some("tool-calls"), _) => Some(FeatureStatus::Working),
            (None, None) => Some(FeatureStatus::Working),
            (_, Some(_)) => Some(FeatureStatus::Idle),
            (Some(_), None) => Some(FeatureStatus::Idle),
        },
        "user" => Some(FeatureStatus::Working),
        _ => None,
    }
}
