use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, RwLock, RwLockReadGuard, RwLockWriteGuard,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

use crossbeam::channel::Sender;

use crate::{
    debug_log,
    er::{events::EventFlagCache, gamedata::read_game_data, inventory::get_key_item_quantity},
    overlay::{
        config::{ConfigSnapshot, SharedConfig, VictoryCondition},
        data::SharedState,
        victory::VictoryTracker,
    },
};

const GREAT_RUNE_FLAGS: [i32; 7] = [181, 182, 183, 184, 185, 186, 187];
const ERROR_LOG_INTERVAL: Duration = Duration::from_secs(5);

pub(crate) fn is_great_rune_flag(flag_id: i32) -> bool {
    GREAT_RUNE_FLAGS.contains(&flag_id)
}

fn write_unpoisoned<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    match lock.write() {
        Ok(guard) => guard,
        Err(error) => {
            lock.clear_poison();
            error.into_inner()
        }
    }
}

fn read_unpoisoned<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    match lock.read() {
        Ok(guard) => guard,
        Err(error) => {
            lock.clear_poison();
            error.into_inner()
        }
    }
}

fn initial_state_snapshot(state: &SharedState) -> (HashMap<i32, bool>, u32, u32, i32, bool) {
    let state = read_unpoisoned(state);
    (
        state.event_flags.clone(),
        state.key_item_quantity,
        state.death_count,
        state.great_runes,
        state.goal_complete,
    )
}

