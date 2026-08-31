use std::{
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use hudhook::ImguiRenderLoop;
use imgui::Ui;

use crate::{
    RENDERER_INITIALIZED, debug_log,
    ingest::{IngestStatus, SharedIngestStatus, create_status, start_reporter, status_line},
    overlay::{
        boss_panel::BossPanel,
        config::{ConfigManager, RuntimeConfig},
        data::{AppState, create_state},
        input::InputController,
        layout::{
            OverlayLayout, header_clicked, normalize_swap_chain_framebuffer_scale,
            render_centered_line, render_centered_text,
        },
        runtime::OverlayRuntime,
        style::{apply_common_config, apply_runtime_font_scale, apply_style_config},
        view_model::OverlayViewModel,
    },
    util::{debug::attach_console, introspection::get_dll_directory},
};

pub struct EROverlayUi {
    last_toggle_time: Instant,
    full_mode: bool,

    config: Option<Arc<RuntimeConfig>>,
    config_error: Option<String>,
    config_manager: ConfigManager,
    base_imgui_style: Option<imgui::Style>,

    input: InputController,

    state: Arc<RwLock<AppState>>,
    igt: Arc<RwLock<u32>>,
    boss_panel: BossPanel,
    runtime: OverlayRuntime,
    startup_started: Instant,
    first_frame_logged: bool,
    corrected_framebuffer_scale: Option<[f32; 2]>,
    ingest_status: SharedIngestStatus,
    ingest_stop: Arc<AtomicBool>,
}

fn append_ingest_text(
    model: &mut OverlayViewModel,
    show_ingest_tally: bool,
    status: &IngestStatus,
) -> Option<String> {
    if !show_ingest_tally {
        return None;
    }
    if let Some(line) = status_line(status) {
        model.lines.push(line);
    }
    status.last_error.clone()
}

impl EROverlayUi {
    pub fn new() -> Self {
        let startup_started = Instant::now();
        let dll_directory = get_dll_directory().unwrap_or_default();
        let config_path = dll_directory.join("overlay_config.toml");
        let (config_manager, config_error) = ConfigManager::new(config_path.clone());
        let config = config_manager.current();
        let console_requested = cfg!(debug_assertions)
            || config.as_ref().is_some_and(|config| config.common.console)
            || config_error.is_some();
        let console_ready = console_requested && attach_console();

        match &config_error {
            Some(error) => debug_log!("[ignite_overlay] ❌ Configuration unavailable: {error}"),
            None => debug_log!(
                "[ignite_overlay] ✅ Loaded configuration from '{}'",
                config_path.display()
            ),
        }
        debug_log!(
            "[ignite_overlay] Startup diagnostics: build={}, console_requested={}, console_ready={}, dll_directory='{}'",
            if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            },
            console_requested,
            console_ready,
            dll_directory.display()
        );

        let input = InputController::new(config.as_deref());
        let boss_panel = BossPanel::load(&dll_directory, config.as_deref());
        debug_log!("[ignite_overlay] UI state constructed successfully");
        Self {
            last_toggle_time: Instant::now(),
            full_mode: false,
            config,
            config_error,
            config_manager,
            base_imgui_style: None,
            input,
            boss_panel,
            state: create_state(),
            igt: Arc::new(RwLock::new(0)),
            runtime: OverlayRuntime::new(),
            startup_started,
            first_frame_logged: false,
            corrected_framebuffer_scale: None,
            ingest_status: create_status(),
            ingest_stop: Arc::new(AtomicBool::new(false)),
        }
    }

    fn render_closed(&mut self, ui: &Ui, model: &OverlayViewModel) {
        render_centered_text(ui, model.title, &model.lines);
        let line_count = model.lines.len() + usize::from(model.title.is_some());
        let total_h = ui.text_line_height_with_spacing() * line_count as f32 + 8.0;

        if header_clicked(ui, total_h) {
            let now = Instant::now();
            if now.duration_since(self.last_toggle_time) > Duration::from_millis(300) {
                self.full_mode = true;
                self.last_toggle_time = now;
                debug_log!("[ignite_overlay] Clicked compact overlay - expanding");
            }
        }
    }

    fn render_open(&mut self, ui: &Ui, model: &OverlayViewModel, ingest_error: Option<&str>) {
        if let Some(title) = model.title {
            render_centered_line(ui, title);
        }
        for line in &model.lines {
            ui.text(line);
        }
        if let Some(error) = ingest_error {
            let _color = ui.push_style_color(imgui::StyleColor::Text, [1.0, 0.6, 0.2, 1.0]);
            ui.text_wrapped(format!("[!] last report failed: {error}"));
        }

        let line_count = model.lines.len() + usize::from(model.title.is_some());
        let header_h = ui.text_line_height_with_spacing() * line_count as f32 + 8.0;
        if header_clicked(ui, header_h) {
            let now = Instant::now();
            if now.duration_since(self.last_toggle_time) > Duration::from_millis(300) {
                self.full_mode = false;
                self.last_toggle_time = now;
                debug_log!("[ignite_overlay] Clicked header - collapsing overlay");
            }
        }

        let avail = ui.content_region_avail();
        let child = ui
            .child_window("BossListRegion")
            .size(avail)
            .border(false)
            .begin();

        if let Some(child_token) = child {
            self.boss_panel.render(ui, &self.state);
            child_token.end();
        }
    }

    fn measure_closed_size(
        ui: &Ui,
        model: &OverlayViewModel,
        fixed_width: Option<f32>,
    ) -> (f32, f32) {
        let pad = unsafe { ui.style().window_padding };
        let max_w = model
            .title
            .into_iter()
            .chain(model.lines.iter().map(String::as_str))
            .map(|line| ui.calc_text_size(line)[0])
            .fold(0.0, f32::max);

        let line_count = model.lines.len() + usize::from(model.title.is_some());
        let total_h = pad[1] * 2.0 + ui.text_line_height_with_spacing() * line_count as f32;
        let measured_width = (pad[0] * 2.0 + max_w).ceil() + 4.0;
        let total_w = fixed_width.map(f32::ceil).unwrap_or(measured_width);

        (total_w, total_h.ceil())
    }

    fn hot_reload_ui_config(&mut self, imgui: &mut imgui::Context, now: Instant) {
        match self.config_manager.poll(now) {
            Ok(true) => {
                let Some(config) = self.config_manager.current() else {
                    return;
                };
                self.input.update_config(Some(config.as_ref()));
                if config.common.console {
                    attach_console();
                }

                if let Some(base_style) = self.base_imgui_style {
                    *imgui.style_mut() = base_style;
                }
                apply_style_config(imgui, config.as_ref());
                apply_runtime_font_scale(imgui, config.as_ref());
                self.config = Some(config);
                self.config_error = None;
                debug_log!(
                    "[ignite_overlay] Hot-reloaded overlay style, layout, text, timer, and input bindings"
                );
            }
            Ok(false) => {}
            Err(error) => {
                self.config_error = Some(error.clone());
                debug_log!(
                    "[ignite_overlay] UI config reload failed; keeping previous settings: {error}"
                );
            }
        }
    }
}

