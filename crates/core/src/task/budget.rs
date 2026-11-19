use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

type Key = (String, usize);

pub(crate) struct Budget {
    state: Mutex<State>,
}
struct State {
    limit: usize,
    used: usize,
    hashing: bool,
    order: BTreeMap<Key, u64>,
    waiting: BTreeMap<u64, Key>,
    next: u64,
}
pub(crate) struct Slot {
    budget: Arc<Budget>,
    hash: bool,
}
impl Budget {
    pub(crate) fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                limit,
                used: 0,
                hashing: false,
                order: BTreeMap::new(),
                waiting: BTreeMap::new(),
                next: 0,
            }),
        })
    }
    pub(crate) fn limit(&self, limit: usize) {
        self.state.lock().expect("slot budget").limit = limit;
    }
    pub(crate) fn usage(&self) -> (usize, usize, bool) {
        let state = self.state.lock().expect("slot budget");
        (state.used, state.limit, state.hashing)
    }
    pub(crate) fn register(&self, task: &str, indices: impl IntoIterator<Item = usize>) {
        let mut state = self.state.lock().expect("slot budget");
        for index in indices {
            let key = (task.to_owned(), index);
            let order = if let Some(order) = state.order.get(&key) {
                *order
            } else {
                state.next += 1;
                let next = state.next;
                state.order.insert(key.clone(), next);
                next
            };
            state.waiting.insert(order, key);
        }
    }
    pub(crate) fn suspend(&self, task: &str) {
        self.state
            .lock()
            .expect("slot budget")
            .waiting
            .retain(|_, key| key.0 != task);
    }
    pub(crate) fn forget(&self, task: &str) {
        let mut state = self.state.lock().expect("slot budget");
        state.waiting.retain(|_, key| key.0 != task);
        state.order.retain(|key, _| key.0 != task);
    }
    pub(crate) fn renew(&self, task: &str, indices: &[usize]) {
        self.discard(task, indices);
        self.register(task, indices.iter().copied());
    }
    pub(crate) fn discard(&self, task: &str, indices: &[usize]) {
        let mut state = self.state.lock().expect("slot budget");
        state
            .waiting
            .retain(|_, key| key.0 != task || !indices.contains(&key.1));
        state
            .order
            .retain(|key, _| key.0 != task || !indices.contains(&key.1));
    }
    pub(crate) fn download(self: &Arc<Self>, task: &str, index: usize) -> Option<Slot> {
        let mut state = self.state.lock().expect("slot budget");
        if state.used >= state.limit {
            return None;
        }
        let (&order, key) = state.waiting.first_key_value()?;
        if key.0 != task || key.1 != index {
            return None;
        }
        state.waiting.remove(&order);
        state.used += 1;
        Some(Slot {
            budget: self.clone(),
            hash: false,
        })
    }
    pub(crate) fn hash(self: &Arc<Self>) -> Option<Slot> {
        let mut state = self.state.lock().expect("slot budget");
        if state.used >= state.limit || state.hashing {
            return None;
        }
        state.used += 1;
        state.hashing = true;
        Some(Slot {
            budget: self.clone(),
            hash: true,
        })
    }
    pub(crate) fn order(&self) -> Vec<(String, usize, u64)> {
        self.state
            .lock()
            .expect("slot budget")
            .order
            .iter()
            .map(|((id, i), order)| (id.clone(), *i, *order))
            .collect()
    }
    pub(crate) fn restore(&self, mut order: Vec<(String, usize, u64)>) {
        let mut state = self.state.lock().expect("slot budget");
        order.sort_by_key(|v| v.2);
        for (id, index, _) in order {
            if !state.order.contains_key(&(id.clone(), index)) {
                state.next += 1;
                let next = state.next;
                state.order.insert((id, index), next);
            }
        }
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        let mut state = self.budget.state.lock().expect("slot budget");
        state.used -= 1;
        if self.hash {
            state.hashing = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fifo_append_and_hash_share_limit_and_survive_restore() {
        let budget = Budget::new(2);
        budget.register("A", [0]);
        budget.register("B", [0]);
        budget.register("A", [1]);
        assert!(budget.download("B", 0).is_none());
        let a = budget.download("A", 0).unwrap();
        let hash = budget.hash().unwrap();
        assert!(budget.hash().is_none());
        assert!(budget.download("B", 0).is_none());
        drop(hash);
        let b = budget.download("B", 0).unwrap();
        assert!(budget.download("A", 1).is_none());
        drop(a);
        let append = budget.download("A", 1).unwrap();
        assert_eq!(budget.usage(), (2, 2, false));
        drop((b, append));
        let restored = Budget::new(1);
        restored.restore(vec![("B".into(), 0, u64::MAX), ("A".into(), 0, 5)]);
        restored.register("B", [0]);
        restored.register("A", [0]);
        assert!(restored.download("B", 0).is_none());
        drop(restored.download("A", 0).unwrap());
        drop(restored.download("B", 0).unwrap());
        restored.register("C", [0]);
        assert!(restored.download("C", 0).is_some());
    }
}
