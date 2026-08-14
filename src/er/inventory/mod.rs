use eldenring::cs::{ItemCategory, ItemId, WorldChrMan};
use fromsoftware_shared::FromStatic;

#[inline(always)]
pub fn get_key_item_quantity(key_item_id: i32) -> u32 {
    let Ok(world_chr_man) = (unsafe { WorldChrMan::instance() }) else {
        return 0;
    };

    let Some(player_ptr) = &world_chr_man.main_player else {
        return 0;
    };

    let player = player_ptr.as_ref();
    let player_game_data = unsafe { player.player_game_data.as_ref() };

    let Ok(item_id) = ItemId::new(ItemCategory::Goods, key_item_id as u32) else {
        return 0;
    };

    let items = &player_game_data.equipment.equip_inventory_data.items_data;

    for entry in items.items() {
        if entry.item_id == item_id {
            return entry.quantity;
        }
    }

    0
}
