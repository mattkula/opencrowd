use anyhow::Result;

use crate::git;
use crate::model::{AppState, Feature, FeatureStatus};
use crate::persist;
use crate::tmux::{self, PaneLayout};

#[derive(Debug, Clone, PartialEq)]
pub enum InputMode {
    Normal,
    CreatingFeature,
    ConfirmDelete,
    ConfirmDeleteBranch,
}

pub struct App {
    pub state: AppState,
    pub selected_index: usize,
    pub input_mode: InputMode,
    pub input_buffer: String,
    pub status_message: Option<String>,
    pub should_quit: bool,
    pub should_quit_and_kill: bool,
    pub should_detach: bool,
    pub layout: Option<PaneLayout>,
    /// Name of the feature currently displayed in the right-side panes.
    pub active_feature: Option<String>,
    pub delete_candidate: Option<usize>,
}

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
        }
    }

    pub fn set_layout(&mut self, layout: PaneLayout) {
        self.layout = Some(layout);
    }

    pub fn selected_feature(&self) -> Option<&Feature> {
        self.state.features.get(self.selected_index)
    }

    pub fn move_selection_up(&mut self) {
        if self.selected_index > 0 {
            self.selected_index -= 1;
        }
    }

    pub fn move_selection_down(&mut self) {
        if !self.state.features.is_empty() && self.selected_index < self.state.features.len() - 1 {
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
        self.selected_index = self.state.features.len() - 1;

        persist::save_state(&self.state)?;

        self.input_mode = InputMode::Normal;
        self.input_buffer.clear();
        self.status_message = Some(format!("Created feature '{}'", name));

        // Automatically show the new feature
        self.open_selected()?;

        Ok(())
    }

    /// Show the selected feature in the right-side panes.
    ///
    /// Each feature has its own inner tmux session that persists in the background.
    /// The right-side panes run `tmux attach -t <inner-session>:<window>`.
    /// Switching features just respawns those panes with a new attach command —
    /// the old inner session detaches and keeps running, the new one attaches.
    pub fn open_selected(&mut self) -> Result<()> {
        let layout = match &self.layout {
            Some(l) => l.clone(),
            None => {
                self.status_message = Some("No tmux layout available".to_string());
                return Ok(());
            }
        };

        if self.state.features.is_empty() {
            return Ok(());
        }

        let idx = self.selected_index;
        let feature = &self.state.features[idx];
        let feature_name = feature.name.clone();

        // Don't re-open if already showing this feature
        if self.active_feature.as_ref() == Some(&feature_name) {
            return Ok(());
        }

        // Ensure the inner sessions exist (might need recreation after reboot)
        if !tmux::inner_sessions_alive(&self.state.repo_name, &feature_name) {
            tmux::create_inner_sessions(&self.state.repo_name, &feature_name, &feature.worktree_path)?;
            self.state.features[idx].status = FeatureStatus::Active;
            persist::save_state(&self.state)?;
        }

        // Respawn the right-side panes to attach to this feature's inner session
        tmux::show_feature(&layout, &self.state.repo_name, &feature_name)?;

        self.active_feature = Some(feature_name.clone());
        self.status_message = Some(format!("Opened '{}'", feature_name));

        Ok(())
    }

    pub fn start_delete_feature(&mut self) {
        if self.state.features.is_empty() {
            self.status_message = Some("No features to delete".to_string());
            return;
        }
        self.delete_candidate = Some(self.selected_index);
        self.input_mode = InputMode::ConfirmDelete;
        if let Some(feature) = self.state.features.get(self.selected_index) {
            self.status_message = Some(format!(
                "Delete feature '{}'? (y/n)",
                feature.name
            ));
        }
    }

    pub fn confirm_delete_feature(&mut self) -> Result<()> {
        let idx = match self.delete_candidate {
            Some(idx) => idx,
            None => return Ok(()),
        };

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
        let idx = match self.delete_candidate {
            Some(idx) => idx,
            None => return Ok(()),
        };

        let feature = &self.state.features[idx];
        let name = feature.name.clone();
        let branch = feature.branch.clone();

        if delete {
            if let Err(e) = git::delete_branch(&self.state.base_repo_path, &branch) {
                self.status_message = Some(format!("Warning: {}", e));
            }
        }

        self.state.features.remove(idx);

        if self.selected_index >= self.state.features.len() && !self.state.features.is_empty() {
            self.selected_index = self.state.features.len() - 1;
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
        for feature in &mut self.state.features {
            if !std::path::Path::new(&feature.worktree_path).exists() {
                feature.status = FeatureStatus::Stopped;
                continue;
            }

            if tmux::inner_sessions_alive(&self.state.repo_name, &feature.name) {
                feature.status = FeatureStatus::Active;
            } else {
                // Worktree exists but no tmux session — will be recreated on open
                feature.status = FeatureStatus::Stopped;
            }
        }
    }
}
