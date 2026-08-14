use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use crate::{
    debug_log,
    er::{events::EventFlagCache, gamedata::read_game_data, inventory::get_key_item_quantity},
    overlay::{config::SharedConfig, data::SharedState},
};

const GREAT_RUNE_FLAGS: [i32; 7] = [181, 182, 183, 184, 185, 186, 187];
const ERROR_LOG_INTERVAL: Duration = Duration::from_secs(5);

pub(crate) fn is_great_rune_flag(flag_id: i32) -> bool {
    GREAT_RUNE_FLAGS.contains(&flag_id)
}

#[derive(Debug, Default)]
struct IgtFreezeLatch {
    flag_id: Option<i32>,
    frozen: bool,
}

impl IgtFreezeLatch {
    fn new(flag_id: Option<i32>) -> Self {
        Self {
            flag_id,
            frozen: false,
        }
    }

    fn reconfigure(&mut self, flag_id: Option<i32>) -> bool {
        if self.flag_id == flag_id {
            return false;
        }
        self.flag_id = flag_id;
        self.frozen = false;
        true
    }

    fn observe(&mut self, flag_id: i32, active: bool) -> bool {
        if !self.frozen && active && self.flag_id == Some(flag_id) {
            self.frozen = true;
            return true;
        }
        false
    }
}

fn configured_freeze_flag(config: &SharedConfig) -> Option<i32> {
    config
        .read()
        .ok()
        .and_then(|snapshot| snapshot.config.as_ref()?.timer.freeze_on_boss_flag)
}

fn create_flag_cache(boss_flag_ids: &HashSet<i32>, freeze_flag: Option<i32>) -> EventFlagCache {
    EventFlagCache::new(
        boss_flag_ids
            .iter()
            .copied()
            .chain(GREAT_RUNE_FLAGS.iter().copied())
            .chain(freeze_flag),
    )
}

