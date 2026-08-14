use imgui::Ui;

use crate::overlay::config::RuntimeConfig;

const MIN_WINDOW_EXTENT: f32 = 1.0;
const FALLBACK_PANEL_POS: [f32; 2] = [-10.0, 10.0];
const FALLBACK_PANEL_DIM: [f32; 2] = [0.15, 0.90];

pub struct OverlayLayout {
    panel_pos: [f32; 2],
    panel_dim: [f32; 2],
}

impl OverlayLayout {
    pub fn from_config(config: Option<&RuntimeConfig>) -> Self {
        Self {
            panel_pos: config
                .map(|config| config.style.panel_pos)
                .unwrap_or(FALLBACK_PANEL_POS),
            panel_dim: config
                .map(|config| config.style.panel_dim)
                .unwrap_or(FALLBACK_PANEL_DIM),
        }
    }

    pub fn expanded_rect(&self, display: [f32; 2]) -> ([f32; 2], [f32; 2]) {
        let display = valid_display_size(display);
        let size = [
            fit_extent(
                display[0] * valid_ratio(self.panel_dim[0], FALLBACK_PANEL_DIM[0]),
                display[0],
            ),
            fit_extent(
                display[1] * valid_ratio(self.panel_dim[1], FALLBACK_PANEL_DIM[1]),
                display[1],
            ),
        ];
        (self.anchored_position(display, size), size)
    }

    pub fn compact_rect(
        &self,
        display: [f32; 2],
        requested_size: [f32; 2],
    ) -> ([f32; 2], [f32; 2]) {
        let display = valid_display_size(display);
        let size = [
            fit_extent(requested_size[0], display[0]),
            fit_extent(requested_size[1], display[1]),
        ];
        (self.anchored_position(display, size), size)
    }

    fn anchored_position(&self, display: [f32; 2], size: [f32; 2]) -> [f32; 2] {
        [
            anchored_axis(self.panel_pos[0], display[0], size[0]),
            anchored_axis(self.panel_pos[1], display[1], size[1]),
        ]
    }
}

fn valid_display_size(display: [f32; 2]) -> [f32; 2] {
    display.map(|extent| {
        if extent.is_finite() {
            extent.max(MIN_WINDOW_EXTENT)
        } else {
            MIN_WINDOW_EXTENT
        }
    })
}

fn valid_ratio(ratio: f32, fallback: f32) -> f32 {
    if ratio.is_finite() {
        ratio.clamp(0.05, 1.0)
    } else {
        fallback
    }
}

fn fit_extent(requested: f32, display: f32) -> f32 {
    if requested.is_finite() {
        requested.clamp(MIN_WINDOW_EXTENT, display)
    } else {
        display
    }
}

fn anchored_axis(offset: f32, display: f32, size: f32) -> f32 {
    let farthest_visible_position = (display - size).max(0.0);
    let requested = if !offset.is_finite() {
        0.0
    } else if offset < 0.0 {
        farthest_visible_position + offset
    } else {
        offset
    };
    requested.clamp(0.0, farthest_visible_position)
}

/// hudhook obtains `display_size` directly from the DXGI swap chain, so it is
/// already expressed in physical framebuffer pixels. hudhook 0.9.2 also sets
/// `display_framebuffer_scale` from the window DPI; leaving both values applied
/// makes its DX12 viewport larger than the actual render target on high-DPI
/// displays. Keep ImGui and DX12 in the same swap-chain pixel coordinate space.
///
/// This runs during `ImguiRenderLoop::render` because hudhook refreshes the DPI
/// scale after `before_render`. An active `Ui` guarantees that ImGui's current
/// context and IO pointer are valid for the duration of this update.
pub fn normalize_swap_chain_framebuffer_scale(_ui: &Ui) -> Option<[f32; 2]> {
    // SAFETY: ImGui owns one IO object in the active context represented by
    // `_ui`. No IO reference is retained across this targeted backend-field
    // update, and the value is set before ImGui produces the frame's DrawData.
    let io = unsafe { &mut *imgui::sys::igGetIO() };
    let reported = [io.DisplayFramebufferScale.x, io.DisplayFramebufferScale.y];
    if reported == [1.0, 1.0] {
        return None;
    }

    io.DisplayFramebufferScale = imgui::sys::ImVec2 { x: 1.0, y: 1.0 };
    Some(reported)
}

