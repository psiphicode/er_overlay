use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::{Duration, Instant, SystemTime},
};

use crate::overlay::style::{IgniteConfig, TimerMode, VictoryConfig, VictoryMode};

#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    pub common: CommonConfig,
    pub input: InputConfig,
    pub style: StyleConfig,
    pub boss: BossConfig,
    pub overlay: OverlayConfig,
    pub timer: TimerSettings,
    pub victory: VictoryCondition,
}

#[derive(Clone, Debug)]
pub struct CommonConfig {
    pub console: bool,
    pub font: Option<String>,
    pub font_size: f32,
    pub font_scale: f32,
    pub charset: String,
    pub language: String,
}

#[derive(Clone, Debug, Default)]
pub struct InputConfig {
    pub unload: Option<String>,
    pub toggle_full_mode: Option<String>,
    pub click_action: Option<String>,
}

#[derive(Clone, Debug)]
pub struct StyleConfig {
    pub text_color: [u8; 4],
    pub check_mark_color: [u8; 4],
    pub bg_color: [u8; 4],
    pub border_color: [u8; 4],
    pub button_color: [u8; 4],
    pub button_hover_color: [u8; 4],
    pub button_press_color: [u8; 4],
    pub node_color: [u8; 4],
    pub node_hover_color: [u8; 4],
    pub node_press_color: [u8; 4],
    pub scroll_bg_color: [u8; 4],
    pub scroll_color: [u8; 4],
    pub scroll_hover_color: [u8; 4],
    pub scroll_press_color: [u8; 4],
    pub border_width: f32,
    pub rounding: f32,
    pub panel_pos: [f32; 2],
    pub panel_dim: [f32; 2],
}

#[derive(Clone, Debug)]
pub struct BossConfig {
    pub data_file: String,
}

#[derive(Clone, Debug)]
pub struct OverlayConfig {
    pub display_text: String,
    /// Fixed compact-window width in pixels. `None` preserves content sizing.
    pub closed_width: Option<f32>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TimerSettings {
    pub mode: TimerMode,
    pub prep_minutes: u32,
    pub timer_minutes: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum VictoryCondition {
    Checklist,
    BossIds(Vec<i32>),
    OneBoss(i32),
    #[default]
    None,
}

const CONFIG_POLL_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Clone)]
pub struct ConfigSnapshot {
    pub revision: u64,
    pub config: Option<Arc<RuntimeConfig>>,
}

pub type SharedConfig = Arc<RwLock<ConfigSnapshot>>;

pub struct ConfigManager {
    path: PathBuf,
    modified: Option<SystemTime>,
    next_check: Instant,
    shared: SharedConfig,
}

impl ConfigManager {
    pub fn new(path: PathBuf) -> (Self, Option<String>) {
        let (config, error) = match load_config(&path) {
            Ok(config) => (Some(Arc::new(config)), None),
            Err(error) => (None, Some(error)),
        };
        let modified = modified_time(&path);
        let shared = Arc::new(RwLock::new(ConfigSnapshot {
            revision: 0,
            config,
        }));
        (
            Self {
                path,
                modified,
                next_check: Instant::now() + CONFIG_POLL_INTERVAL,
                shared,
            },
            error,
        )
    }

    pub fn shared(&self) -> SharedConfig {
        self.shared.clone()
    }

    pub fn current(&self) -> Option<Arc<RuntimeConfig>> {
        self.shared.read().ok()?.config.clone()
    }

    /// Returns `Ok(true)` only after a changed file has parsed and been
    /// published. Invalid files leave the last known-good snapshot untouched.
    pub fn poll(&mut self, now: Instant) -> Result<bool, String> {
        if now < self.next_check {
            return Ok(false);
        }
        self.next_check = now + CONFIG_POLL_INTERVAL;
        let modified = modified_time(&self.path);
        if modified == self.modified {
            return Ok(false);
        }
        self.modified = modified;

        let config = Arc::new(load_config(&self.path)?);
        let mut shared = self
            .shared
            .write()
            .map_err(|_| "Configuration snapshot lock is poisoned".to_string())?;
        shared.revision = shared.revision.wrapping_add(1);
        shared.config = Some(config);
        Ok(true)
    }
}