impl Default for EROverlayUi {
    fn default() -> Self {
        Self::new()
    }
}

impl ImguiRenderLoop for EROverlayUi {
    fn initialize(&mut self, imgui: &mut imgui::Context, _ctx: &mut dyn hudhook::RenderContext) {
        RENDERER_INITIALIZED.store(true, std::sync::atomic::Ordering::Release);
        debug_log!(
            "[ignite_overlay] ✅ DX12 renderer initialized after {:.2?}; initializing overlay resources...",
            self.startup_started.elapsed()
        );
        self.base_imgui_style = Some(*imgui.style());

        if let Some(config) = &self.config {
            apply_style_config(imgui, config);
            apply_common_config(imgui, config);
            apply_runtime_font_scale(imgui, config);
        }

        let flag_ids = self.boss_panel.flag_ids();
        debug_log!("[ignite_overlay] Loaded {} boss flags", flag_ids.len());
        self.ingest_stop.store(false, Ordering::Release);
        let observation_tx = start_reporter(
            self.config
                .as_ref()
                .and_then(|config| config.ingest.clone()),
            self.ingest_status.clone(),
            self.ingest_stop.clone(),
        );
        self.runtime.start(
            self.state.clone(),
            self.igt.clone(),
            flag_ids,
            self.config_manager.shared(),
            2_008_021,
            100,
            observation_tx,
        );
        debug_log!("[ignite_overlay] Game monitor started successfully.");
    }

    fn before_render(&mut self, imgui: &mut imgui::Context, _ctx: &mut dyn hudhook::RenderContext) {
        let now = Instant::now();
        self.hot_reload_ui_config(imgui, now);
        if self.input.before_render(imgui, now) {
            debug_log!("[ignite_overlay] Simulated mouse click");
        }
    }

