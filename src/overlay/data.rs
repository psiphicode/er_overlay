use crate::debug_log;
use serde::Deserialize;
use std::collections::HashMap;
use std::{fs, path::Path};

use std::sync::{Arc, RwLock};

#[derive(Default)]
pub struct AppState {
    pub key_item_quantity: u32,
    pub event_flags: HashMap<i32, bool>,
    pub goal_complete: bool,
    pub initialized: bool,
    pub death_count: u32,
    pub great_runes: i32,
}

pub type SharedState = Arc<RwLock<AppState>>;

pub fn create_state() -> SharedState {
    Arc::new(RwLock::new(AppState::default()))
}

#[derive(Debug, Deserialize, Clone)]
pub struct BossEntry {
    pub boss: String,
    pub place: String,
    pub flag_id: i32,
    #[serde(default)]
    pub remembrance: Option<i32>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct RegionData {
    pub region_name: String,
    #[serde(default)]
    pub regions: Vec<i32>,
    #[serde(default)]
    pub bosses: Vec<BossEntry>,
}

pub type BossRegions = Vec<RegionData>;

/// Loads the localized boss data JSON file according to `language` in config.
///
/// Search order:
/// 1️⃣ `data/<language>/bosses.json`
/// 2️⃣ `data/engus/bosses.json`
/// 3️⃣ `data/bosses.json`
pub fn load_localized_boss_data(
    config_dir: &Path,
    language: &str,
    data_file: &str,
) -> Option<BossRegions> {
    // normalize language (trim + lowercase for directory consistency)
    let lang_norm = language.trim();
    let lang_norm = if lang_norm.is_empty() {
        "engus"
    } else {
        lang_norm
    };

    // construct candidate search order
    let candidates = [
        config_dir.join(format!("data/{}/{}", lang_norm, data_file)),
        config_dir.join(format!("data/engus/{}", data_file)),
        config_dir.join(format!("data/{}", data_file)),
    ];

    for path in candidates {
        if !path.exists() {
            debug_log!(
                "[ignite_overlay] ⚠ Boss data not found at '{}'",
                path.display()
            );
            continue;
        }

        match fs::read_to_string(&path) {
            Ok(contents) => match serde_json::from_str::<BossRegions>(&contents) {
                Ok(data) => {
                    debug_log!(
                        "[ignite_overlay] ✅ Loaded boss data from '{}'",
                        path.display()
                    );
                    return Some(data);
                }
                Err(e) => {
                    debug_log!(
                        "[ignite_overlay] ❌ Failed to parse JSON '{}': {:?}",
                        path.display(),
                        e
                    );
                }
            },
            Err(e) => {
                debug_log!(
                    "[ignite_overlay] ❌ Could not read '{}': {:?}",
                    path.display(),
                    e
                );
            }
        }
    }

    debug_log!("[ignite_overlay] ❌ No valid boss data file found");
    None
}
