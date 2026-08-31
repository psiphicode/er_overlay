use std::{
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

use crate::overlay::{
    config::SharedConfig,
    data::SharedState,
    game_monitor::{SharedObservationSender, start_game_monitor},
};

pub struct OverlayRuntime {
    monitor_stop: Arc<AtomicBool>,
    monitor_thread: Option<thread::JoinHandle<()>>,
}

impl OverlayRuntime {
    pub fn new() -> Self {
        Self {
            monitor_stop: Arc::new(AtomicBool::new(false)),
            monitor_thread: None,
        }
    }

    pub fn start(
        &mut self,
        state: SharedState,
        in_game_time: Arc<RwLock<u32>>,
        boss_flags: Vec<i32>,
        config: SharedConfig,
        key_item_id: i32,
        poll_ms: u64,
        observation_tx: SharedObservationSender,
    ) {
        self.stop();
        self.monitor_stop.store(false, Ordering::Release);
        self.monitor_thread = Some(start_game_monitor(
            state,
            in_game_time,
            boss_flags,
            config,
            key_item_id,
            poll_ms,
            self.monitor_stop.clone(),
            observation_tx,
        ));
    }

    pub fn stop(&mut self) {
        self.monitor_stop.store(true, Ordering::Release);
        if let Some(thread) = self.monitor_thread.take() {
            let _ = thread.join();
        }
    }
}

impl Default for OverlayRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for OverlayRuntime {
    fn drop(&mut self) {
        self.stop();
    }
}