fn initial_published_igt(igt: &RwLock<u32>) -> u32 {
    *read_unpoisoned(igt)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ConfiguredVictory {
    revision: u64,
    condition: VictoryCondition,
}

impl From<&ConfigSnapshot> for ConfiguredVictory {
    fn from(snapshot: &ConfigSnapshot) -> Self {
        Self {
            revision: snapshot.revision,
            condition: snapshot
                .config
                .as_ref()
                .map(|config| config.victory.clone())
                .unwrap_or_default(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum SampleObservation {
    Current(bool),
    Stale(ConfiguredVictory),
}

fn configured_victory(config: &SharedConfig) -> ConfiguredVictory {
    let snapshot = read_unpoisoned(config);
    ConfiguredVictory::from(&*snapshot)
}

fn reconfigure_victory(
    configured: &mut ConfiguredVictory,
    latest: ConfiguredVictory,
    victory: &mut VictoryTracker,
) -> bool {
    if *configured == latest {
        return false;
    }
    let reconfigured = victory.reconfigure(latest.condition.clone());
    *configured = latest;
    reconfigured
}

fn observe_if_current(
    config: &SharedConfig,
    sampled_revision: u64,
    game_time: Option<u32>,
    victory: &mut VictoryTracker,
    values: &HashMap<i32, bool>,
    unresolved: &[i32],
) -> SampleObservation {
    let snapshot = read_unpoisoned(config);
    if snapshot.revision != sampled_revision {
        return SampleObservation::Stale(ConfiguredVictory::from(&*snapshot));
    }
    if game_time.is_none() {
        return SampleObservation::Current(false);
    }
    SampleObservation::Current(victory.observe(values, unresolved))
}

fn monitored_flag_ids(boss_flag_ids: &HashSet<i32>, victory: &VictoryTracker) -> HashSet<i32> {
    boss_flag_ids
        .iter()
        .copied()
        .chain(GREAT_RUNE_FLAGS)
        .chain(victory.requested_flag_ids())
        .collect()
}

fn create_flag_cache(boss_flag_ids: &HashSet<i32>, victory: &VictoryTracker) -> EventFlagCache {
    EventFlagCache::new(monitored_flag_ids(boss_flag_ids, victory))
}

fn merge_boss_flags(
    published: &mut HashMap<i32, bool>,
    sampled: &HashMap<i32, bool>,
    boss_flag_ids: &HashSet<i32>,
) -> bool {
    let mut changed = false;
    for flag_id in boss_flag_ids {
        if let Some(value) = sampled.get(flag_id)
            && published.insert(*flag_id, *value) != Some(*value)
        {
            changed = true;
        }
    }
    changed
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonitorObservation {
    pub active_boss_flags: Vec<i32>,
    pub observed_at: SystemTime,
}

pub struct ObservationSenderState {
    revision: u64,
    sender: Option<Sender<MonitorObservation>>,
}

pub type SharedObservationSender = Arc<RwLock<ObservationSenderState>>;

pub fn create_observation_sender() -> SharedObservationSender {
    Arc::new(RwLock::new(ObservationSenderState {
        revision: 0,
        sender: None,
    }))
}

pub(crate) fn replace_observation_sender(
    shared: &SharedObservationSender,
    sender: Option<Sender<MonitorObservation>>,
) {
    let mut state = write_unpoisoned(shared);
    state.revision = state.revision.wrapping_add(1);
    state.sender = sender;
}

pub(crate) fn current_observation_sender(
    shared: &SharedObservationSender,
) -> (u64, Option<Sender<MonitorObservation>>) {
    let state = read_unpoisoned(shared);
    (state.revision, state.sender.clone())
}

fn build_monitor_observation(
    sampled: &HashMap<i32, bool>,
    boss_flag_ids: &HashSet<i32>,
    observed_at: SystemTime,
) -> MonitorObservation {
    let mut active_boss_flags = boss_flag_ids
        .iter()
        .filter(|flag_id| sampled.get(flag_id).copied().unwrap_or(false))
        .copied()
        .collect::<Vec<_>>();
    active_boss_flags.sort_unstable();
    MonitorObservation {
        active_boss_flags,
        observed_at,
    }
}

fn deliver_monitor_observation(
    sender: Option<&Sender<MonitorObservation>>,
    observation: MonitorObservation,
) {
    if let Some(sender) = sender {
        let _ = sender.send(observation);
    }
}

fn monitor_observation_due(
    flags_changed: bool,
    last_reporter_revision: u64,
    reporter_revision: u64,
    reporter_enabled: bool,
) -> bool {
    reporter_enabled && (flags_changed || reporter_revision != last_reporter_revision)
}

struct MonitorStateUpdate {
    event_flags: Option<HashMap<i32, bool>>,
    key_item_quantity: u32,
    death_count: u32,
    great_runes: i32,
    goal_complete: bool,
}

fn publish_state_after_igt(
    state: &SharedState,
    igt: &RwLock<u32>,
    game_time: Option<u32>,
    update: MonitorStateUpdate,
) {
    if let Some(game_time) = game_time {
        *write_unpoisoned(igt) = game_time;
    }
    let mut state = write_unpoisoned(state);
    if let Some(event_flags) = update.event_flags {
        state.event_flags = event_flags;
    }
    state.key_item_quantity = update.key_item_quantity;
    state.death_count = update.death_count;
    state.great_runes = update.great_runes;
    state.goal_complete = update.goal_complete;
    state.initialized = true;
}

pub fn start_game_monitor(
    state: SharedState,
    igt: Arc<RwLock<u32>>,
    flag_ids: Vec<i32>,
    config: SharedConfig,
    key_item_id: i32,
    poll_ms: u64,
    stop: Arc<AtomicBool>,
    observation_tx: SharedObservationSender,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let update_interval = Duration::from_millis(poll_ms.max(10));
        let boss_flag_ids: HashSet<_> = flag_ids
            .into_iter()
            .filter(|flag_id| !is_great_rune_flag(*flag_id))
            .collect();
        let mut configured = configured_victory(&config);
        let mut victory =
            VictoryTracker::new(configured.condition.clone(), boss_flag_ids.iter().copied());
        let mut flag_cache = create_flag_cache(&boss_flag_ids, &victory);
        let (
            mut published_flags,
            mut published_quantity,
            mut published_deaths,
            mut published_runes,
            mut published_goal_complete,
        ) = initial_state_snapshot(&state);
        let previous_flag_count = published_flags.len();
        published_flags.retain(|flag_id, _| boss_flag_ids.contains(flag_id));
        let mut boss_flags_need_publish = published_flags.len() != previous_flag_count;
        let mut sampled_rune_flags = HashMap::with_capacity(GREAT_RUNE_FLAGS.len());
        let mut published_igt = initial_published_igt(&igt);
        let mut next_flag_error_log = Instant::now();
        let mut last_unresolved_count = None;
        let mut last_reporter_revision = 0;

        while !stop.load(Ordering::Acquire) {
            let cycle_started = Instant::now();
            let latest = configured_victory(&config);
            if reconfigure_victory(&mut configured, latest, &mut victory) {
                flag_cache = create_flag_cache(&boss_flag_ids, &victory);
                last_unresolved_count = None;
                debug_log!(
                    "[ignite_overlay] Victory condition changed to {:?}",
                    victory.condition()
                );
            }

            let mut flags_changed = boss_flags_need_publish;
            let mut completed_this_cycle = false;
            let game_data = read_game_data();
            match flag_cache.sample(cycle_started) {
                Ok(sample) => {
                    match observe_if_current(
                        &config,
                        configured.revision,
                        game_data.map(|snapshot| snapshot.play_time_ms),
                        &mut victory,
                        &sample.values,
                        &sample.unresolved,
                    ) {
                        SampleObservation::Current(completed) => {
                            completed_this_cycle = completed;
                            flags_changed |= merge_boss_flags(
                                &mut published_flags,
                                &sample.values,
                                &boss_flag_ids,
                            );
                            let (reporter_revision, sender) =
                                current_observation_sender(&observation_tx);
                            if monitor_observation_due(
                                flags_changed,
                                last_reporter_revision,
                                reporter_revision,
                                sender.is_some(),
                            ) && let Some(sender) = sender
                            {
                                let observation = build_monitor_observation(
                                    &published_flags,
                                    &boss_flag_ids,
                                    SystemTime::now(),
                                );
                                deliver_monitor_observation(Some(&sender), observation);
                            }
                            last_reporter_revision = reporter_revision;
                            for (flag_id, value) in sample.values {
                                if is_great_rune_flag(flag_id) {
                                    sampled_rune_flags.insert(flag_id, value);
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
                        SampleObservation::Stale(latest) => {
                            if reconfigure_victory(&mut configured, latest, &mut victory) {
                                flag_cache = create_flag_cache(&boss_flag_ids, &victory);
                                last_unresolved_count = None;
                                debug_log!(
                                    "[ignite_overlay] Victory condition changed to {:?}",
                                    victory.condition()
                                );
                            }
                        }
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
            let deaths = game_data
                .map(|snapshot| snapshot.death_count)
                .unwrap_or(published_deaths);
            let state_changed = flags_changed
                || quantity != published_quantity
                || deaths != published_deaths
                || runes != published_runes
                || victory.is_complete() != published_goal_complete;
            let game_time_to_publish = if !victory.is_complete() || completed_this_cycle {
                game_data
                    .map(|snapshot| snapshot.play_time_ms)
                    .filter(|game_time| *game_time != published_igt)
            } else {
                None
            };
            if state_changed {
                publish_state_after_igt(
                    &state,
                    &igt,
                    game_time_to_publish,
                    MonitorStateUpdate {
                        event_flags: flags_changed.then(|| published_flags.clone()),
                        key_item_quantity: quantity,
                        death_count: deaths,
                        great_runes: runes,
                        goal_complete: victory.is_complete(),
                    },
                );
                if flags_changed {
                    boss_flags_need_publish = false;
                }
                published_quantity = quantity;
                published_deaths = deaths;
                published_runes = runes;
                published_goal_complete = victory.is_complete();
            } else if let Some(game_time) = game_time_to_publish {
                let mut in_game_time = write_unpoisoned(&igt);
                *in_game_time = game_time;
            }
            if let Some(game_time) = game_time_to_publish {
                published_igt = game_time;
            }
            if completed_this_cycle {
                debug_log!(
                    "[ignite_overlay] Victory condition {:?} completed at {published_igt} ms",
                    victory.condition()
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
    use std::{
        collections::{HashMap, HashSet},
        sync::{Arc, RwLock},
        thread,
        time::{Duration, Instant, SystemTime},
    };

    use crate::overlay::{
        config::{ConfigSnapshot, RuntimeConfig, SharedConfig, VictoryCondition},
        data::create_state,
        style::IgniteConfig,
        victory::VictoryTracker,
    };

    use super::{
        ConfiguredVictory, GREAT_RUNE_FLAGS, MonitorObservation, MonitorStateUpdate,
        SampleObservation, build_monitor_observation, configured_victory,
        create_observation_sender, current_observation_sender, deliver_monitor_observation,
        initial_published_igt, initial_state_snapshot, merge_boss_flags, monitor_observation_due,
        monitored_flag_ids, observe_if_current, publish_state_after_igt, reconfigure_victory,
        replace_observation_sender, write_unpoisoned,
    };

    fn runtime_with_victory(victory: VictoryCondition) -> Arc<RuntimeConfig> {
        let mut runtime = RuntimeConfig::try_from(IgniteConfig {
            common: None,
            input: None,
            style: None,
            boss: None,
            overlay: None,
            timer: None,
            victory: None,
            ingest: None,
        })
        .unwrap();
        runtime.victory = victory;
        Arc::new(runtime)
    }

    #[test]
    fn monitor_observation_contains_only_sorted_active_boss_flags() {
        let sampled = HashMap::from([(30, true), (10, false), (20, true), (181, true)]);
        let boss_ids = HashSet::from([10, 20, 30]);
        let observed_at = SystemTime::UNIX_EPOCH + Duration::from_secs(7);

        let observation = build_monitor_observation(&sampled, &boss_ids, observed_at);

        assert_eq!(observation.active_boss_flags, [20, 30]);
        assert_eq!(observation.observed_at, observed_at);
    }

    #[test]
    fn disconnected_reporter_does_not_fail_monitor_delivery() {
        let (sender, receiver) = crossbeam::channel::unbounded();
        drop(receiver);
        let observation = MonitorObservation {
            active_boss_flags: vec![20, 30],
            observed_at: SystemTime::UNIX_EPOCH,
        };

        deliver_monitor_observation(Some(&sender), observation);
    }

    #[test]
    fn monitor_delivery_switches_channels_without_cross_generation_leakage() {
        let shared = create_observation_sender();
        let (first_sender, first_receiver) = crossbeam::channel::unbounded();
        let (second_sender, second_receiver) = crossbeam::channel::unbounded();
        replace_observation_sender(&shared, Some(first_sender));

        let first = MonitorObservation {
            active_boss_flags: vec![10],
            observed_at: SystemTime::UNIX_EPOCH,
        };
        let (_, current) = current_observation_sender(&shared);
        deliver_monitor_observation(current.as_ref(), first.clone());
        assert_eq!(first_receiver.recv().unwrap(), first);

        replace_observation_sender(&shared, Some(second_sender));
        let second = MonitorObservation {
            active_boss_flags: vec![20],
            observed_at: SystemTime::UNIX_EPOCH + Duration::from_secs(1),
        };
        let (_, current) = current_observation_sender(&shared);
        deliver_monitor_observation(current.as_ref(), second.clone());

        assert!(first_receiver.try_recv().is_err());
        assert_eq!(second_receiver.recv().unwrap(), second);
    }

    #[test]
    fn new_reporter_generation_receives_current_snapshot_without_a_flag_change() {
        assert!(!monitor_observation_due(false, 4, 4, true));
        assert!(monitor_observation_due(false, 4, 5, true));
        assert!(!monitor_observation_due(false, 4, 5, false));
        assert!(monitor_observation_due(true, 5, 5, true));
    }

    fn shared_config(revision: u64, victory: VictoryCondition) -> SharedConfig {
        Arc::new(RwLock::new(ConfigSnapshot {
            revision,
            config: Some(runtime_with_victory(victory)),
        }))
    }

    #[test]
    fn monitor_targets_include_victory_ids_without_making_them_bosses() {
        let boss_ids = HashSet::from([1, 2]);
        let tracker = VictoryTracker::new(VictoryCondition::BossIds(vec![20, 30]), [1, 2]);
        let monitored = monitored_flag_ids(&boss_ids, &tracker);

        assert!(monitored.is_superset(&HashSet::from([1, 2, 20, 30])));
        assert!(GREAT_RUNE_FLAGS.iter().all(|id| monitored.contains(id)));

        let mut published = HashMap::new();
        assert!(merge_boss_flags(
            &mut published,
            &HashMap::from([(1, true), (20, true)]),
            &boss_ids,
        ));
        assert_eq!(published, HashMap::from([(1, true)]));
    }

    #[test]
    fn poisoned_shared_writes_recover_the_inner_state() {
        let shared = Arc::new(RwLock::new(0));
        let poisoned = Arc::clone(&shared);
        assert!(
            thread::spawn(move || {
                let _guard = poisoned.write().unwrap();
                panic!("poison test lock");
            })
            .join()
            .is_err()
        );

        *write_unpoisoned(&shared) = 1;

        assert_eq!(*shared.read().unwrap(), 1);
    }

    #[test]
    fn runtime_config_poison_recovers_the_valid_snapshot_and_allows_reload() {
        let config = shared_config(7, VictoryCondition::OneBoss(10));
        let mut configured = configured_victory(&config);
        let mut victory = VictoryTracker::new(configured.condition.clone(), []);
        let poisoned = Arc::clone(&config);
        assert!(
            thread::spawn(move || {
                let _guard = poisoned.write().unwrap();
                panic!("poison test config");
            })
            .join()
            .is_err()
        );

        let recovered = configured_victory(&config);
        assert_eq!(recovered.revision, 7);
        assert_eq!(recovered.condition, VictoryCondition::OneBoss(10));
        assert!(config.write().is_ok());

        *config.write().unwrap() = ConfigSnapshot {
            revision: 8,
            config: Some(runtime_with_victory(VictoryCondition::OneBoss(20))),
        };
        let latest = configured_victory(&config);
        assert!(reconfigure_victory(&mut configured, latest, &mut victory,));
        assert_eq!(victory.condition(), &VictoryCondition::OneBoss(20));
    }

    #[test]
    fn monitor_initialization_recovers_a_poisoned_valid_victory_snapshot() {
        let config = shared_config(7, VictoryCondition::OneBoss(10));
        let poisoned = Arc::clone(&config);
        assert!(
            thread::spawn(move || {
                let _guard = poisoned.write().unwrap();
                panic!("poison test initial config");
            })
            .join()
            .is_err()
        );

        let configured = configured_victory(&config);

        assert_eq!(configured.revision, 7);
        assert_eq!(configured.condition, VictoryCondition::OneBoss(10));
        assert!(config.read().is_ok());
    }

    #[test]
    fn hot_reload_between_sampling_and_observation_discards_the_stale_sample() {
        let config = shared_config(0, VictoryCondition::OneBoss(10));
        let mut configured = configured_victory(&config);
        let mut victory = VictoryTracker::new(configured.condition.clone(), []);

        *config.write().unwrap() = ConfigSnapshot {
            revision: 1,
            config: Some(runtime_with_victory(VictoryCondition::OneBoss(20))),
        };

        let latest = match observe_if_current(
            &config,
            configured.revision,
            None,
            &mut victory,
            &HashMap::from([(10, true)]),
            &[],
        ) {
            SampleObservation::Stale(latest) => latest,
            other => panic!("expected a stale sample, got {other:?}"),
        };
        assert!(!victory.is_complete());
        assert!(reconfigure_victory(&mut configured, latest, &mut victory,));
        assert_eq!(victory.requested_flag_ids().collect::<Vec<_>>(), [20]);
    }

    #[test]
    fn same_condition_revision_updates_without_rebuilding_victory() {
        let mut configured = ConfiguredVictory {
            revision: 1,
            condition: VictoryCondition::OneBoss(10),
        };
        let mut victory = VictoryTracker::new(configured.condition.clone(), []);

        assert!(!reconfigure_victory(
            &mut configured,
            ConfiguredVictory {
                revision: 2,
                condition: VictoryCondition::OneBoss(10),
            },
            &mut victory,
        ));
        assert_eq!(configured.revision, 2);
        assert_eq!(victory.condition(), &VictoryCondition::OneBoss(10));
    }

    #[test]
    fn post_completion_revision_updates_without_rebuilding_victory() {
        let mut configured = ConfiguredVictory {
            revision: 1,
            condition: VictoryCondition::OneBoss(10),
        };
        let mut victory = VictoryTracker::new(configured.condition.clone(), []);
        assert!(victory.observe(&HashMap::from([(10, true)]), &[]));

        assert!(!reconfigure_victory(
            &mut configured,
            ConfiguredVictory {
                revision: 2,
                condition: VictoryCondition::OneBoss(20),
            },
            &mut victory,
        ));
        assert_eq!(configured.revision, 2);
        assert_eq!(configured.condition, VictoryCondition::OneBoss(20));
        assert_eq!(victory.condition(), &VictoryCondition::OneBoss(10));
        assert!(victory.is_complete());
    }

    #[test]
    fn victory_waits_for_current_game_time_before_completing() {
        let config = shared_config(0, VictoryCondition::OneBoss(10));
        let configured = configured_victory(&config);
        let mut victory = VictoryTracker::new(configured.condition, []);
        let active_target = HashMap::from([(10, true)]);

        assert_eq!(
            observe_if_current(
                &config,
                configured.revision,
                None,
                &mut victory,
                &active_target,
                &[],
            ),
            SampleObservation::Current(false)
        );
        assert!(!victory.is_complete());

        assert_eq!(
            observe_if_current(
                &config,
                configured.revision,
                Some(456),
                &mut victory,
                &active_target,
                &[],
            ),
            SampleObservation::Current(true)
        );
        assert!(victory.is_complete());
    }

    #[test]
    fn completion_cycle_publishes_current_igt_before_goal_state() {
        let state = create_state();
        let igt = Arc::new(RwLock::new(100));
        let state_guard = state.write().unwrap();
        let worker_state = Arc::clone(&state);
        let worker_igt = Arc::clone(&igt);
        let worker = thread::spawn(move || {
            publish_state_after_igt(
                &worker_state,
                &worker_igt,
                Some(456),
                MonitorStateUpdate {
                    event_flags: None,
                    key_item_quantity: 0,
                    death_count: 0,
                    great_runes: 0,
                    goal_complete: true,
                },
            );
        });

        let deadline = Instant::now() + Duration::from_secs(2);
        while *igt.read().unwrap() != 456 {
            assert!(
                Instant::now() < deadline,
                "IGT was not published while goal-state publication was blocked"
            );
            thread::yield_now();
        }
        assert!(!state_guard.goal_complete);
        drop(state_guard);

        worker.join().unwrap();
        assert!(state.read().unwrap().goal_complete);
        assert_eq!(*igt.read().unwrap(), 456);
    }

    #[test]
    fn monitor_initialization_recovers_default_valued_poisoned_snapshots() {
        let state = create_state();
        let poisoned_state = Arc::clone(&state);
        assert!(
            thread::spawn(move || {
                let _guard = poisoned_state.write().unwrap();
                panic!("poison test state");
            })
            .join()
            .is_err()
        );
        let igt = Arc::new(RwLock::new(0));
        let poisoned_igt = Arc::clone(&igt);
        assert!(
            thread::spawn(move || {
                let _guard = poisoned_igt.write().unwrap();
                panic!("poison test IGT");
            })
            .join()
            .is_err()
        );

        let (flags, quantity, deaths, runes, goal_complete) = initial_state_snapshot(&state);
        let published_igt = initial_published_igt(&igt);

        assert!(flags.is_empty());
        assert_eq!((quantity, deaths, runes, goal_complete), (0, 0, 0, false));
        assert_eq!(published_igt, 0);
        assert!(state.read().is_ok());
        assert!(igt.read().is_ok());
    }
}
