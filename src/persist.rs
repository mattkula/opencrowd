use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;

use crate::model::AppState;

fn state_dir() -> Result<PathBuf> {
    let dir = dirs::data_local_dir()
        .context("Could not determine local data directory")?
        .join("opencrowd");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn state_file_for(repo_path: &str) -> Result<PathBuf> {
    let hash = {
        let mut h: u64 = 5381;
        for b in repo_path.bytes() {
            h = h.wrapping_mul(33).wrapping_add(b as u64);
        }
        h
    };
    let filename = format!("state-{:x}.json", hash);
    Ok(state_dir()?.join(filename))
}

pub fn save_state(state: &AppState) -> Result<()> {
    let path = state_file_for(&state.base_repo_path)?;
    let json = serde_json::to_string_pretty(state)?;
    fs::write(&path, json).context("Failed to write state file")?;
    Ok(())
}

pub fn load_state(repo_path: &str) -> Result<Option<AppState>> {
    let path = state_file_for(repo_path)?;
    if !path.exists() {
        return Ok(None);
    }
    let json = fs::read_to_string(&path).context("Failed to read state file")?;
    let state: AppState = serde_json::from_str(&json).context("Failed to parse state file")?;
    Ok(Some(state))
}
