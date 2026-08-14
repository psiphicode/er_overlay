use crate::debug_log;
use crate::overlay::config::RuntimeConfig;
use crate::overlay::embedded_font::EMBEDDED_FONT;
use crate::util::introspection::get_dll_directory;
use imgui::{Context, FontConfig, FontSource, Key, StyleColor};
use serde::Deserialize;
use std::fs;

pub const DEFAULT_PANEL_DIM: [f32; 2] = [0.17, 0.92];
pub const DEFAULT_PANEL_POS: [f32; 2] = [-10.0, 10.0];
pub const DEFAULT_DISPLAY_TEXT: &str = "Current: {kills}/{total}$nIGT: {igt}$nDeaths: {deaths}";

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Common {
    pub console: Option<bool>,
    pub font: Option<String>,
    pub font_size: Option<f32>,
    pub font_scale: Option<f32>,
    pub charset: Option<String>,
    pub language: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Input {
    pub unload: Option<String>,
    pub toggle_full_mode: Option<String>,
    pub click_action: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Style {
    pub text_color: Option<[u8; 4]>,
    pub check_mark_color: Option<[u8; 4]>,
    pub bg_color: Option<[u8; 4]>,
    pub border_color: Option<[u8; 4]>,
    pub button_color: Option<[u8; 4]>,
    pub button_hover_color: Option<[u8; 4]>,
    pub button_press_color: Option<[u8; 4]>,
    pub node_color: Option<[u8; 4]>,
    pub node_hover_color: Option<[u8; 4]>,
    pub node_press_color: Option<[u8; 4]>,
    pub scroll_bg_color: Option<[u8; 4]>,
    pub scroll_color: Option<[u8; 4]>,
    pub scroll_hover_color: Option<[u8; 4]>,
    pub scroll_press_color: Option<[u8; 4]>,
    pub border_width: Option<f32>,
    pub rounding: Option<f32>,
    pub panel_pos: Option<[f32; 2]>,
    pub panel_dim: Option<[f32; 2]>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Boss {
    pub data_file: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Overlay {
    pub display_text: Option<String>,
    pub closed_width: Option<f32>,
}

pub fn apply_runtime_font_scale(imgui: &mut Context, cfg: &RuntimeConfig) {
    imgui.io_mut().font_global_scale = cfg.common.font_scale;
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub enum TimerMode {
    #[default]
    Regular,
    Timer,
    Prep,
    PrepTimer,
}

#[derive(Clone, Debug, Deserialize)]
pub struct TimerConfig {
    #[serde(default)]
    pub mode: TimerMode,
    pub prep_minutes: Option<u32>,
    pub timer_minutes: Option<u32>,
    pub freeze_on_boss_flag: Option<i32>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct IgniteConfig {
    pub common: Option<Common>,
    pub input: Option<Input>,
    pub style: Option<Style>,
    pub boss: Option<Boss>,
    pub overlay: Option<Overlay>,
    pub timer: Option<TimerConfig>,
}

pub fn apply_style_config(imgui: &mut Context, cfg: &RuntimeConfig) {
    let style_cfg = &cfg.style;
    let style = imgui.style_mut();

    macro_rules! color {
        ($field:ident, $col:expr) => {
            style.colors[$col as usize] = rgba_u8_to_f32(style_cfg.$field);
        };
    }

    // --- text and basics ---
    color!(text_color, StyleColor::Text);
    color!(check_mark_color, StyleColor::CheckMark);
    color!(bg_color, StyleColor::WindowBg);
    color!(border_color, StyleColor::Border);

    // --- buttons ---
    color!(button_color, StyleColor::Button);
    color!(button_hover_color, StyleColor::ButtonHovered);
    color!(button_press_color, StyleColor::ButtonActive);

    // --- nodes (we’ll map to tree headers for best visual equivalence) ---
    color!(node_color, StyleColor::Header);
    color!(node_hover_color, StyleColor::HeaderHovered);
    color!(node_press_color, StyleColor::HeaderActive);

    // --- scrollbars ---
    color!(scroll_bg_color, StyleColor::ScrollbarBg);
    color!(scroll_color, StyleColor::ScrollbarGrab);
    color!(scroll_hover_color, StyleColor::ScrollbarGrabHovered);
    color!(scroll_press_color, StyleColor::ScrollbarGrabActive);

    // --- rounding & borders ---
    style.window_rounding = style_cfg.rounding;
    style.frame_rounding = style_cfg.rounding;
    style.scrollbar_rounding = style_cfg.rounding;
    style.window_border_size = style_cfg.border_width;
    style.frame_border_size = style_cfg.border_width;
}

fn rgba_u8_to_f32(c: [u8; 4]) -> [f32; 4] {
    [
        c[0] as f32 / 255.0,
        c[1] as f32 / 255.0,
        c[2] as f32 / 255.0,
        c[3] as f32 / 255.0,
    ]
}

/// Apply common config (font, size, charset, etc.).
///
/// The render backend owns font-atlas creation and upload after the render loop
/// finishes initializing. Uploading it here would make the DX12 backend submit
/// the same atlas twice before the first frame.
pub fn apply_common_config(imgui: &mut Context, cfg: &RuntimeConfig) {
    let font_size = cfg.common.font_size;

    let fonts = imgui.fonts();
    fonts.clear();

    let mut font_loaded = false;
    let mut font_used = String::new();

    // --- 1️⃣ Charset-based font loading ---
    {
        let charset = cfg.common.charset.to_lowercase();
        let font_path_opt = match charset.as_str() {
            "jajp" => Some("C:\\Windows\\Fonts\\meiryo.ttc"), // Japanese
            "kokr" => Some("C:\\Windows\\Fonts\\malgun.ttf"), // Korean
            "zhcn" => Some("C:\\Windows\\Fonts\\msyh.ttc"),   // Simplified Chinese
            "zhtw" => Some("C:\\Windows\\Fonts\\mingliu.ttc"), // Traditional Chinese
            "thth" => Some("C:\\Windows\\Fonts\\tahoma.ttf"), // Thai fallback
            "ruru" => Some("C:\\Windows\\Fonts\\arial.ttf"),  // Cyrillic
            "plpl" => Some("C:\\Windows\\Fonts\\arial.ttf"),  // Latin extended
            _ => None,
        };

        if let Some(path) = font_path_opt {
            if let Ok(bytes) = fs::read(path) {
                let leaked = Box::leak(bytes.into_boxed_slice());
                fonts.add_font(&[FontSource::TtfData {
                    data: leaked,
                    size_pixels: font_size,
                    config: Some(FontConfig::default()),
                }]);
                font_loaded = true;
                font_used = format!("system charset font: {}", charset);
                debug_log!("[ignite_overlay] ✅ Loaded charset font '{}'", charset);
            } else {
                debug_log!(
                    "[ignite_overlay] ⚠ Charset font '{}' not found at {}",
                    charset,
                    path
                );
            }
        }
    }

    // --- 2️⃣ User-defined font path ---
    if !font_loaded && let Some(path) = cfg.common.font.as_ref() {
        let dll_dir = get_dll_directory().expect("Failed to get DLL directory");
        let font_path = dll_dir.join(path);
        if let Ok(bytes) = fs::read(font_path) {
            let leaked = Box::leak(bytes.into_boxed_slice());
            fonts.add_font(&[FontSource::TtfData {
                data: leaked,
                size_pixels: font_size,
                config: Some(FontConfig::default()),
            }]);
            font_loaded = true;
            font_used = path.to_string();
            debug_log!("[ignite_overlay] ✅ Loaded user font '{}'", path);
        } else {
            debug_log!("[ignite_overlay] ⚠ Could not read font '{}'", path);
        }
    }

    // --- 3️⃣ Embedded fallback font ---
    if !font_loaded {
        fonts.add_font(&[FontSource::TtfData {
            data: EMBEDDED_FONT,
            size_pixels: font_size,
            config: Some(FontConfig::default()),
        }]);
        font_used = "EmbeddedFont".to_string();
        font_loaded = true;
        debug_log!("[ignite_overlay] ✅ Loaded embedded font");
    }

    // --- 4️⃣ Final fallback: ImGui default font ---
    if !font_loaded {
        fonts.add_font(&[FontSource::DefaultFontData {
            config: Some(FontConfig {
                size_pixels: font_size,
                ..Default::default()
            }),
        }]);
        font_used = "ImGui Default".to_string();
        debug_log!("[ignite_overlay] ⚠ Fallback to ImGui default font");
    }

    debug_log!(
        "[ignite_overlay] 🧩 Font configured: '{}' at {:.1}px; waiting for hudhook atlas upload",
        font_used,
        font_size
    );
}

pub fn key_from_name(name: &str) -> Option<Key> {
    use Key::*;
    match name.trim().to_uppercase().as_str() {
        // --- Modifier keys ---
        "LCTRL" | "CTRL" => Some(LeftCtrl),
        "RCTRL" => Some(RightCtrl),
        "LSHIFT" | "SHIFT" => Some(LeftShift),
        "RSHIFT" => Some(RightShift),
        "LALT" | "ALT" => Some(LeftAlt),
        "RALT" => Some(RightAlt),
        "LWIN" | "WIN" => Some(LeftSuper),
        "RWIN" => Some(RightSuper),

        // --- Mouse buttons ---
        "LBUTTON" => Some(MouseLeft),
        "RBUTTON" => Some(MouseRight),
        "MBUTTON" => Some(MouseMiddle),
        "XBUTTON1" => Some(MouseX1),
        "XBUTTON2" => Some(MouseX2),

        // --- Control & navigation ---
        "BACK" | "BACKSPACE" => Some(Backspace),
        "TAB" => Some(Tab),
        "RETURN" | "ENTER" => Some(Enter),
        "ESCAPE" | "ESC" => Some(Escape),
        "SPACE" => Some(Space),
        "DELETE" => Some(Delete),
        "INSERT" => Some(Insert),
        "HOME" => Some(Home),
        "END" => Some(End),
        "PRIOR" | "PAGEUP" => Some(PageUp),
        "NEXT" | "PAGEDOWN" => Some(PageDown),
        "LEFT" => Some(LeftArrow),
        "RIGHT" => Some(RightArrow),
        "UP" => Some(UpArrow),
        "DOWN" => Some(DownArrow),

        // --- Function keys ---
        "F1" => Some(F1),
        "F2" => Some(F2),
        "F3" => Some(F3),
        "F4" => Some(F4),
        "F5" => Some(F5),
        "F6" => Some(F6),
        "F7" => Some(F7),
        "F8" => Some(F8),
        "F9" => Some(F9),
        "F10" => Some(F10),
        "F11" => Some(F11),
        "F12" => Some(F12),

        // --- Alphabet keys ---
        "A" => Some(A),
        "B" => Some(B),
        "C" => Some(C),
        "D" => Some(D),
        "E" => Some(E),
        "F" => Some(F),
        "G" => Some(G),
        "H" => Some(H),
        "I" => Some(I),
        "J" => Some(J),
        "K" => Some(K),
        "L" => Some(L),
        "M" => Some(M),
        "N" => Some(N),
        "O" => Some(O),
        "P" => Some(P),
        "Q" => Some(Q),
        "R" => Some(R),
        "S" => Some(S),
        "T" => Some(T),
        "U" => Some(U),
        "V" => Some(V),
        "W" => Some(W),
        "X" => Some(X),
        "Y" => Some(Y),
        "Z" => Some(Z),

        // --- Number row ---
        "0" => Some(Alpha0),
        "1" => Some(Alpha1),
        "2" => Some(Alpha2),
        "3" => Some(Alpha3),
        "4" => Some(Alpha4),
        "5" => Some(Alpha5),
        "6" => Some(Alpha6),
        "7" => Some(Alpha7),
        "8" => Some(Alpha8),
        "9" => Some(Alpha9),

        // --- Numpad ---
        "NUMPAD0" | "NUM0" => Some(Keypad0),
        "NUMPAD1" | "NUM1" => Some(Keypad1),
        "NUMPAD2" | "NUM2" => Some(Keypad2),
        "NUMPAD3" | "NUM3" => Some(Keypad3),
        "NUMPAD4" | "NUM4" => Some(Keypad4),
        "NUMPAD5" | "NUM5" => Some(Keypad5),
        "NUMPAD6" | "NUM6" => Some(Keypad6),
        "NUMPAD7" | "NUM7" => Some(Keypad7),
        "NUMPAD8" | "NUM8" => Some(Keypad8),
        "NUMPAD9" | "NUM9" => Some(Keypad9),
        "DIVIDE" | "/" => Some(KeypadDivide),
        "MULTIPLY" | "*" => Some(KeypadMultiply),
        "-" | "MINUS" | "HYPHEN" => Some(Key::Minus),
        "SUBTRACT" | "NUMPADSUBTRACT" => Some(Key::KeypadSubtract),
        "ADD" | "+" => Some(KeypadAdd),
        "EQUAL" | "=" => Some(Equal),
        "DECIMAL" | "NUMPADPERIOD" => Some(KeypadDecimal),

        // --- Lock keys ---
        "CAPSLOCK" | "CAPITAL" => Some(CapsLock),
        "NUMLOCK" => Some(NumLock),
        "SCROLL" | "SCROLLLOCK" => Some(ScrollLock),

        // --- Symbols ---
        "," => Some(Comma),
        "." | "PERIOD" => Some(Period),
        ";" => Some(Semicolon),
        "'" => Some(Apostrophe),
        "[" => Some(LeftBracket),
        "]" => Some(RightBracket),
        "\\" => Some(Backslash),
        "~" => Some(GraveAccent),

        // --- Misc / System ---
        "PRINT" | "PRINTSCREEN" | "SNAPSHOT" => Some(PrintScreen),
        "PAUSE" => Some(Pause),

        _ => None,
    }
}

/// Parse a single or multi-key combination like "CTRL+SHIFT+F1".
/// Returns a vector of `Key` values (empty if none matched).
pub fn parse_key_combo(combo: &str) -> Vec<Key> {
    combo
        .split('+')
        .filter_map(|part| key_from_name(part.trim()))
        .collect()
}
