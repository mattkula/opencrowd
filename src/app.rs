use std::time::Instant;

use anyhow::Result;

use crate::git;
use crate::model::{AppState, Feature, FeatureStatus};
use crate::persist;
use crate::status;
use crate::tmux::{self, PaneLayout};

#[derive(Debug, Clone, PartialEq)]
pub enum InputMode {
    Normal,
    CreatingFeature,
    ConfirmDelete,
    ConfirmDeleteBranch,
}

/// Special name used for the "base" entry's inner tmux sessions.
const BASE_NAME: &str = "base";

pub struct App {
    pub state: AppState,
    /// Selected index in the displayed list. 0 = base, 1+ = features.
    pub selected_index: usize,
    pub input_mode: InputMode,
    pub input_buffer: String,
    pub status_message: Option<String>,
    pub should_quit: bool,
    pub should_quit_and_kill: bool,
    pub should_detach: bool,
    pub layout: Option<PaneLayout>,
    /// Name of the entry currently displayed in the right-side panes.
    /// "base" for the base repo, or a feature name.
    pub active_feature: Option<String>,
    pub delete_candidate: Option<usize>,
    /// Timestamp of the last status poll, used to throttle polling to every ~2s.
    pub last_status_poll: Instant,
    /// Animation frame counter for spinner, incremented each render tick.
    pub spinner_frame: usize,
    /// Whether the TUI pane currently has tmux focus.
    pub tui_focused: bool,
    /// Status of the base repo entry.
    pub base_status: FeatureStatus,
}

/// How often to poll feature statuses.
const STATUS_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

impl App {
    pub fn new(state: AppState) -> Self {
        App {
            state,
            selected_index: 0,
            input_mode: InputMode::Normal,
            input_buffer: String::new(),
            status_message: None,
            should_quit: false,
            should_quit_and_kill: false,
            should_detach: false,
            layout: None,
            active_feature: None,
            delete_candidate: None,
            last_status_poll: Instant::now(),
            spinner_frame: 0,
            tui_focused: true,
            base_status: FeatureStatus::Idle,
        }
    }

    pub fn set_layout(&mut self, layout: PaneLayout) {
        self.layout = Some(layout);
    }

    /// Returns true if the "base" entry is selected (index 0).
    pub fn is_base_selected(&self) -> bool {
        self.selected_index == 0
    }

    /// Get the selected feature, or None if "base" is selected.
    pub fn selected_feature(&self) -> Option<&Feature> {
        if self.selected_index == 0 {
            None
        } else {
            self.state.features.get(self.selected_index - 1)
        }
    }

    /// Total number of entries in the list (base + features).
    pub fn total_entries(&self) -> usize {
        1 + self.state.features.len()
    }

    pub fn move_selection_up(&mut self) {
        if self.selected_index > 0 {
            self.selected_index -= 1;
        }
    }

    pub fn move_selection_down(&mut self) {
        if self.selected_index < self.total_entries() - 1 {
            self.selected_index += 1;
        }
    }

    pub fn start_create_feature(&mut self) {
        self.input_mode = InputMode::CreatingFeature;
        self.input_buffer.clear();
        self.status_message = Some("Enter feature name:".to_string());
    }

    pub fn cancel_input(&mut self) {
        self.input_mode = InputMode::Normal;
        self.input_buffer.clear();
        self.status_message = None;
        self.delete_candidate = None;
    }

    pub fn confirm_create_feature(&mut self) -> Result<()> {
        let name = self.input_buffer.trim().to_string();
        if name.is_empty() {
            self.status_message = Some("Feature name cannot be empty".to_string());
            return Ok(());
        }

        if self.state.features.iter().any(|f| f.name == name) {
            self.status_message = Some(format!("Feature '{}' already exists", name));
            return Ok(());
        }

        let repo_parent = git::repo_parent(&self.state.base_repo_path);
        let feature = Feature::new(
            name.clone(),
            &self.state.repo_name,
            &repo_parent,
        );

        // Create the git worktree
        git::create_worktree(
            &self.state.base_repo_path,
            &feature.worktree_path,
            &feature.branch,
        )?;

        // Create the inner tmux sessions for this feature
        tmux::create_inner_sessions(&self.state.repo_name, &feature.name, &feature.worktree_path)?;

        self.state.features.push(feature);
        self.selected_index = self.state.features.len(); // +1 offset for base

        persist::save_state(&self.state)?;

        self.input_mode = InputMode::Normal;
        self.input_buffer.clear();
        self.status_message = Some(format!("Created feature '{}'", name));

        // Automatically show the new feature
        self.open_selected()?;

        Ok(())
    }

