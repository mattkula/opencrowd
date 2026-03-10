use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum FeatureStatus {
    #[serde(alias = "Active")]
    Working,
    WaitingForInput,
    #[serde(alias = "Completed")]
    Idle,
    Stopped,
}

impl fmt::Display for FeatureStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FeatureStatus::Working => write!(f, "Working"),
            FeatureStatus::WaitingForInput => write!(f, "Waiting"),
            FeatureStatus::Idle => write!(f, "Idle"),
            FeatureStatus::Stopped => write!(f, "Stopped"),
        }
    }
}

impl FeatureStatus {
    pub fn symbol(&self) -> &str {
        match self {
            FeatureStatus::Working => ">>",
            FeatureStatus::WaitingForInput => "??",
            FeatureStatus::Idle => "ok",
            FeatureStatus::Stopped => "--",
        }
    }

    pub fn color(&self) -> ratatui::style::Color {
        match self {
            FeatureStatus::Working => ratatui::style::Color::Green,
            FeatureStatus::WaitingForInput => ratatui::style::Color::Yellow,
            FeatureStatus::Idle => ratatui::style::Color::Cyan,
            FeatureStatus::Stopped => ratatui::style::Color::DarkGray,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Feature {
    pub name: String,
    pub branch: String,
    pub worktree_path: String,
    pub status: FeatureStatus,
    pub created_at: DateTime<Utc>,
    pub description: Option<String>,
}

impl Feature {
    pub fn new(name: String, repo_name: &str, base_repo_parent: &str) -> Self {
        let branch = format!("kula/{}", name);
        // Worktree is a sibling to the base repo: <parent>/<repo_name>-<feature_name_kebab>
        let kebab_name = name.replace('_', "-").replace('.', "-").to_lowercase();
        let worktree_path = format!("{}/{}-{}", base_repo_parent, repo_name, kebab_name);

        Feature {
            name,
            branch,
            worktree_path,
            status: FeatureStatus::Idle,
            created_at: Utc::now(),
            description: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppState {
    pub base_repo_path: String,
    pub repo_name: String,
    pub features: Vec<Feature>,
}

impl AppState {
    pub fn new(base_repo_path: String, repo_name: String) -> Self {
        AppState {
            base_repo_path,
            repo_name,
            features: Vec::new(),
        }
    }
}
