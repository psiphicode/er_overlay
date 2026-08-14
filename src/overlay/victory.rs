use std::collections::{BTreeSet, HashMap, HashSet};

use crate::overlay::config::VictoryCondition;

#[derive(Debug)]
pub struct VictoryTracker {
    condition: VictoryCondition,
    checklist_ids: BTreeSet<i32>,
    target_ids: BTreeSet<i32>,
    complete: bool,
}

impl VictoryTracker {
    pub fn new(condition: VictoryCondition, checklist_ids: impl IntoIterator<Item = i32>) -> Self {
        let checklist_ids = checklist_ids.into_iter().collect();
        let target_ids = Self::targets(&condition, &checklist_ids);
        Self {
            condition,
            checklist_ids,
            target_ids,
            complete: false,
        }
    }

    fn targets(condition: &VictoryCondition, checklist_ids: &BTreeSet<i32>) -> BTreeSet<i32> {
        match condition {
            VictoryCondition::Checklist => checklist_ids.clone(),
            VictoryCondition::BossIds(ids) => ids.iter().copied().collect(),
            VictoryCondition::OneBoss(id) => BTreeSet::from([*id]),
            VictoryCondition::None => BTreeSet::new(),
        }
    }

    pub fn reconfigure(&mut self, condition: VictoryCondition) -> bool {
        if self.complete || self.condition == condition {
            return false;
        }
        self.target_ids = Self::targets(&condition, &self.checklist_ids);
        self.condition = condition;
        true
    }

    pub fn requested_flag_ids(&self) -> impl Iterator<Item = i32> + '_ {
        self.target_ids.iter().copied()
    }

    pub fn observe(&mut self, values: &HashMap<i32, bool>, unresolved: &[i32]) -> bool {
        if self.complete || self.target_ids.is_empty() {
            return false;
        }
        let unresolved: HashSet<_> = unresolved.iter().copied().collect();
        if self.target_ids.iter().any(|id| unresolved.contains(id))
            || !self
                .target_ids
                .iter()
                .all(|id| values.get(id).copied().unwrap_or(false))
        {
            return false;
        }
        self.complete = true;
        true
    }

    pub fn is_complete(&self) -> bool {
        self.complete
    }

    pub fn condition(&self) -> &VictoryCondition {
        &self.condition
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::overlay::config::VictoryCondition;

    use super::VictoryTracker;

    #[test]
    fn none_and_empty_checklists_never_complete() {
        for condition in [VictoryCondition::None, VictoryCondition::Checklist] {
            let mut tracker = VictoryTracker::new(condition, []);
            assert!(!tracker.observe(&HashMap::new(), &[]));
            assert!(!tracker.is_complete());
        }
    }

    #[test]
    fn boss_ids_require_every_target_and_resolved_value() {
        let mut tracker = VictoryTracker::new(VictoryCondition::BossIds(vec![10, 20]), [1, 2]);
        assert!(!tracker.observe(&HashMap::from([(10, true)]), &[20]));
        assert!(!tracker.observe(&HashMap::from([(10, true), (20, false)]), &[]));
        assert!(tracker.observe(&HashMap::from([(10, true), (20, true)]), &[]));
        assert!(tracker.is_complete());
    }

    #[test]
    fn checklist_and_one_boss_resolve_their_own_targets() {
        let checklist = VictoryTracker::new(VictoryCondition::Checklist, [1, 2]);
        assert_eq!(checklist.requested_flag_ids().collect::<Vec<_>>(), [1, 2]);

        let one_boss = VictoryTracker::new(VictoryCondition::OneBoss(99), [1, 2]);
        assert_eq!(one_boss.requested_flag_ids().collect::<Vec<_>>(), [99]);
    }

    #[test]
    fn reconfigures_before_completion_and_ignores_changes_after_completion() {
        let mut tracker = VictoryTracker::new(VictoryCondition::OneBoss(10), [1]);
        assert!(tracker.reconfigure(VictoryCondition::OneBoss(20)));
        assert!(!tracker.observe(&HashMap::from([(10, true)]), &[]));
        assert!(tracker.observe(&HashMap::from([(20, true)]), &[]));
        assert!(!tracker.reconfigure(VictoryCondition::None));
        assert!(tracker.is_complete());
        assert_eq!(tracker.condition(), &VictoryCondition::OneBoss(20));
    }
}