pub fn render_centered_text(ui: &Ui, lines: &[String]) {
    let total_height = ui.text_line_height_with_spacing() * lines.len() as f32;
    let offset = (ui.content_region_avail()[1] - total_height) * 0.5;
    if offset > 0.0 {
        let mut position = ui.cursor_pos();
        position[1] += offset;
        ui.set_cursor_pos(position);
    }
    for line in lines {
        ui.text(line);
    }
}

pub fn header_clicked(ui: &Ui, height: f32) -> bool {
    if !ui.io().mouse_down[0] {
        return false;
    }
    let mouse = ui.io().mouse_pos;
    let position = ui.window_pos();
    let size = ui.window_size();
    mouse[0] >= position[0]
        && mouse[0] <= position[0] + size[0]
        && mouse[1] >= position[1]
        && mouse[1] <= position[1] + height
}

#[cfg(test)]
mod tests {
    use imgui::Context;

    use super::{OverlayLayout, normalize_swap_chain_framebuffer_scale};

    fn assert_visible(position: [f32; 2], size: [f32; 2], display: [f32; 2]) {
        for axis in 0..2 {
            assert!(position[axis].is_finite());
            assert!(size[axis].is_finite());
            assert!(position[axis] >= 0.0);
            assert!(size[axis] >= 1.0);
            assert!(position[axis] + size[axis] <= display[axis]);
        }
    }

    #[test]
    fn negative_offsets_anchor_to_the_far_edges() {
        let layout = OverlayLayout {
            panel_pos: [-10.0, -20.0],
            panel_dim: [0.2, 0.5],
        };
        let (position, size) = layout.expanded_rect([1000.0, 800.0]);
        assert_eq!(size, [200.0, 400.0]);
        assert_eq!(position, [790.0, 380.0]);
    }

    #[test]
    fn expanded_panel_stays_visible_at_common_and_extreme_resolutions() {
        let layout = OverlayLayout {
            panel_pos: [-10.0, 10.0],
            panel_dim: [0.30, 0.92],
        };
        let displays = [
            [320.0, 200.0],
            [640.0, 480.0],
            [1280.0, 720.0],
            [1920.0, 1080.0],
            [2560.0, 1440.0],
            [3440.0, 1440.0],
            [3840.0, 2160.0],
            [5120.0, 1440.0],
            [7680.0, 4320.0],
            [1080.0, 1920.0],
        ];

        for display in displays {
            let (position, size) = layout.expanded_rect(display);
            assert_visible(position, size, display);
        }
    }

    #[test]
    fn configured_positions_cannot_move_windows_off_screen() {
        let layout = OverlayLayout {
            panel_pos: [50_000.0, -50_000.0],
            panel_dim: [0.30, 0.92],
        };
        let display = [3840.0, 2160.0];
        let (position, size) = layout.expanded_rect(display);

        assert_visible(position, size, display);
        assert_eq!(position, [display[0] - size[0], 0.0]);
    }

    #[test]
    fn compact_content_is_fitted_to_the_viewport() {
        let layout = OverlayLayout {
            panel_pos: [-10.0, 10.0],
            panel_dim: [0.30, 0.92],
        };
        let display = [640.0, 480.0];
        let (position, size) = layout.compact_rect(display, [1200.0, 900.0]);

        assert_eq!(position, [0.0, 0.0]);
        assert_eq!(size, display);
        assert_visible(position, size, display);
    }

    #[test]
    fn high_dpi_scale_is_removed_from_swap_chain_pixel_coordinates() {
        let mut imgui = Context::create();
        imgui.set_ini_filename(None);
        imgui.io_mut().display_size = [3840.0, 2160.0];
        imgui.io_mut().display_framebuffer_scale = [2.0, 2.0];
        imgui.fonts().build_rgba32_texture();

        let ui = imgui.frame();
        assert_eq!(normalize_swap_chain_framebuffer_scale(ui), Some([2.0, 2.0]));
        assert_eq!(ui.io().display_framebuffer_scale, [1.0, 1.0]);
        assert_eq!(normalize_swap_chain_framebuffer_scale(ui), None);

        let draw_data = imgui.render();
        assert_eq!(draw_data.framebuffer_scale, [1.0, 1.0]);
    }
}
