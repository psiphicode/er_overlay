use std::collections::HashMap;

use crate::{
    overlay::{
        config::TimerSettings,
        data::AppState,
        game_monitor::is_great_rune_flag,
        style::{DEFAULT_DISPLAY_TEXT, TimerMode},
    },
    util::text_formatter::format_display_text,
};

const GOAL_COMPLETE_TITLE: &str = "GOAL COMPLETE";

pub struct OverlayViewModel {
    pub title: Option<&'static str>,
    pub lines: Vec<String>,
}

impl OverlayViewModel {
    pub fn build(
        state: Option<&AppState>,
        in_game_time_ms: u32,
        timer: TimerSettings,
        template: Option<&str>,
    ) -> Self {
        let (kills, total, deaths, shards, runes) = state
            .map(|state| {
                (
                    state
                        .event_flags
                        .iter()
                        .filter(|(flag_id, defeated)| !is_great_rune_flag(**flag_id) && **defeated)
                        .count(),
                    state
                        .event_flags
                        .keys()
                        .filter(|flag_id| !is_great_rune_flag(**flag_id))
                        .count(),
                    state.death_count,
                    state.key_item_quantity,
                    state.great_runes,
                )
            })
            .unwrap_or((0, 0, 0, 0, 0));
        let variables = HashMap::from([
            ("kills", kills.to_string()),
            ("total", total.to_string()),
            ("deaths", deaths.to_string()),
            ("igt", format_timer(in_game_time_ms, timer)),
            ("shards", shards.to_string()),
            ("runes", runes.to_string()),
        ]);
        let title = state
            .is_some_and(|state| state.goal_complete)
            .then_some(GOAL_COMPLETE_TITLE);
        Self {
            title,
            lines: format_display_text(template.unwrap_or(DEFAULT_DISPLAY_TEXT), &variables),
        }
    }
}

fn format_timer(in_game_time_ms: u32, timer: TimerSettings) -> String {
    let raw_ms = i64::from(in_game_time_ms);
    let prep_ms = i64::from(timer.prep_minutes) * 60_000;
    let timer_target_ms = i64::from(timer.timer_minutes) * 60_000;
    let display_ms = match timer.mode {
        TimerMode::Regular => raw_ms,
        TimerMode::Timer => timer_target_ms - raw_ms,
        TimerMode::Prep => raw_ms - prep_ms,
        TimerMode::PrepTimer if raw_ms < prep_ms => raw_ms - prep_ms,
        TimerMode::PrepTimer => timer_target_ms - (raw_ms - prep_ms),
    };
    format_duration(display_ms)
}

fn format_duration(milliseconds: i64) -> String {
    let total_seconds = milliseconds / 1_000;
    let sign = if total_seconds < 0 { "-" } else { "" };
    let total_seconds = total_seconds.unsigned_abs();
    if total_seconds > 86_400 {
        let (days, remainder) = (total_seconds / 86_400, total_seconds % 86_400);
        let (hours, remainder) = (remainder / 3_600, remainder % 3_600);
        format!(
            "{sign}{days:02}:{hours:02}:{:02}:{:02}",
            remainder / 60,
            remainder % 60
        )
    } else {
        let (hours, remainder) = (total_seconds / 3_600, total_seconds % 3_600);
        format!(
            "{sign}{hours:02}:{:02}:{:02}",
            remainder / 60,
            remainder % 60
        )
    }
}

#[cfg(test)]
mod tests {
    use crate::overlay::{config::TimerSettings, data::AppState, style::TimerMode};

    use super::{OverlayViewModel, format_duration, format_timer};

    #[test]
    fn formats_short_multi_day_and_negative_times() {
        assert_eq!(format_duration(3_661_000), "01:01:01");
        assert_eq!(format_duration(90_061_000), "01:01:01:01");
        assert_eq!(format_duration(-61_000), "-00:01:01");
    }

    #[test]
    fn preserves_all_legacy_timer_modes() {
        let timer = |mode, prep_minutes, timer_minutes| TimerSettings {
            mode,
            prep_minutes,
            timer_minutes,
        };
        assert_eq!(
            format_timer(60_000, timer(TimerMode::Regular, 0, 0)),
            "00:01:00"
        );
        assert_eq!(
            format_timer(60_000, timer(TimerMode::Timer, 0, 10)),
            "00:09:00"
        );
        assert_eq!(
            format_timer(60_000, timer(TimerMode::Prep, 2, 0)),
            "-00:01:00"
        );
        assert_eq!(
            format_timer(60_000, timer(TimerMode::PrepTimer, 2, 10)),
            "-00:01:00"
        );
        assert_eq!(
            format_timer(180_000, timer(TimerMode::PrepTimer, 2, 10)),
            "00:09:00"
        );
    }

    #[test]
    fn builds_template_once_from_snapshot() {
        let model = OverlayViewModel::build(
            None,
            0,
            TimerSettings::default(),
            Some("Bosses: {kills}/{total}$n{igt}"),
        );
        assert_eq!(model.lines, [" Bosses: 0/0 ", " 00:00:00 "]);
    }

    #[test]
    fn excludes_great_rune_flags_from_boss_totals() {
        let mut state = AppState::default();
        state.event_flags.extend([(1000, true), (1001, false)]);
        state
            .event_flags
            .extend((181..=187).map(|flag_id| (flag_id, true)));
        state.great_runes = 7;

        let model = OverlayViewModel::build(
            Some(&state),
            0,
            TimerSettings::default(),
            Some("{kills}/{total} runes={runes}"),
        );

        assert_eq!(model.lines, [" 1/2 runes=7 "]);
    }

    #[test]
    fn exposes_goal_title_only_after_completion() {
        let incomplete = OverlayViewModel::build(
            Some(&AppState::default()),
            0,
            TimerSettings::default(),
            Some("IGT: {igt}"),
        );
        assert_eq!(incomplete.title, None);

        let complete = AppState {
            goal_complete: true,
            ..Default::default()
        };
        let complete = OverlayViewModel::build(
            Some(&complete),
            0,
            TimerSettings::default(),
            Some("IGT: {igt}"),
        );
        assert_eq!(complete.title, Some("GOAL COMPLETE"));
        assert_eq!(complete.lines, [" IGT: 00:00:00 "]);
    }
}