pub fn start_game_monitor(
    state: SharedState,
    igt: Arc<RwLock<u32>>,
    flag_ids: Vec<i32>,
    config: SharedConfig,
    key_item_id: i32,
    poll_ms: u64,
    stop: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let update_interval = Duration::from_millis(poll_ms.max(10));
        let boss_flag_ids: HashSet<_> = flag_ids
            .into_iter()
            .filter(|flag_id| !is_great_rune_flag(*flag_id))
            .collect();
        let mut igt_freeze = IgtFreezeLatch::new(configured_freeze_flag(&config));
        let mut flag_cache = create_flag_cache(&boss_flag_ids, igt_freeze.flag_id);
        let (
            mut published_flags,
            mut published_quantity,
            mut published_deaths,
            mut published_runes,
        ) = state
            .read()
            .map(|state| {
                (
                    state.event_flags.clone(),
                    state.key_item_quantity,
                    state.death_count,
                    state.great_runes,
                )
            })
            .unwrap_or_default();
        let previous_flag_count = published_flags.len();
        published_flags.retain(|flag_id, _| boss_flag_ids.contains(flag_id));
        let mut boss_flags_need_publish = published_flags.len() != previous_flag_count;
        let mut sampled_rune_flags = HashMap::with_capacity(GREAT_RUNE_FLAGS.len());
        let mut published_igt = igt.read().map(|value| *value).unwrap_or_default();
        let mut next_flag_error_log = Instant::now();
        let mut last_unresolved_count = None;

        while !stop.load(Ordering::Acquire) {
            let cycle_started = Instant::now();
            let configured_flag = configured_freeze_flag(&config);
            if igt_freeze.reconfigure(configured_flag) {
                flag_cache = create_flag_cache(&boss_flag_ids, configured_flag);
                last_unresolved_count = None;
                match configured_flag {
                    Some(flag_id) => debug_log!(
                        "[ignite_overlay] IGT freeze flag changed to {flag_id}; timer resumed until it activates"
                    ),
                    None => debug_log!("[ignite_overlay] IGT freeze flag disabled; timer resumed"),
                }
            }

            let mut flags_changed = boss_flags_need_publish;
            let mut igt_froze_this_cycle = false;
            match flag_cache.sample(cycle_started) {
                Ok(sample) => {
                    for (flag_id, value) in sample.values {
                        igt_froze_this_cycle |= igt_freeze.observe(flag_id, value);
                        if is_great_rune_flag(flag_id) {
                            sampled_rune_flags.insert(flag_id, value);
                        } else if boss_flag_ids.contains(&flag_id)
                            && published_flags.insert(flag_id, value) != Some(value)
                        {
                            flags_changed = true;
                        }
                    }
                    let unresolved_count = sample.unresolved.len();
                    if last_unresolved_count != Some(unresolved_count) {
                        if unresolved_count != 0 {
                            debug_log!(
                                "[event_flags] {unresolved_count} requested flags are not currently resolvable; retrying"
                            );
                        } else if last_unresolved_count.is_some_and(|count| count != 0) {
                            debug_log!("[event_flags] All requested flags resolved");
                        }
                        last_unresolved_count = Some(unresolved_count);
                    }
                }
                Err(error) => {
                    if cycle_started >= next_flag_error_log {
                        debug_log!("[event_flags] {error}; retrying");
                        next_flag_error_log = cycle_started + ERROR_LOG_INTERVAL;
                    }
                }
            }

            let runes = GREAT_RUNE_FLAGS
                .iter()
                .filter(|flag| sampled_rune_flags.get(flag).copied().unwrap_or(false))
                .count() as i32;
            let quantity = get_key_item_quantity(key_item_id);
            let game_data = read_game_data();
            let deaths = game_data
                .map(|snapshot| snapshot.death_count)
                .unwrap_or(published_deaths);
            let state_changed = flags_changed
                || quantity != published_quantity
                || deaths != published_deaths
                || runes != published_runes;
            if state_changed && let Ok(mut state) = state.write() {
                if flags_changed {
                    state.event_flags = published_flags.clone();
                    boss_flags_need_publish = false;
                }
                state.key_item_quantity = quantity;
                state.death_count = deaths;
                state.great_runes = runes;
                state.initialized = true;
                published_quantity = quantity;
                published_deaths = deaths;
                published_runes = runes;
            }

            if (!igt_freeze.frozen || igt_froze_this_cycle)
                && let Some(game_time) = game_data.map(|snapshot| snapshot.play_time_ms)
                && game_time != published_igt
                && let Ok(mut in_game_time) = igt.write()
            {
                *in_game_time = game_time;
                published_igt = game_time;
            }
            if igt_froze_this_cycle {
                debug_log!(
                    "[ignite_overlay] IGT frozen at {published_igt} ms after event flag {} activated",
                    igt_freeze
                        .flag_id
                        .expect("a configured flag triggered the latch")
                );
            }

            if let Some(remaining) = update_interval.checked_sub(cycle_started.elapsed()) {
                thread::sleep(remaining);
            }
        }
        debug_log!("[ignite_overlay] Monitor thread exiting gracefully");
    })
}

#[cfg(test)]
mod tests {
    use super::IgtFreezeLatch;

    #[test]
    fn freezes_only_when_the_configured_flag_activates() {
        let mut latch = IgtFreezeLatch::new(Some(123));

        assert!(!latch.observe(122, true));
        assert!(!latch.frozen);
        assert!(!latch.observe(123, false));
        assert!(!latch.frozen);
        assert!(latch.observe(123, true));
        assert!(latch.frozen);
        assert!(!latch.observe(123, true));
    }

    #[test]
    fn changing_or_disabling_the_flag_resets_the_latch() {
        let mut latch = IgtFreezeLatch::new(Some(123));
        assert!(latch.observe(123, true));

        assert!(!latch.reconfigure(Some(123)));
        assert!(latch.frozen);
        assert!(latch.reconfigure(Some(456)));
        assert!(!latch.frozen);
        assert!(latch.reconfigure(None));
        assert!(!latch.frozen);
        assert!(!latch.observe(456, true));
    }
}
