mod app;
mod git;
mod model;
mod persist;
mod tmux;
mod ui;

use std::io;
use std::time::Duration;

use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

use app::{App, InputMode};
use model::AppState;

/// Environment variable set when opencrowd re-launches itself inside tmux.
/// When present, we know we're in the correct tmux session and should run the TUI directly.
const INSIDE_MARKER: &str = "OPENCROWD_SESSION";

fn main() -> Result<()> {
    // Pre-flight checks
    let repo_path = git::ensure_git_repo()?;
    tmux::ensure_tmux()?;

    let repo_name = git::repo_name(&repo_path);
    let session_name = format!("opencrowd-{}", repo_name);

    // If OPENCROWD_SESSION is set, we were launched by ourselves inside tmux.
    // Go straight to TUI mode.
    if std::env::var(INSIDE_MARKER).is_ok() {
        return run_app(&repo_path, &repo_name);
    }

    // We're the outer invocation. We need to ensure a tmux session exists
    // and get ourselves running inside it.
    ensure_tmux_session(&session_name, &repo_path)?;

    Ok(())
}

/// Ensure the opencrowd tmux session is running and attach to it.
fn ensure_tmux_session(session_name: &str, repo_path: &str) -> Result<()> {
    if tmux::inside_tmux() {
        // Already in tmux. Check if we're in the right session.
        if let Some(current) = tmux::current_session_name() {
            if current == session_name {
                // We're in the right session already but without the marker,
                // which means user ran opencrowd manually in the session.
                // Just run the app directly.
                return run_app(repo_path, &git::repo_name(repo_path));
            }
        }

        // We're in tmux but not in the opencrowd session.
        if tmux::session_exists(session_name) {
            // Session exists, switch to it.
            eprintln!("Switching to existing opencrowd session...");
            tmux::switch_client(session_name)?;
        } else {
            // Create the session and switch to it.
            // The new session runs opencrowd with the marker env var.
            let exe = std::env::current_exe()?;
            let cmd = format!("{}={} {}", INSIDE_MARKER, "1", exe.display());

            let output = std::process::Command::new("tmux")
                .args([
                    "new-session",
                    "-d",
                    "-s", session_name,
                    "-c", repo_path,
                    &cmd,
                ])
                .output()?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!("Failed to create tmux session: {}", stderr.trim());
            }

            tmux::switch_client(session_name)?;
        }
    } else {
        // Not in tmux at all.
        if tmux::session_exists(session_name) {
            // Session already exists, just attach.
            eprintln!("Attaching to existing opencrowd session...");
            tmux::attach_session(session_name)?;
        } else {
            // Create a new session running opencrowd with the marker.
            let exe = std::env::current_exe()?;
            let cmd = format!("{}={} {}", INSIDE_MARKER, "1", exe.display());
            tmux::create_session_and_attach(session_name, repo_path, &cmd)?;
        }
    }

    Ok(())
}

/// Run the actual TUI application (called from inside the correct tmux session).
fn run_app(repo_path: &str, repo_name: &str) -> Result<()> {
    // Load or create state
    let state = match persist::load_state(repo_path)? {
        Some(state) => state,
        None => AppState::new(repo_path.to_string(), repo_name.to_string()),
    };

    let mut app = App::new(state);
    app.reconcile();

    // Create the 3-pane tmux layout
    let layout = tmux::create_layout()?;
    app.set_layout(layout);

    // Save reconciled state
    persist::save_state(&app.state)?;

    // Run TUI loop
    run_tui(&mut app)
}

fn run_tui(app: &mut App) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = tui_loop(&mut terminal, app);

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    result
}

fn tui_loop(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|f| ui::draw(f, app))?;

        if app.should_quit {
            return Ok(());
        }

        if app.should_quit_and_kill {
            // Kill all inner sessions for this repo, then kill the outer session.
            // Killing the outer session terminates our process, so this won't return.
            let repo_name = app.state.repo_name.clone();
            let _ = tmux::kill_all_inner_sessions(&repo_name);
            let _ = tmux::kill_outer_session(&repo_name);
            // If kill_outer_session didn't terminate us (shouldn't happen), exit cleanly
            return Ok(());
        }

        if app.should_detach {
            // Detach from tmux — leave the process running.
            // Don't leave alternate screen; tmux preserves the pane contents.
            // When the user reattaches, tmux restores the pane and we continue
            // the event loop, redrawing on the next iteration.
            tmux::detach_client()?;
            // After reattach, force a full redraw so the TUI is visible.
            terminal.clear()?;
            app.should_detach = false;
            continue;
        }

        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                match app.input_mode {
                    InputMode::Normal => handle_normal_input(app, key.code, key.modifiers)?,
                    InputMode::CreatingFeature => handle_create_input(app, key.code)?,
                    InputMode::ConfirmDelete => handle_confirm_delete(app, key.code)?,
                    InputMode::ConfirmDeleteBranch => handle_confirm_branch_delete(app, key.code)?,
                }
            }
        }
    }
}

fn handle_normal_input(app: &mut App, key: KeyCode, modifiers: KeyModifiers) -> Result<()> {
    match key {
        KeyCode::Char('q') => {
            // Detach from tmux, leave everything running in background
            app.should_detach = true;
        }
        KeyCode::Char('Q') => {
            // Quit and kill all sessions for this repo
            app.should_quit_and_kill = true;
        }
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
            app.should_detach = true;
        }
        KeyCode::Char('n') => {
            app.start_create_feature();
        }
        KeyCode::Char('d') => {
            app.start_delete_feature();
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.move_selection_up();
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.move_selection_down();
        }
        KeyCode::Enter => {
            app.open_selected()?;
        }
        _ => {}
    }
    Ok(())
}

fn handle_create_input(app: &mut App, key: KeyCode) -> Result<()> {
    match key {
        KeyCode::Enter => {
            if let Err(e) = app.confirm_create_feature() {
                app.status_message = Some(format!("Error: {}", e));
                app.input_mode = InputMode::Normal;
            }
        }
        KeyCode::Esc => {
            app.cancel_input();
        }
        KeyCode::Char(c) => {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                app.input_buffer.push(c);
            }
        }
        KeyCode::Backspace => {
            app.input_buffer.pop();
        }
        _ => {}
    }
    Ok(())
}

fn handle_confirm_delete(app: &mut App, key: KeyCode) -> Result<()> {
    match key {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            if let Err(e) = app.confirm_delete_feature() {
                app.status_message = Some(format!("Error: {}", e));
                app.input_mode = InputMode::Normal;
                app.delete_candidate = None;
            }
        }
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
            app.cancel_input();
        }
        _ => {}
    }
    Ok(())
}

fn handle_confirm_branch_delete(app: &mut App, key: KeyCode) -> Result<()> {
    match key {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            if let Err(e) = app.confirm_delete_branch(true) {
                app.status_message = Some(format!("Error: {}", e));
                app.input_mode = InputMode::Normal;
                app.delete_candidate = None;
            }
        }
        KeyCode::Char('n') | KeyCode::Char('N') => {
            if let Err(e) = app.confirm_delete_branch(false) {
                app.status_message = Some(format!("Error: {}", e));
                app.input_mode = InputMode::Normal;
                app.delete_candidate = None;
            }
        }
        KeyCode::Esc => {
            if let Err(e) = app.confirm_delete_branch(false) {
                app.status_message = Some(format!("Error: {}", e));
                app.input_mode = InputMode::Normal;
                app.delete_candidate = None;
            }
        }
        _ => {}
    }
    Ok(())
}
