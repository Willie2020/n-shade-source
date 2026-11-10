//! Bounded in-memory cache for stage outputs.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use crate::pipeline::{StageKey, StageOutput};
use crate::types::Stage;

pub const DEFAULT_CACHE_BUDGET_BYTES: usize = 512 * 1024 * 1024;

pub struct MapStore {
    entries: HashMap<StageKey, Arc<StageOutput>>,
    order: VecDeque<StageKey>,
    current: HashMap<Stage, StageKey>,
    bytes: usize,
    budget: usize,
}

impl Default for MapStore {
    fn default() -> Self {
        Self::with_budget(DEFAULT_CACHE_BUDGET_BYTES)
    }
}

impl MapStore {
    pub fn with_budget(budget: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            current: HashMap::new(),
            bytes: 0,
            budget,
        }
    }

    pub fn get(&self, key: &StageKey) -> Option<Arc<StageOutput>> {
        self.entries.get(key).cloned()
    }

    pub fn output(&self, stage: Stage) -> Option<&StageOutput> {
        let key = self.current.get(&stage)?;
        self.entries.get(key).map(AsRef::as_ref)
    }

    pub fn current_key(&self, stage: Stage) -> Option<StageKey> {
        self.current.get(&stage).copied()
    }

    pub fn set_current(&mut self, keys: &HashMap<Stage, StageKey>) {
        self.current = keys.clone();
    }

    pub fn insert(&mut self, key: StageKey, output: Arc<StageOutput>) {
        let size = output.approx_bytes();
        if let Some(previous) = self.entries.insert(key, output) {
            self.bytes = self.bytes.saturating_sub(previous.approx_bytes());
            self.order.retain(|candidate| *candidate != key);
        }
        self.bytes += size;
        self.order.push_back(key);
        self.evict();
    }

    pub fn cached_for(
        &self,
        keys: &HashMap<Stage, StageKey>,
    ) -> HashMap<StageKey, Arc<StageOutput>> {
        keys.values()
            .filter_map(|key| self.entries.get(key).map(|output| (*key, output.clone())))
            .collect()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.current.clear();
        self.bytes = 0;
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn budget(&self) -> usize {
        self.budget
    }

    fn evict(&mut self) {
        while self.bytes > self.budget && self.entries.len() > 1 {
            let protected: std::collections::HashSet<StageKey> =
                self.current.values().copied().collect();
            let Some(index) = self.order.iter().position(|key| !protected.contains(key)) else {
                break;
            };
            let Some(key) = self.order.remove(index) else {
                break;
            };
            if let Some(output) = self.entries.remove(&key) {
                self.bytes = self.bytes.saturating_sub(output.approx_bytes());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ns_core::Grid;

    fn key(value: u8) -> StageKey {
        StageKey([value; 32])
    }

    fn scalar() -> Arc<StageOutput> {
        Arc::new(StageOutput::Ao(Grid::new(8, 8, 0.0)))
    }

    #[test]
    fn insert_get_and_current() {
        let mut store = MapStore::with_budget(1024 * 1024);
        store.insert(key(1), scalar());
        assert!(store.get(&key(1)).is_some());
        let mut current = HashMap::new();
        current.insert(Stage::Ao, key(1));
        store.set_current(&current);
        assert!(store.output(Stage::Ao).is_some());
        assert!(store.current_key(Stage::Ao).is_some());
    }

    #[test]
    fn eviction_respects_budget_and_current_keys() {
        let mut store = MapStore::with_budget(200);
        let mut current = HashMap::new();
        current.insert(Stage::Ao, key(1));
        store.set_current(&current);
        store.insert(key(1), scalar());
        store.insert(key(2), scalar());
        store.insert(key(3), scalar());
        assert!(store.get(&key(1)).is_some(), "current key must survive");
        assert!(store.bytes() <= store.budget() || store.len() == 1);
    }

    #[test]
    fn cached_for_only_returns_matching_keys() {
        let mut store = MapStore::with_budget(1024 * 1024);
        store.insert(key(4), scalar());
        let mut keys = HashMap::new();
        keys.insert(Stage::Ao, key(4));
        keys.insert(Stage::Cavity, key(5));
        let cached = store.cached_for(&keys);
        assert_eq!(cached.len(), 1);
        assert!(cached.contains_key(&key(4)));
    }
}
