//! Per-user application data directory.
//!
//! The location mirrors the historical one, so existing sync history and
//! recovery state survive the engine change. The GUI never migrates or
//! deletes it.

use std::path::PathBuf;

pub fn app_data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("TEAMUP_SHIFT_SYNC_DATA_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(dirs) = directories::ProjectDirs::from("", "", "teamup-shift-sync") {
        return dirs.data_dir().to_path_buf();
    }
    PathBuf::from(".local")
}
