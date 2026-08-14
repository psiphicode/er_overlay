use std::path::Path;

use imgui::Ui;

use crate::overlay::{
    config::RuntimeConfig,
    data::{BossRegions, RegionData, SharedState, load_localized_boss_data},
};

pub struct BossPanel {
    regions: BossRegions,
}

impl BossPanel {
    pub fn load(dll_directory: &Path, config: Option<&RuntimeConfig>) -> Self {
        let language = config
            .map(|config| config.common.language.trim())
            .filter(|language| !language.is_empty())
            .unwrap_or("engus");
        let data_file = config
            .map(|config| config.boss.data_file.trim())
            .filter(|file| !file.is_empty())
            .unwrap_or("bosses.json");
        Self {
            regions: load_localized_boss_data(dll_directory, language, data_file)
                .unwrap_or_default(),
        }
    }

    pub fn flag_ids(&self) -> Vec<i32> {
        self.regions
            .iter()
            .flat_map(|region| region.bosses.iter().map(|boss| boss.flag_id))
            .collect()
    }

    pub fn render(&self, ui: &Ui, state: &SharedState) {
        let Ok(state) = state.read() else {
            return;
        };
        for region in &self.regions {
            render_region(ui, region, &state.event_flags);
        }
    }
}

fn render_region(ui: &Ui, region: &RegionData, flags: &std::collections::HashMap<i32, bool>) {
    let defeated = region
        .bosses
        .iter()
        .filter(|boss| *flags.get(&boss.flag_id).unwrap_or(&false))
        .count();
    if let Some(_tree) = ui
        .tree_node_config(format!(
            "{} ({}/{})",
            region.region_name,
            defeated,
            region.bosses.len()
        ))
        .flags(imgui::TreeNodeFlags::SPAN_AVAIL_WIDTH)
        .push()
    {
        for boss in &region.bosses {
            let mut checked = *flags.get(&boss.flag_id).unwrap_or(&false);
            ui.checkbox(
                format!(
                    "{}{}",
                    boss.boss,
                    if boss.place.is_empty() { "" } else { " " }
                ),
                &mut checked,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::BossPanel;
    use crate::overlay::data::{BossEntry, RegionData};

    #[test]
    fn collects_flags_from_all_regions() {
        let panel = BossPanel {
            regions: vec![RegionData {
                region_name: "Test".to_string(),
                regions: vec![],
                bosses: vec![BossEntry {
                    boss: "Boss".to_string(),
                    place: String::new(),
                    flag_id: 123,
                    remembrance: None,
                }],
            }],
        };
        assert_eq!(panel.flag_ids(), [123]);
    }
}
