use eldenring::cs::GameDataMan;
use fromsoftware_shared::FromStatic;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GameDataSnapshot {
    pub death_count: u32,
    pub play_time_ms: u32,
}

pub fn read_game_data() -> Option<GameDataSnapshot> {
    let manager = unsafe { GameDataMan::instance().ok()? };
    Some(GameDataSnapshot {
        death_count: manager.death_count,
        play_time_ms: manager.play_time,
    })
}
