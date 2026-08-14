use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::{Duration, Instant, SystemTime},
};

use crate::overlay::style::{IgniteConfig, TimerMode};

#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    pub common: CommonConfig,
    pub input: InputConfig,
    pub style: StyleConfig,
    pub boss: BossConfig,
    pub overlay: OverlayConfig,
    pub timer: TimerSettings,
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
    pub freeze_on_boss_flag: Option<i32>,
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
    Ok(RuntimeConfig::from(file))
}

impl From<IgniteConfig> for RuntimeConfig {
    fn from(file: IgniteConfig) -> Self {
        let common = file.common.unwrap_or_default();
        let input = file.input.unwrap_or_default();
        let style = file.style.unwrap_or_default();
        let boss = file.boss.unwrap_or_default();
        let overlay = file.overlay.unwrap_or_default();
        let timer = file.timer;
        Self {
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
                freeze_on_boss_flag: timer.and_then(|timer| timer.freeze_on_boss_flag),
            },
        }
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
    use super::RuntimeConfig;
    use crate::overlay::style::{IgniteConfig, TimerMode};

    #[test]
    fn preserves_legacy_timer_and_overlay_settings() {
        let source = r#"
            [common]
            font_size = 42

            [timer]
            mode = "PrepTimer"
            prep_minutes = 5
            timer_minutes = 90
            freeze_on_boss_flag = 123456

            [overlay]
            display_text = "IGT: {igt}"
        "#;
        let file: IgniteConfig = toml::from_str(source).unwrap();
        let config = RuntimeConfig::from(file);

        assert_eq!(config.common.font_size, 42.0);
        assert_eq!(config.timer.mode, TimerMode::PrepTimer);
        assert_eq!(config.timer.prep_minutes, 5);
        assert_eq!(config.timer.timer_minutes, 90);
        assert_eq!(config.timer.freeze_on_boss_flag, Some(123456));
        assert_eq!(config.overlay.display_text, "IGT: {igt}");
    }

    #[test]
    fn fills_defaults_for_existing_minimal_configs() {
        let file: IgniteConfig = toml::from_str("").unwrap();
        let config = RuntimeConfig::from(file);

        assert_eq!(config.timer.mode, TimerMode::Regular);
        assert_eq!(config.timer.freeze_on_boss_flag, None);
        assert_eq!(config.common.font_scale, 1.0);
        assert_eq!(config.boss.data_file, "bosses.json");
    }

    #[test]
    fn timer_section_can_only_set_a_freeze_flag() {
        let file: IgniteConfig = toml::from_str(
            r#"
                [timer]
                freeze_on_boss_flag = 123456
            "#,
        )
        .unwrap();
        let config = RuntimeConfig::from(file);

        assert_eq!(config.timer.mode, TimerMode::Regular);
        assert_eq!(config.timer.prep_minutes, 0);
        assert_eq!(config.timer.timer_minutes, 0);
        assert_eq!(config.timer.freeze_on_boss_flag, Some(123456));
    }
}
