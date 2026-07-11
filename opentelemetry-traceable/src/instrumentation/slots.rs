use std::sync::{Arc, LazyLock, Mutex};

use arc_swap::ArcSwap;

use crate::instrumentation::{MAX_INSTRUMENTATIONS, tracer::DynTracer};

#[derive(Clone)]
pub struct Slots {
    pub tracers: [Option<Arc<dyn DynTracer>>; MAX_INSTRUMENTATIONS as usize],
    /// The slot held by the live distributed instrumentation
    pub distributed: Option<u8>,
    pub next: u8,
    pub freed: Vec<u8>,
}

impl Default for Slots {
    fn default() -> Self {
        Self {
            tracers: std::array::from_fn(|_| None),
            distributed: None,
            next: 0,
            freed: Vec::new(),
        }
    }
}

impl Slots {
    /// Takes the next free slot, or `None` when we reached [`MAX_INSTRUMENTATIONS`]
    pub fn alloc(&mut self) -> Option<u8> {
        if let Some(slot) = self.freed.pop() {
            return Some(slot);
        }
        if u32::from(self.next) < MAX_INSTRUMENTATIONS {
            let slot = self.next;
            self.next += 1;
            return Some(slot);
        }
        None
    }
}

/// Lock free (read) slot state. ArcSwap allows atomic swap on the (rare)
/// create/drop of an instrumentation.
/// We don't use RwLock here because reads are the hot path and ArcSwap is lock free + wait free
pub static SLOTS: LazyLock<ArcSwap<Slots>> =
    LazyLock::new(|| ArcSwap::from_pointee(Slots::default()));

/// Write lock, so that writes happen sequentially
static SLOTS_WRITE: Mutex<()> = Mutex::new(());

pub fn update_slots<T, E>(f: impl FnOnce(&mut Slots) -> Result<T, E>) -> Result<T, E> {
    let _guard = SLOTS_WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let mut next = (**SLOTS.load()).clone();
    let out = f(&mut next)?;
    SLOTS.store(Arc::new(next));
    Ok(out)
}
