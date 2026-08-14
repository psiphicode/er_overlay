use std::collections::HashMap;

/// Expands a display-text template into multiple lines of formatted output.
/// Supports:
/// - `{var}` placeholders from `vars`
/// - `$n` for newlines
pub fn format_display_text(template: &str, vars: &HashMap<&str, String>) -> Vec<String> {
    let mut current = String::new();
    let mut lines = Vec::new();
    let chars: Vec<char> = template.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '{' {
            // handle variable substitution
            if let Some(end) = chars[i + 1..].iter().position(|&c| c == '}') {
                let key: String = chars[i + 1..i + 1 + end].iter().collect();
                if let Some(value) = vars.get(key.as_str()) {
                    current.push_str(value);
                } else {
                    current.push_str(&format!("{{{}}}", key)); // preserve unknown placeholders
                }
                i += end + 2;
                continue;
            }
        } else if chars[i] == '$' && i + 1 < chars.len() && chars[i + 1] == 'n' {
            // newline escape → push current line, start new one
            lines.push(format!(" {} ", current));
            current = String::new();
            i += 2;
            continue;
        }
        current.push(chars[i]);
        i += 1;
    }

    if !current.is_empty() {
        lines.push(format!(" {} ", current));
    }

    lines
}