    fn render(&mut self, ui: &mut imgui::Ui) {
        let corrected_scale = normalize_swap_chain_framebuffer_scale(ui);
        if corrected_scale != self.corrected_framebuffer_scale {
            if let Some(reported) = corrected_scale {
                debug_log!(
                    "[ignite_overlay] Corrected framebuffer scale from {:.2}x{:.2} to 1x1; display size already uses DXGI swap-chain pixels",
                    reported[0],
                    reported[1]
                );
            }
            self.corrected_framebuffer_scale = corrected_scale;
        }

        if self.input.toggle_requested(ui) {
            self.full_mode = !self.full_mode;
            debug_log!("[ignite_overlay] full_mode toggled -> {}", self.full_mode);
        }

        let io = ui.io();
        let display = [io.display_size[0], io.display_size[1]];
        let (model, ingest_error) = {
            let state = self.state.read().ok();
            let in_game_time = self.igt.read().map(|value| *value).unwrap_or(0);
            let timer = self
                .config
                .as_ref()
                .map(|config| config.timer)
                .unwrap_or_default();
            let template = self
                .config
                .as_ref()
                .map(|config| config.overlay.display_text.as_str());
            let mut model =
                OverlayViewModel::build(state.as_deref(), in_game_time, timer, template);
            let show_ingest_tally = self
                .config
                .as_ref()
                .is_some_and(|config| config.show_ingest_tally);
            let ingest_error = self
                .ingest_status
                .read()
                .ok()
                .and_then(|status| append_ingest_text(&mut model, show_ingest_tally, &status));
            (model, ingest_error)
        };

        let layout = OverlayLayout::from_config(self.config.as_deref());
        let ([x, y], [width, height]) = layout.expanded_rect(display);

        if self.full_mode {
            ui.window("Ignite HUD")
                .position([x, y], imgui::Condition::Always)
                .size([width, height], imgui::Condition::Always)
                .flags(
                    imgui::WindowFlags::NO_TITLE_BAR
                        | imgui::WindowFlags::NO_RESIZE
                        | imgui::WindowFlags::NO_MOVE,
                )
                .build(|| {
                    if let Some(error) = &self.config_error {
                        let _color =
                            ui.push_style_color(imgui::StyleColor::Text, [1.0, 0.2, 0.2, 1.0]);
                        ui.text("Failed to load config:");
                        ui.text_wrapped(error);
                        return;
                    }
                    self.render_open(ui, &model, ingest_error.as_deref());
                });

            if !self.first_frame_logged {
                debug_log!(
                    "[ignite_overlay] ✅ First overlay frame built: mode=expanded, display={:.0}x{:.0}, window_pos=({:.0}, {:.0}), window_size={:.0}x{:.0}",
                    display[0],
                    display[1],
                    x,
                    y,
                    width,
                    height
                );
                self.first_frame_logged = true;
            }
        } else {
            let fixed_width = self
                .config
                .as_ref()
                .and_then(|config| config.overlay.closed_width)
                .map(|width| width.min(display[0].max(1.0)));
            let requested_size = Self::measure_closed_size(ui, &model, fixed_width);
            let ([x, y], [width, height]) =
                layout.compact_rect(display, [requested_size.0, requested_size.1]);

            ui.window("Ignite HUD")
                .position([x, y], imgui::Condition::Always)
                .size([width, height], imgui::Condition::Always)
                .flags(
                    imgui::WindowFlags::NO_TITLE_BAR
                        | imgui::WindowFlags::NO_RESIZE
                        | imgui::WindowFlags::NO_MOVE,
                )
                .build(|| {
                    if let Some(error) = &self.config_error {
                        let _color =
                            ui.push_style_color(imgui::StyleColor::Text, [1.0, 0.2, 0.2, 1.0]);
                        ui.text("Failed to load config:");
                        ui.text_wrapped(error);
                        return;
                    }
                    self.render_closed(ui, &model);
                });

            if !self.first_frame_logged {
                debug_log!(
                    "[ignite_overlay] ✅ First overlay frame built: mode=compact, display={:.0}x{:.0}, window_pos=({:.0}, {:.0}), window_size={:.0}x{:.0}",
                    display[0],
                    display[1],
                    x,
                    y,
                    width,
                    height
                );
                self.first_frame_logged = true;
            }
        }
    }
}

impl Drop for EROverlayUi {
    fn drop(&mut self) {
        self.ingest_stop.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use crate::ingest::{IngestStatus, Tally};

    use super::{OverlayViewModel, append_ingest_text};

    fn model() -> OverlayViewModel {
        OverlayViewModel {
            title: None,
            lines: vec!["normal".to_string()],
        }
    }

    #[test]
    fn ingest_text_is_hidden_when_configured_off() {
        let mut model = model();
        let status = IngestStatus {
            eligible: true,
            tally: Some(Tally {
                hits: 8,
                misses: 4,
                shots: 12,
                accuracy: Some(67),
            }),
            warn: true,
            last_error: Some("server error".to_string()),
            kills_tracked: 2,
        };

        let expanded_error = append_ingest_text(&mut model, false, &status);

        assert_eq!(model.lines, ["normal"]);
        assert_eq!(expanded_error, None);
    }

    #[test]
    fn ingest_text_includes_compact_line_and_expanded_error() {
        let mut model = model();
        let status = IngestStatus {
            eligible: true,
            tally: Some(Tally {
                hits: 8,
                misses: 4,
                shots: 12,
                accuracy: Some(67),
            }),
            warn: true,
            last_error: Some("server error".to_string()),
            kills_tracked: 2,
        };

        let expanded_error = append_ingest_text(&mut model, true, &status);

        assert_eq!(
            model.lines,
            ["normal", "Hit 8   Miss 4   Total 12   Acc 67%   [!]"]
        );
        assert_eq!(expanded_error.as_deref(), Some("server error"));
    }
}
