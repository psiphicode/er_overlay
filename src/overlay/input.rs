use std::time::{Duration, Instant};

use imgui::{Context, Key, MouseButton, Ui};

use crate::overlay::{config::RuntimeConfig, style::parse_key_combo};

pub struct InputController {
    toggle_full_mode: Option<Vec<Key>>,
    click_action: Option<Vec<Key>>,
    last_click_action: Instant,
}

impl InputController {
    pub fn new(config: Option<&RuntimeConfig>) -> Self {
        let mut controller = Self {
            toggle_full_mode: None,
            click_action: None,
            last_click_action: Instant::now(),
        };
        controller.update_config(config);
        controller
    }

    pub fn update_config(&mut self, config: Option<&RuntimeConfig>) {
        self.toggle_full_mode = config
            .and_then(|config| config.input.toggle_full_mode.as_deref())
            .map(parse_key_combo);
        self.click_action = config
            .and_then(|config| config.input.click_action.as_deref())
            .map(parse_key_combo);
    }

    pub fn before_render(&mut self, imgui: &mut Context, now: Instant) -> bool {
        let pressed = self
            .click_action
            .as_ref()
            .is_some_and(|keys| keys.iter().all(|&key| imgui.io().keys_down[key as usize]));
        if !pressed || now.duration_since(self.last_click_action) <= Duration::from_millis(200) {
            return false;
        }
        let io = imgui.io_mut();
        io.add_mouse_button_event(MouseButton::Left, true);
        io.add_mouse_button_event(MouseButton::Left, false);
        self.last_click_action = now;
        true
    }

    pub fn toggle_requested(&self, ui: &Ui) -> bool {
        self.toggle_full_mode
            .as_ref()
            .is_some_and(|keys| keys.iter().all(|&key| ui.is_key_pressed(key)))
    }
}
