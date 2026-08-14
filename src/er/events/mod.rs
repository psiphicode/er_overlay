use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt,
    mem::{MaybeUninit, size_of},
    time::{Duration, Instant},
};

use eldenring::cs::CSEventFlagMan;
use fromsoftware_shared::FromStatic;
use windows_sys::Win32::System::{
    Diagnostics::Debug::ReadProcessMemory, Threading::GetCurrentProcess,
};

const EVENT_FLAG_DIVISOR: usize = 0x1C;
const FLAG_HOLDER_ENTRY_SIZE: usize = 0x20;
const FLAG_HOLDER_COUNT: usize = 0x24;
const FLAG_HOLDER: usize = 0x28;
const FLAG_GROUP_TREE_HEAD: usize = 0x38;
const FLAG_GROUP_TREE_SIZE: usize = 0x40;
const MANAGER_HEADER_SIZE: usize = 0x48;

const NODE_LEFT: usize = 0x00;
const NODE_PARENT: usize = 0x08;
const NODE_RIGHT: usize = 0x10;
const NODE_IS_NIL: usize = 0x19;
const NODE_GROUP: usize = 0x20;
const NODE_LOCATION_MODE: usize = 0x28;
const NODE_LOCATION: usize = 0x30;
const NODE_SIZE: usize = 0x38;

const COMPLETE_CACHE_REFRESH: Duration = Duration::from_secs(5);
const INCOMPLETE_CACHE_RETRY: Duration = Duration::from_millis(500);
const MAX_ENTRY_SIZE: usize = 4_096;
const MAX_HOLDER_COUNT: usize = 1_000_000;
const MAX_TREE_WALK: usize = 10_000;

trait MemoryReader {
    fn read_exact(&self, address: usize, destination: &mut [u8]) -> bool;
}

#[derive(Clone, Copy, Default)]
struct ProcessMemoryReader;

impl MemoryReader for ProcessMemoryReader {
    fn read_exact(&self, address: usize, destination: &mut [u8]) -> bool {
        if address == 0 || destination.is_empty() {
            return false;
        }
        let mut bytes_read = 0;
        let succeeded = unsafe {
            ReadProcessMemory(
                GetCurrentProcess(),
                address as *const _,
                destination.as_mut_ptr().cast(),
                destination.len(),
                &mut bytes_read,
            )
        };
        succeeded != 0 && bytes_read == destination.len()
    }
}

fn read_value<R: MemoryReader, T: Copy>(reader: &R, address: usize) -> Option<T> {
    let mut value = MaybeUninit::<T>::uninit();
    let destination =
        unsafe { std::slice::from_raw_parts_mut(value.as_mut_ptr().cast::<u8>(), size_of::<T>()) };
    reader
        .read_exact(address, destination)
        .then(|| unsafe { value.assume_init() })
}

fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn usize_at(bytes: &[u8], offset: usize) -> Option<usize> {
    Some(u64::from_le_bytes(bytes.get(offset..offset + 8)?.try_into().ok()?) as usize)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ManagerSnapshot {
    manager: usize,
    divisor: u32,
    entry_size: usize,
    holder_count: usize,
    holder: usize,
    tree_head: usize,
    tree_root: usize,
    tree_size: usize,
}

fn resolve_event_flag_manager() -> Option<usize> {
    let manager = unsafe { CSEventFlagMan::instance().ok()? };
    Some(std::ptr::from_ref(manager) as usize)
}

fn read_manager_snapshot<R: MemoryReader>(reader: &R, manager: usize) -> Option<ManagerSnapshot> {
    let mut header = [0_u8; MANAGER_HEADER_SIZE];
    if !reader.read_exact(manager, &mut header) {
        return None;
    }
    let divisor = u32_at(&header, EVENT_FLAG_DIVISOR)?;
    let entry_size = u32_at(&header, FLAG_HOLDER_ENTRY_SIZE)? as usize;
    let holder_count = u32_at(&header, FLAG_HOLDER_COUNT)? as usize;
    let holder = usize_at(&header, FLAG_HOLDER)?;
    let tree_head = usize_at(&header, FLAG_GROUP_TREE_HEAD)?;
    let tree_size = usize_at(&header, FLAG_GROUP_TREE_SIZE)?;
    if divisor == 0
        || entry_size == 0
        || entry_size > MAX_ENTRY_SIZE
        || entry_size < (divisor as usize).div_ceil(8)
        || holder_count > MAX_HOLDER_COUNT
        || tree_size > MAX_HOLDER_COUNT
        || tree_head == 0
        || (holder_count != 0 && holder == 0)
    {
        return None;
    }
    let tree_root = read_value::<_, usize>(reader, tree_head.checked_add(NODE_PARENT)?)?;
    if tree_root == 0 {
        return None;
    }
    Some(ManagerSnapshot {
        manager,
        divisor,
        entry_size,
        holder_count,
        holder,
        tree_head,
        tree_root,
        tree_size,
    })
}

#[derive(Clone, Copy, Debug)]
struct FlagSpec {
    id: i32,
    byte: usize,
    mask: u8,
}

#[derive(Debug)]
struct FlagBlockPlan {
    base: usize,
    flags: Vec<FlagSpec>,
}

#[derive(Debug)]
struct FlagReadPlan {
    snapshot: ManagerSnapshot,
    blocks: Vec<FlagBlockPlan>,
    unresolved: Vec<i32>,
}

fn resolve_group<R: MemoryReader>(
    reader: &R,
    snapshot: ManagerSnapshot,
    wanted_group: u32,
) -> Option<usize> {
    let mut current = snapshot.tree_root;
    let walk_limit = snapshot.tree_size.saturating_add(1).min(MAX_TREE_WALK);
    for _ in 0..walk_limit {
        if current == snapshot.tree_head {
            return None;
        }
        let mut node = [0_u8; NODE_SIZE];
        if !reader.read_exact(current, &mut node) || node[NODE_IS_NIL] != 0 {
            return None;
        }
        let group = u32_at(&node, NODE_GROUP)?;
        if wanted_group < group {
            current = usize_at(&node, NODE_LEFT)?;
            continue;
        }
        if wanted_group > group {
            current = usize_at(&node, NODE_RIGHT)?;
            continue;
        }
        let location_mode = u32_at(&node, NODE_LOCATION_MODE)?;
        return match location_mode {
            1 => {
                let holder_index = u32_at(&node, NODE_LOCATION)? as usize;
                (holder_index < snapshot.holder_count)
                    .then_some(holder_index)
                    .and_then(|index| snapshot.entry_size.checked_mul(index))
                    .and_then(|offset| snapshot.holder.checked_add(offset))
            }
            2 => usize_at(&node, NODE_LOCATION).filter(|location| *location != 0),
            _ => None,
        };
    }
    None
}

fn build_read_plan<R: MemoryReader>(
    reader: &R,
    snapshot: ManagerSnapshot,
    flag_ids: &[i32],
) -> Option<FlagReadPlan> {
    let mut groups = BTreeMap::<u32, Vec<FlagSpec>>::new();
    let mut unresolved = Vec::new();
    for id in flag_ids.iter().copied().collect::<BTreeSet<_>>() {
        let Ok(id_u32) = u32::try_from(id) else {
            unresolved.push(id);
            continue;
        };
        let group = id_u32 / snapshot.divisor;
        let bit = id_u32 % snapshot.divisor;
        let byte = (bit / 8) as usize;
        if byte >= snapshot.entry_size {
            unresolved.push(id);
            continue;
        }
        groups.entry(group).or_default().push(FlagSpec {
            id,
            byte,
            mask: 1 << (7 - (bit % 8)),
        });
    }

    let mut by_base = BTreeMap::<usize, Vec<FlagSpec>>::new();
    for (group, flags) in groups {
        if let Some(base) = resolve_group(reader, snapshot, group) {
            by_base.entry(base).or_default().extend(flags);
        } else {
            unresolved.extend(flags.into_iter().map(|flag| flag.id));
        }
    }
    unresolved.sort_unstable();
    let plan = FlagReadPlan {
        snapshot,
        blocks: by_base
            .into_iter()
            .map(|(base, flags)| FlagBlockPlan { base, flags })
            .collect(),
        unresolved,
    };
    (read_manager_snapshot(reader, snapshot.manager)? == snapshot).then_some(plan)
}

fn execute_read_plan<R: MemoryReader>(
    reader: &R,
    plan: &FlagReadPlan,
) -> Option<HashMap<i32, bool>> {
    let mut values =
        HashMap::with_capacity(plan.blocks.iter().map(|block| block.flags.len()).sum());
    let mut block_bytes = vec![0_u8; plan.snapshot.entry_size];
    for block in &plan.blocks {
        if !reader.read_exact(block.base, &mut block_bytes) {
            return None;
        }
        for flag in &block.flags {
            values.insert(flag.id, block_bytes[flag.byte] & flag.mask != 0);
        }
    }
    Some(values)
}

#[derive(Debug)]
pub enum EventFlagReadError {
    ManagerUnavailable,
    InvalidManager,
    CacheBuildChanged,
    BlockReadFailed,
    ManagerChanged,
}

impl fmt::Display for EventFlagReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ManagerUnavailable => "event flag manager is unavailable",
            Self::InvalidManager => "event flag manager metadata is invalid",
            Self::CacheBuildChanged => "event flag data changed while building the cache",
            Self::BlockReadFailed => "an event flag block could not be read",
            Self::ManagerChanged => "event flag data changed while reading",
        })
    }
}

