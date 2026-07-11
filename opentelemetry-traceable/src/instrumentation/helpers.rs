use std::sync::atomic::Ordering;

use crate::{registry::REGISTRY, selector::Selection};

#[inline]
pub fn bit(slot: u8) -> u64 {
    1u64 << slot
}

/// How a name match updates a slot bit.
#[derive(Clone, Copy)]
pub enum BitOp {
    /// Set the bit on matches, clear it on non-matches
    Replace,
    /// Set the bit on matches
    Add,
    /// Clear the bit on matches
    Remove,
}

/// Applies an already-resolved [`Selection`] (infallible)
pub fn apply(selection: &Selection, slot: u8, op: BitOp) {
    let b = bit(slot);
    // Walk the registry, match string keys. `selection.keys` is
    // sorted, so we use binary search.
    for site in REGISTRY.iter() {
        let hit = selection.keys.binary_search(&site.name).is_ok();
        match (op, hit) {
            (BitOp::Replace | BitOp::Add, true) => {
                site.enabled_slots.fetch_or(b, Ordering::Relaxed);
            }
            (BitOp::Replace, false) | (BitOp::Remove, true) => {
                site.enabled_slots.fetch_and(!b, Ordering::Relaxed);
            }
            (BitOp::Add, false) | (BitOp::Remove, false) => {}
        }
    }
}

/// Keys enabled for `slot`
pub fn slot_names(slot: u8) -> impl Iterator<Item = &'static str> {
    let b = bit(slot);
    let mut names: Vec<&'static str> = REGISTRY
        .iter()
        .filter(|site| site.enabled_slots.load(Ordering::Relaxed) & b != 0)
        .map(|site| site.name)
        .collect();
    names.sort_unstable();
    // dedup because one key can carry multiple call sites
    names.dedup();
    names.into_iter()
}
