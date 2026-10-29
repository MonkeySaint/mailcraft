//! `MetaCache` (storage ADR §3.3): the least recently used mail items read from `store.db`, as shared
//! `Arc<MailItem>`, capped in bytes.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use mailcraft_model::{ItemId, MailItem};

/// Default byte cap (about 20k items).
pub const DEFAULT_CACHE_BYTES: usize = 32 << 20;

#[derive(Debug, Default)]
pub struct MetaCache {
    cap: usize,
    used: usize,
    tick: u64,
    map: HashMap<ItemId, (Arc<MailItem>, usize, u64)>,
    order: BTreeMap<u64, ItemId>,
    pub hits: u64,
    pub misses: u64,
}

impl MetaCache {
    pub fn new(cap: usize) -> MetaCache {
        MetaCache { cap, ..Default::default() }
    }

    pub fn get(&mut self, id: ItemId) -> Option<Arc<MailItem>> {
        let Some(entry) = self.map.get_mut(&id) else {
            self.misses = self.misses.saturating_add(1);
            return None;
        };
        self.hits = self.hits.saturating_add(1);
        self.tick = self.tick.wrapping_add(1);
        self.order.remove(&entry.2);
        entry.2 = self.tick;
        self.order.insert(self.tick, id);
        Some(Arc::clone(&entry.0))
    }

    pub fn put(&mut self, item: &Arc<MailItem>) {
        let id = item.id;
        self.remove(id);
        let cost = item.approx_bytes().saturating_add(64);
        if cost > self.cap {
            return;
        }
        while self.used.saturating_add(cost) > self.cap {
            let Some((_, oldest)) = self.order.pop_first() else { break };
            if let Some((_, c, _)) = self.map.remove(&oldest) {
                self.used = self.used.saturating_sub(c);
            }
        }
        self.tick = self.tick.wrapping_add(1);
        self.order.insert(self.tick, id);
        self.map.insert(id, (Arc::clone(item), cost, self.tick));
        self.used = self.used.saturating_add(cost);
    }

    pub fn remove(&mut self, id: ItemId) {
        if let Some((_, c, t)) = self.map.remove(&id) {
            self.order.remove(&t);
            self.used = self.used.saturating_sub(c);
        }
    }

    pub fn clear(&mut self) {
        let cap = self.cap;
        *self = MetaCache::new(cap);
    }

    pub fn entries(&self) -> usize {
        self.map.len()
    }

    pub fn bytes(&self) -> usize {
        self.used
    }
}