    /// Show the selected entry (base or feature) in the right-side panes.
    ///
    /// Each entry has its own inner tmux sessions that persist in the background.
    /// The right-side panes run `tmux attach -t <inner-session>`.
    /// Switching entries just respawns those panes with a new attach command —
    /// the old inner session detaches and keeps running, the new one attaches.
    pub fn open_selected(&mut self) -> Result<()> {
        if self.layout.is_none() {
            self.status_message = Some("No tmux layout available".to_string());
            return Ok(());
        }

        if self.is_base_selected() {
            if self.active_feature.as_deref() == Some(BASE_NAME) {
                return Ok(());
            }

            let repo_name = self.state.repo_name.clone();
            let base_path = self.state.base_repo_path.clone();

            if !tmux::inner_sessions_alive(&repo_name, BASE_NAME) {
                tmux::create_inner_sessions(&repo_name, BASE_NAME, &base_path)?;
            }

            let layout = self.layout.as_mut().unwrap();
            tmux::ensure_right_panes(layout)?;
            tmux::show_feature(layout, &repo_name, BASE_NAME)?;

            self.active_feature = Some(BASE_NAME.to_string());
            self.status_message = Some("Opened 'base'".to_string());
        } else {
            let feature_idx = self.selected_index - 1;
            let feature_name = self.state.features[feature_idx].name.clone();
            let worktree_path = self.state.features[feature_idx].worktree_path.clone();
            let repo_name = self.state.repo_name.clone();

            // Don't re-open if already showing this feature
            if self.active_feature.as_ref() == Some(&feature_name) {
                return Ok(());
            }

            // Ensure the inner sessions exist (might need recreation after reboot)
            if !tmux::inner_sessions_alive(&repo_name, &feature_name) {
                tmux::create_inner_sessions(&repo_name, &feature_name, &worktree_path)?;
                self.state.features[feature_idx].status = FeatureStatus::Idle;
                persist::save_state(&self.state)?;
            }

            let layout = self.layout.as_mut().unwrap();
            tmux::ensure_right_panes(layout)?;
            tmux::show_feature(layout, &repo_name, &feature_name)?;

            self.active_feature = Some(feature_name.clone());
            self.status_message = Some(format!("Opened '{}'", feature_name));
        }

        Ok(())
    }

    pub fn start_delete_feature(&mut self) {
        if self.is_base_selected() {
            self.status_message = Some("Cannot delete base".to_string());
            return;
        }
        if self.state.features.is_empty() {
            self.status_message = Some("No features to delete".to_string());
            return;
        }
        self.delete_candidate = Some(self.selected_index);
        self.input_mode = InputMode::ConfirmDelete;
        if let Some(feature) = self.selected_feature() {
            self.status_message = Some(format!(
                "Delete feature '{}'? (y/n)",
                feature.name
            ));
        }
    }

    pub fn confirm_delete_feature(&mut self) -> Result<()> {
        let display_idx = match self.delete_candidate {
            Some(idx) => idx,
            None => return Ok(()),
        };

        let idx = display_idx - 1; // offset for base
        let feature = self.state.features[idx].clone();

        // If this feature is currently shown, clear the right panes
        if self.active_feature.as_ref() == Some(&feature.name) {
            if let Some(layout) = &self.layout {
                let _ = tmux::clear_feature(layout);
            }
            self.active_feature = None;
        }

        // Kill the inner tmux sessions
        let _ = tmux::kill_inner_sessions(&self.state.repo_name, &feature.name);

        // Remove worktree
        if let Err(e) = git::remove_worktree(&self.state.base_repo_path, &feature.worktree_path) {
            self.status_message = Some(format!("Warning: {}", e));
        }

        // Ask about branch deletion
        self.input_mode = InputMode::ConfirmDeleteBranch;
        self.status_message = Some(format!(
            "Also delete branch '{}'? (y/n)",
            feature.branch
        ));

        Ok(())
    }

    pub fn confirm_delete_branch(&mut self, delete: bool) -> Result<()> {
        let display_idx = match self.delete_candidate {
            Some(idx) => idx,
            None => return Ok(()),
        };

        let idx = display_idx - 1; // offset for base
        let feature = &self.state.features[idx];
        let name = feature.name.clone();
        let branch = feature.branch.clone();

        if delete {
            if let Err(e) = git::delete_branch(&self.state.base_repo_path, &branch) {
                self.status_message = Some(format!("Warning: {}", e));
            }
        }

        self.state.features.remove(idx);

        if self.selected_index >= self.total_entries() {
            self.selected_index = self.total_entries() - 1;
        }

        persist::save_state(&self.state)?;

        self.input_mode = InputMode::Normal;
        self.delete_candidate = None;
        if self.status_message.as_ref().map_or(true, |m| !m.starts_with("Warning")) {
            self.status_message = Some(format!("Deleted feature '{}'", name));
        }

        Ok(())
    }

    /// Reconcile persisted state with actual worktrees/sessions on startup.
    pub fn reconcile(&mut self) {
        // Reconcile base
        self.base_status = status::detect_status(
            &self.state.repo_name,
            BASE_NAME,
            &self.state.base_repo_path,
        );

        for feature in &mut self.state.features {
            feature.status = status::detect_status(
                &self.state.repo_name,
                &feature.name,
                &feature.worktree_path,
            );
        }
    }

    /// Poll feature statuses if enough time has elapsed since the last poll.
    /// Returns true if any status changed (caller should trigger a redraw).
    pub fn poll_statuses(&mut self) -> bool {
        if self.last_status_poll.elapsed() < STATUS_POLL_INTERVAL {
            return false;
        }
        self.last_status_poll = Instant::now();

        let mut changed = false;
        let repo_name = self.state.repo_name.clone();
        let base_path = self.state.base_repo_path.clone();

        // Poll base status
        let new_base = status::detect_status(&repo_name, BASE_NAME, &base_path);
        if self.base_status != new_base {
            self.base_status = new_base;
            changed = true;
        }

        for feature in &mut self.state.features {
            let new_status = status::detect_status(
                &repo_name,
                &feature.name,
                &feature.worktree_path,
            );

            if feature.status != new_status {
                feature.status = new_status;
                changed = true;
            }
        }

        if changed {
            let _ = persist::save_state(&self.state);
        }

        changed
    }
}