fn load_config(path: &Path) -> Result<RuntimeConfig, String> {
    let contents = fs::read_to_string(path)
        .map_err(|error| format!("Failed to read {}: {error}", path.display()))?;
    let file: IgniteConfig = toml::from_str(&contents)
        .map_err(|error| format!("Failed to parse {}: {error}", path.display()))?;
    RuntimeConfig::try_from(file)
        .map_err(|error| format!("Failed to validate {}: {error}", path.display()))
}

impl TryFrom<IgniteConfig> for RuntimeConfig {
    type Error = String;

    fn try_from(file: IgniteConfig) -> Result<Self, Self::Error> {
        let common = file.common.unwrap_or_default();
        let input = file.input.unwrap_or_default();
        let style = file.style.unwrap_or_default();
        let boss = file.boss.unwrap_or_default();
        let overlay = file.overlay.unwrap_or_default();
        let timer = file.timer;
        let victory = validate_victory(file.victory.unwrap_or_default())?;
        Ok(Self {
            common: CommonConfig {
                console: common.console.unwrap_or(false),
                font: common.font.filter(|font| !font.trim().is_empty()),
                font_size: common.font_size.unwrap_or(20.0).clamp(8.0, 128.0),
                font_scale: common.font_scale.unwrap_or(1.0).clamp(0.25, 4.0),
                charset: nonempty(common.charset, "engus"),
                language: nonempty(common.language, "engus"),
            },
            input: InputConfig {
                unload: input.unload,
                toggle_full_mode: input.toggle_full_mode,
                click_action: input.click_action,
            },
            style: StyleConfig {
                text_color: style.text_color.unwrap_or([255, 255, 255, 250]),
                check_mark_color: style.check_mark_color.unwrap_or([138, 43, 226, 230]),
                bg_color: style.bg_color.unwrap_or([18, 18, 30, 220]),
                border_color: style.border_color.unwrap_or([180, 0, 255, 180]),
                button_color: style.button_color.unwrap_or([30, 0, 60, 150]),
                button_hover_color: style.button_hover_color.unwrap_or([50, 50, 120, 170]),
                button_press_color: style.button_press_color.unwrap_or([70, 20, 120, 230]),
                node_color: style.node_color.unwrap_or([15, 15, 30, 160]),
                node_hover_color: style.node_hover_color.unwrap_or([45, 45, 90, 160]),
                node_press_color: style.node_press_color.unwrap_or([65, 25, 100, 180]),
                scroll_bg_color: style.scroll_bg_color.unwrap_or([20, 20, 40, 160]),
                scroll_color: style.scroll_color.unwrap_or([80, 80, 160, 160]),
                scroll_hover_color: style.scroll_hover_color.unwrap_or([100, 100, 200, 180]),
                scroll_press_color: style.scroll_press_color.unwrap_or([140, 100, 220, 200]),
                border_width: style.border_width.unwrap_or(1.0).max(0.0),
                rounding: style.rounding.unwrap_or(7.0).max(0.0),
                panel_pos: style.panel_pos.unwrap_or([-10.0, 10.0]),
                panel_dim: style
                    .panel_dim
                    .unwrap_or([0.15, 0.90])
                    .map(|value| value.clamp(0.05, 1.0)),
            },
            boss: BossConfig {
                data_file: nonempty(boss.data_file, "bosses.json"),
            },
            overlay: OverlayConfig {
                display_text: nonempty(
                    overlay.display_text,
                    crate::overlay::style::DEFAULT_DISPLAY_TEXT,
                ),
                closed_width: overlay.closed_width.map(|width| width.clamp(80.0, 4096.0)),
            },
            timer: TimerSettings {
                mode: timer.as_ref().map(|timer| timer.mode).unwrap_or_default(),
                prep_minutes: timer
                    .as_ref()
                    .and_then(|timer| timer.prep_minutes)
                    .unwrap_or(0),
                timer_minutes: timer
                    .as_ref()
                    .and_then(|timer| timer.timer_minutes)
                    .unwrap_or(0),
            },
            victory,
        })
    }
}