pub struct EventFlagSample {
    pub values: HashMap<i32, bool>,
    pub unresolved: Vec<i32>,
}

pub struct EventFlagCache {
    requested: Vec<i32>,
    reader: ProcessMemoryReader,
    plan: Option<FlagReadPlan>,
    next_rebuild: Instant,
}

impl EventFlagCache {
    pub fn new(flag_ids: impl IntoIterator<Item = i32>) -> Self {
        let requested = flag_ids.into_iter().collect::<BTreeSet<_>>();
        Self {
            requested: requested.into_iter().collect(),
            reader: ProcessMemoryReader,
            plan: None,
            next_rebuild: Instant::now(),
        }
    }

    pub fn sample(&mut self, now: Instant) -> Result<EventFlagSample, EventFlagReadError> {
        let manager = resolve_event_flag_manager().ok_or(EventFlagReadError::ManagerUnavailable)?;
        let snapshot = read_manager_snapshot(&self.reader, manager)
            .ok_or(EventFlagReadError::InvalidManager)?;
        let must_rebuild = self
            .plan
            .as_ref()
            .is_none_or(|plan| plan.snapshot != snapshot)
            || now >= self.next_rebuild;
        if must_rebuild {
            let plan = build_read_plan(&self.reader, snapshot, &self.requested)
                .ok_or(EventFlagReadError::CacheBuildChanged)?;
            self.next_rebuild = now
                + if plan.unresolved.is_empty() {
                    COMPLETE_CACHE_REFRESH
                } else {
                    INCOMPLETE_CACHE_RETRY
                };
            self.plan = Some(plan);
        }
        let plan = self.plan.as_ref().expect("event flag plan was just built");
        let plan_snapshot = plan.snapshot;
        let Some(values) = execute_read_plan(&self.reader, plan) else {
            self.plan = None;
            return Err(EventFlagReadError::BlockReadFailed);
        };
        if read_manager_snapshot(&self.reader, manager) != Some(plan_snapshot) {
            self.plan = None;
            return Err(EventFlagReadError::ManagerChanged);
        }
        Ok(EventFlagSample {
            values,
            unresolved: plan.unresolved.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, collections::HashMap};

    use super::{
        FLAG_GROUP_TREE_HEAD, FLAG_GROUP_TREE_SIZE, FLAG_HOLDER, FLAG_HOLDER_COUNT,
        FLAG_HOLDER_ENTRY_SIZE, NODE_GROUP, NODE_IS_NIL, NODE_LEFT, NODE_LOCATION,
        NODE_LOCATION_MODE, NODE_PARENT, NODE_RIGHT, build_read_plan, execute_read_plan,
        read_manager_snapshot,
    };

    #[derive(Default)]
    struct FakeMemory {
        bytes: HashMap<usize, u8>,
        reads: Cell<usize>,
    }

    impl super::MemoryReader for FakeMemory {
        fn read_exact(&self, address: usize, destination: &mut [u8]) -> bool {
            self.reads.set(self.reads.get() + 1);
            for (offset, byte) in destination.iter_mut().enumerate() {
                let Some(value) = self.bytes.get(&(address + offset)) else {
                    return false;
                };
                *byte = *value;
            }
            true
        }
    }

    impl FakeMemory {
        fn write_bytes(&mut self, address: usize, bytes: &[u8]) {
            for (offset, byte) in bytes.iter().copied().enumerate() {
                self.bytes.insert(address + offset, byte);
            }
        }

        fn write_u32(&mut self, address: usize, value: u32) {
            self.write_bytes(address, &value.to_le_bytes());
        }

        fn write_usize(&mut self, address: usize, value: usize) {
            self.write_bytes(address, &(value as u64).to_le_bytes());
        }

        fn reset_reads(&self) {
            self.reads.set(0);
        }
    }

    fn one_group_memory(group: u32, holder_index: usize) -> (FakeMemory, usize, usize) {
        const MANAGER: usize = 0x1000;
        const HOLDER: usize = 0x2000;
        const HEAD: usize = 0x3000;
        const NODE: usize = 0x4000;
        let mut memory = FakeMemory::default();
        memory.write_bytes(MANAGER, &[0; super::MANAGER_HEADER_SIZE]);
        memory.write_u32(MANAGER + super::EVENT_FLAG_DIVISOR, 1000);
        memory.write_u32(MANAGER + FLAG_HOLDER_ENTRY_SIZE, 125);
        memory.write_u32(MANAGER + FLAG_HOLDER_COUNT, 8);
        memory.write_usize(MANAGER + FLAG_HOLDER, HOLDER);
        memory.write_usize(MANAGER + FLAG_GROUP_TREE_HEAD, HEAD);
        memory.write_usize(MANAGER + FLAG_GROUP_TREE_SIZE, 1);
        memory.write_bytes(HEAD, &[0; super::NODE_SIZE]);
        memory.write_usize(HEAD + NODE_PARENT, NODE);
        memory.write_bytes(HEAD + NODE_IS_NIL, &[1]);
        memory.write_bytes(NODE, &[0; super::NODE_SIZE]);
        memory.write_usize(NODE + NODE_LEFT, HEAD);
        memory.write_usize(NODE + NODE_PARENT, HEAD);
        memory.write_usize(NODE + NODE_RIGHT, HEAD);
        memory.write_u32(NODE + NODE_GROUP, group);
        memory.write_u32(NODE + NODE_LOCATION_MODE, 1);
        memory.write_usize(NODE + NODE_LOCATION, holder_index);
        let block = HOLDER + holder_index * 125;
        memory.write_bytes(block, &[0; 125]);
        (memory, MANAGER, block)
    }

    #[test]
    fn flags_in_one_group_use_one_batched_block_read() {
        let (mut memory, manager, block) = one_group_memory(1, 2);
        memory.write_bytes(block, &[0b1010_0000]);
        let snapshot = read_manager_snapshot(&memory, manager).unwrap();
        let plan = build_read_plan(&memory, snapshot, &[1000, 1001, 1002]).unwrap();
        assert!(plan.unresolved.is_empty());
        assert_eq!(plan.blocks.len(), 1);

        memory.reset_reads();
        let values = execute_read_plan(&memory, &plan).unwrap();
        assert_eq!(memory.reads.get(), 1);
        assert!(values[&1000]);
        assert!(!values[&1001]);
        assert!(values[&1002]);
    }

    #[test]
    fn invalid_flags_and_holder_locations_remain_unresolved() {
        let (mut memory, manager, _) = one_group_memory(1, 20);
        memory.write_u32(manager + FLAG_HOLDER_COUNT, 8);
        let snapshot = read_manager_snapshot(&memory, manager).unwrap();
        let plan = build_read_plan(&memory, snapshot, &[-1, 1000]).unwrap();
        assert!(plan.blocks.is_empty());
        assert_eq!(plan.unresolved, [-1, 1000]);
    }

    #[test]
    fn cache_build_rejects_manager_changes() {
        let (mut memory, manager, _) = one_group_memory(1, 2);
        let snapshot = read_manager_snapshot(&memory, manager).unwrap();
        memory.write_usize(manager + FLAG_GROUP_TREE_SIZE, 2);
        assert!(build_read_plan(&memory, snapshot, &[1000]).is_none());
    }
}