fn validate_flag_id(flag_id: i32) -> Result<i32, String> {
    (flag_id >= 0)
        .then_some(flag_id)
        .ok_or_else(|| format!("Victory event flag ID must be non-negative, got {flag_id}"))
}

fn validate_victory(config: VictoryConfig) -> Result<VictoryCondition, String> {
    match (config.mode, config.boss_ids, config.boss_id) {
        (VictoryMode::Checklist, None, None) => Ok(VictoryCondition::Checklist),
        (VictoryMode::BossIds, Some(mut ids), None) if !ids.is_empty() => {
            for id in &mut ids {
                *id = validate_flag_id(*id)?;
            }
            ids.sort_unstable();
            ids.dedup();
            Ok(VictoryCondition::BossIds(ids))
        }
        (VictoryMode::OneBoss, None, Some(id)) => {
            Ok(VictoryCondition::OneBoss(validate_flag_id(id)?))
        }
        (VictoryMode::None, None, None) => Ok(VictoryCondition::None),
        (mode, _, _) => Err(format!(
            "Victory mode {mode:?} has missing or contradictory boss ID fields"
        )),
    }
}

fn nonempty(value: Option<String>, fallback: &str) -> String {
    value
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

fn modified_time(path: &Path) -> Option<SystemTime> {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
}

#[cfg(test)]
mod tests {
    use super::{RuntimeConfig, VictoryCondition};
    use crate::overlay::style::IgniteConfig;

    fn parse_runtime(source: &str) -> Result<RuntimeConfig, String> {
        let file: IgniteConfig = toml::from_str(source).map_err(|error| error.to_string())?;
        RuntimeConfig::try_from(file)
    }

    #[test]
    fn parses_all_victory_modes_and_defaults_to_none() {
        assert_eq!(parse_runtime("").unwrap().victory, VictoryCondition::None);
        assert_eq!(
            parse_runtime("[victory]\nmode = \"Checklist\"")
                .unwrap()
                .victory,
            VictoryCondition::Checklist
        );
        assert_eq!(
            parse_runtime("[victory]\nmode = \"BossIds\"\nboss_ids = [20, 10, 20]")
                .unwrap()
                .victory,
            VictoryCondition::BossIds(vec![10, 20])
        );
        assert_eq!(
            parse_runtime("[victory]\nmode = \"OneBoss\"\nboss_id = 19000800")
                .unwrap()
                .victory,
            VictoryCondition::OneBoss(19000800)
        );
        assert_eq!(
            parse_runtime("[victory]\nmode = \"None\"").unwrap().victory,
            VictoryCondition::None
        );
    }

    #[test]
    fn rejects_incomplete_contradictory_and_negative_victory_settings() {
        for source in [
            "[victory]\nmode = \"BossIds\"\nboss_ids = []",
            "[victory]\nmode = \"BossIds\"\nboss_ids = [1]\nboss_id = 2",
            "[victory]\nmode = \"OneBoss\"",
            "[victory]\nmode = \"OneBoss\"\nboss_id = -1",
            "[victory]\nmode = \"Checklist\"\nboss_id = 1",
            "[victory]\nmode = \"None\"\nboss_ids = [1]",
        ] {
            assert!(parse_runtime(source).is_err(), "accepted {source}");
        }
    }

    #[test]
    fn rejects_removed_freeze_on_boss_flag_setting() {
        assert!(parse_runtime("[timer]\nfreeze_on_boss_flag = 19000800").is_err());
    }
}
