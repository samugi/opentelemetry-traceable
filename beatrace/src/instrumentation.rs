//! Independently-configured tracing *instrumentations* over the same set of
//! `#[traceable]` functions.
//!
//! An [`Instrumentation`] is the only way to turn tracing on: there is no
//! global or default instrumentation, and no process-wide tracer. Each one owns
//! its enabled subset, its own isolated span hierarchy built from the same
//! underlying call flow, and its own [`Tracer`] (potentially a different
//! backend). Up to [`MAX_INSTRUMENTATIONS`] may be live at once.
//!
//! ```ignore
//! let provider = /* some SdkTracerProvider */;
//! let checkout = beatrace::instrumentation::Instrumentation::builder()
//!     .name("checkout-debug")
//!     .tracer(provider.tracer("checkout-debug"))
//!     .build()
//!     .expect("a free instrumentation slot");
//! checkout.enable_encoded(&encoded)?;
//! // ... dropping `checkout` stops new spans for it and frees its slot for reuse.
//! ```
//!
//! # How isolation works
//!
//! Each `#[traceable]` site carries a `u64` enabled bitmask (one bit per slot).
//! The macro loads it once per call: `0` means nobody is tracing (the
//! single-atomic-load fast path), and anything else routes through
//! [`start_spans`], which builds one child span *per active slot* using that
//! slot's own tracer and parent. The per-slot parents ride together inside one
//! `opentelemetry::Context` extension so async propagation stays O(1) -- there
//! is exactly one ambient context per thread, so the currently-active per-slot
//! spans must share one propagation envelope rather than N independent ones.
//!
//! # Limitation: in-process only
//!
//! Instrumentations never read or write `opentelemetry::Context`'s single
//! "current span" slot -- their spans live exclusively in that extension
//! envelope. Two consequences, both deliberate:
//!
//! * An incoming `traceparent` is **not** joined. A propagator deposits the
//!   remote parent in `Context`'s span slot, which no instrumentation consults,
//!   so the first traced function on a call path always roots a fresh trace.
//!   By the same token, a span created outside beatrace -- a web framework's
//!   server span, a hand-rolled `tracer.start()` -- is never a parent either.
//! * Outbound `traceparent` headers carry nothing from beatrace, since
//!   propagators inject whatever is in that same span slot.
//!
//! An instrumentation's spans nest only under other `#[traceable]` spans of
//! that same instrumentation.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use arc_swap::ArcSwap;
use opentelemetry::trace::{SpanBuilder, TraceContextExt, Tracer};
use opentelemetry::{Context, KeyValue};
use smallvec::SmallVec;

use crate::codec::{self, DecodeError};
use crate::registry::{self, REGISTRY, TraceSite};

/// Number of [`Instrumentation`]s that can be live simultaneously -- one bit per
/// slot in each site's `u64` masks. Slots are released on `Drop` and reused, so
/// this bounds concurrent instrumentations, not how many may be created over the
/// process lifetime.
pub const MAX_INSTRUMENTATIONS: u32 = 64;

// ---------------------------------------------------------------------------
// Slot-scoped bit operations backing each `Instrumentation`'s own slot. Each
// walks the full `REGISTRY` once -- meant to be called rarely (config/reload
// events), never on a hot path.
// ---------------------------------------------------------------------------

#[inline]
fn bit(slot: u8) -> u64 {
    1u64 << slot
}

/// How a name/id match updates a slot's bit.
#[derive(Clone, Copy)]
enum BitOp {
    /// Set the bit on matches, clear it on non-matches (whole-set replace).
    Replace,
    /// Set the bit on matches only, leave non-matches untouched (additive).
    Add,
    /// Clear the bit on matches only, leave non-matches untouched.
    Remove,
}

fn apply_encoded(
    encoded: &str,
    field: impl Fn(&TraceSite) -> &AtomicU64,
    slot: u8,
    op: BitOp,
) -> Result<(), DecodeError> {
    // Decoded ids index the sorted set of registry keys (see
    // `registry::names_by_id`), never a raw `REGISTRY` position; membership is a
    // direct positional lookup -- no hashing, no false positives. Iterating the
    // grouped view means a key shared by two call sites flips both.
    let wanted: HashSet<u64> = codec::decode(encoded)?.into_iter().collect();
    let b = bit(slot);
    for (id, sites) in registry::sites_by_id().iter().enumerate() {
        let hit_wanted = wanted.contains(&(id as u64));
        for site in sites {
            match (op, hit_wanted) {
                (BitOp::Replace | BitOp::Add, true) => {
                    field(site).fetch_or(b, Ordering::Relaxed);
                }
                (BitOp::Replace, false) | (BitOp::Remove, true) => {
                    field(site).fetch_and(!b, Ordering::Relaxed);
                }
                (BitOp::Add, false) | (BitOp::Remove, false) => {}
            }
        }
    }
    Ok(())
}

pub(crate) fn slot_enable_all(slot: u8) {
    let b = bit(slot);
    for site in REGISTRY.iter() {
        site.enabled_mask.fetch_or(b, Ordering::Relaxed);
    }
}

pub(crate) fn slot_disable_all(slot: u8) {
    let b = bit(slot);
    for site in REGISTRY.iter() {
        site.enabled_mask.fetch_and(!b, Ordering::Relaxed);
    }
}

pub(crate) fn slot_set_enabled_encoded(encoded: &str, slot: u8) -> Result<(), DecodeError> {
    apply_encoded(encoded, |s| &s.enabled_mask, slot, BitOp::Replace)
}

pub(crate) fn slot_enable_encoded(encoded: &str, slot: u8) -> Result<(), DecodeError> {
    apply_encoded(encoded, |s| &s.enabled_mask, slot, BitOp::Add)
}

pub(crate) fn slot_disable_encoded(encoded: &str, slot: u8) -> Result<(), DecodeError> {
    apply_encoded(encoded, |s| &s.enabled_mask, slot, BitOp::Remove)
}

// Both name listings walk the stable id order, so what they yield is ordered by
// id (and thus reproducible) rather than by whatever order the linker chose.
// One key yields one name even when several call sites carry it.
pub(crate) fn slot_enabled_names(slot: u8) -> impl Iterator<Item = &'static str> {
    let b = bit(slot);
    registry::names_by_id()
        .iter()
        .zip(registry::sites_by_id())
        .filter(move |(_, sites)| {
            sites
                .iter()
                .any(|s| s.enabled_mask.load(Ordering::Relaxed) & b != 0)
        })
        .map(|(name, _)| *name)
}

pub(crate) fn slot_set_child_only_encoded(encoded: &str, slot: u8) -> Result<(), DecodeError> {
    apply_encoded(encoded, |s| &s.child_only_mask, slot, BitOp::Replace)
}

pub(crate) fn slot_child_only_names(slot: u8) -> impl Iterator<Item = &'static str> {
    let b = bit(slot);
    registry::names_by_id()
        .iter()
        .zip(registry::sites_by_id())
        .filter(move |(_, sites)| {
            sites
                .iter()
                .any(|s| s.child_only_mask.load(Ordering::Relaxed) & b != 0)
        })
        .map(|(name, _)| *name)
}

// ---------------------------------------------------------------------------
// Type-erased tracers + the slot table.
// ---------------------------------------------------------------------------

/// Object-safe view of a [`Tracer`]: start a child span (from `builder`,
/// parented to `parent`) and return a new `Context` whose current span is that
/// child. This is how a concrete tracer of arbitrary span type is stored
/// per-slot without naming its `Span` type -- the span is erased into the
/// returned `Context` via `with_span`.
trait DynTracer: Send + Sync {
    fn start_in(&self, builder: SpanBuilder, parent: &Context) -> Context;
}

impl<T> DynTracer for T
where
    T: Tracer + Send + Sync,
    T::Span: Send + Sync + 'static,
{
    fn start_in(&self, builder: SpanBuilder, parent: &Context) -> Context {
        let span = self.build_with_context(builder, parent);
        parent.with_span(span)
    }
}

type SlotTable = [Option<Arc<dyn DynTracer>>; MAX_INSTRUMENTATIONS as usize];

/// Lock-free-readable table of the named slots' tracers. Read once per
/// multi-slot call (a single atomic pointer load); swapped wholesale on the
/// rare create/drop of an instrumentation.
static SLOT_TABLE: LazyLock<ArcSwap<SlotTable>> =
    LazyLock::new(|| ArcSwap::from_pointee(std::array::from_fn(|_| None)));

/// Slot allocator: bump `next` through `0..MAX_INSTRUMENTATIONS` for fresh
/// slots, then recycle whatever `Drop` has released into `freed`. So the cap is
/// on *concurrently live* instrumentations, not on how many are created over the
/// process lifetime -- which matters for hot-reload, where an instrumentation is
/// torn down and rebuilt every time its identity (tracer/endpoint) changes.
///
/// Reuse leaves one narrow race. `Drop` clears its bit across many sites
/// non-atomically, and a `#[traceable]` call reads the site's mask (in the
/// macro) before reading [`SLOT_TABLE`] (in [`start_spans`]). A thread
/// descheduled between those two reads, across an entire drop *and* rebuild,
/// would find the new occupant's tracer behind a bit the old occupant set, and
/// emit one span into the wrong instrumentation. Closing it properly needs
/// epoch-based reclamation before a slot is released; the cost of losing the
/// race is a single mis-attributed span during a reload, which isn't worth that
/// machinery or the hot-path validation it would add.
struct SlotAlloc {
    next: u8,
    freed: Vec<u8>,
}

static SLOTS: Mutex<SlotAlloc> = Mutex::new(SlotAlloc {
    next: 0,
    freed: Vec::new(),
});

// ---------------------------------------------------------------------------
// The per-call multi-slot state carried inside `Context`.
// ---------------------------------------------------------------------------

/// The currently-active named slots' parent contexts, carried inside one
/// `opentelemetry::Context` extension. Sparse: only slots active on this call
/// path appear, so cloning it costs the active count, not the slot capacity.
#[derive(Clone)]
struct MultiInstrumentState(SmallVec<[(u8, Context); 4]>);

fn span_builder(name: &'static str, attrs: &[KeyValue]) -> SpanBuilder {
    if attrs.is_empty() {
        SpanBuilder::from_name(name)
    } else {
        SpanBuilder::from_name(name).with_attributes(attrs.to_vec())
    }
}

/// Build one child span per active slot in `enabled_mask` and return the new
/// `Context` to attach for the wrapped call, or `None` if no span was created --
/// every active slot suppressed by child-only mode, or its instrumentation
/// dropped mid-flight -- in which case the caller runs the body with no context
/// machinery at all.
///
/// Not part of the stable API -- called by the `#[traceable]` macro whenever the
/// mask is non-zero.
///
/// Every span goes into the [`MultiInstrumentState`] envelope; the ambient
/// `Context`'s own span slot is neither read nor written, so a slot's parent is
/// strictly its own previous span on this call path (see the module's
/// in-process-only limitation).
#[doc(hidden)]
pub fn start_spans(
    enabled_mask: u64,
    child_only_mask: u64,
    span_name: &'static str,
    attrs: Vec<KeyValue>,
) -> Option<Context> {
    let cur = Context::current();
    let mut env: SmallVec<[(u8, Context); 4]> = cur
        .get::<MultiInstrumentState>()
        .map(|s| s.0.clone())
        .unwrap_or_default();

    let table = SLOT_TABLE.load();
    let mut created_any = false;

    let mut bits = enabled_mask;
    while bits != 0 {
        let slot = bits.trailing_zeros() as u8;
        bits &= bits - 1;

        let parent = env.iter().find(|(s, _)| *s == slot).map(|(_, c)| c);
        // Child-only: only trace when this slot already has a recording span on
        // this call path, so a shared helper never orphans a root span on the
        // paths that aren't being traced.
        if child_only_mask & bit(slot) != 0 && !parent.is_some_and(|c| c.span().is_recording()) {
            continue;
        }
        let Some(tracer) = table[slot as usize].as_ref() else {
            // Instrumentation was dropped mid-flight; just skip its slot.
            continue;
        };
        let base = parent.cloned().unwrap_or_default();
        let child = tracer.start_in(span_builder(span_name, &attrs), &base);
        match env.iter_mut().find(|(s, _)| *s == slot) {
            Some(entry) => entry.1 = child,
            None => env.push((slot, child)),
        }
        created_any = true;
    }

    if !created_any {
        return None;
    }

    // One physical context to attach for the whole call, carrying every active
    // slot's new tip. Inserted straight onto `cur` (which we still own) rather
    // than cloning it first -- `with_value` clones `entries` itself.
    Some(cur.with_value(MultiInstrumentState(env)))
}

// ---------------------------------------------------------------------------
// Public handle + builder.
// ---------------------------------------------------------------------------

/// Returned by [`InstrumentationBuilder::build`] when [`MAX_INSTRUMENTATIONS`]
/// are already live. Dropping any of them frees its slot for reuse.
#[derive(Debug)]
pub struct SlotsExhausted;

impl std::fmt::Display for SlotsExhausted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "no free instrumentation slot ({MAX_INSTRUMENTATIONS} already live)"
        )
    }
}

impl std::error::Error for SlotsExhausted {}

/// A dynamically-created, independently-configured tracing instrumentation --
/// the only way to enable tracing for a `#[traceable]` function.
///
/// Its span tree, tracer, and enabled set are isolated from every other
/// instrumentation. Dropping the handle stops new spans for it and releases its
/// slot for reuse; spans already in flight finish and export normally.
#[derive(Debug)]
pub struct Instrumentation {
    slot: u8,
}

/// Builder for an [`Instrumentation`]; obtain one via [`Instrumentation::builder`].
#[derive(Default)]
pub struct InstrumentationBuilder {
    name: Option<String>,
    tracer: Option<Arc<dyn DynTracer>>,
}

impl std::fmt::Debug for InstrumentationBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstrumentationBuilder")
            .field("name", &self.name)
            .field("tracer", &self.tracer.as_ref().map(|_| "<tracer>"))
            .finish()
    }
}

impl InstrumentationBuilder {
    /// Attach an optional human-readable name (diagnostic only).
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set the [`Tracer`] this instrumentation's spans are created with -- e.g.
    /// one obtained from a dedicated `SdkTracerProvider` pointing at its own
    /// backend. Required.
    #[must_use]
    pub fn tracer<T>(mut self, tracer: T) -> Self
    where
        T: Tracer + Send + Sync + 'static,
        T::Span: Send + Sync + 'static,
    {
        self.tracer = Some(Arc::new(tracer));
        self
    }

    /// Allocate a slot and register the tracer, returning a live handle.
    ///
    /// # Panics
    ///
    /// Panics if no tracer was set via [`tracer`](Self::tracer).
    pub fn build(self) -> Result<Instrumentation, SlotsExhausted> {
        let tracer = self
            .tracer
            .expect("InstrumentationBuilder::build requires a tracer");

        let mut slots = SLOTS.lock().expect("beatrace slot allocator poisoned");
        let slot = if let Some(slot) = slots.freed.pop() {
            slot
        } else if u32::from(slots.next) < MAX_INSTRUMENTATIONS {
            let slot = slots.next;
            slots.next += 1;
            slot
        } else {
            return Err(SlotsExhausted);
        };
        drop(slots);

        // Publish the tracer before returning the handle, so any later
        // `enable` on this slot is guaranteed to find it.
        SLOT_TABLE.rcu(|old| {
            let mut new: SlotTable = std::array::from_fn(|i| (**old)[i].clone());
            new[slot as usize] = Some(tracer.clone());
            new
        });

        Ok(Instrumentation { slot })
    }
}

impl Instrumentation {
    /// Start building a new instrumentation.
    #[must_use]
    pub fn builder() -> InstrumentationBuilder {
        InstrumentationBuilder::default()
    }

    /// Enable every known `#[traceable]` function for this instrumentation.
    pub fn enable_all(&self) {
        slot_enable_all(self.slot);
    }

    /// Disable every function for this instrumentation.
    pub fn disable_all(&self) {
        slot_disable_all(self.slot);
    }

    /// Replace this instrumentation's enabled set from a compact id list
    /// produced by [`crate::codec::encode`]: exactly the decoded ids are
    /// enabled for it, everything else is disabled.
    pub fn set_enabled_encoded(&self, encoded: &str) -> Result<(), DecodeError> {
        slot_set_enabled_encoded(encoded, self.slot)
    }

    /// Enable whatever's in the encoded id list (additive).
    pub fn enable_encoded(&self, encoded: &str) -> Result<(), DecodeError> {
        slot_enable_encoded(encoded, self.slot)
    }

    /// Disable whatever's in the encoded id list.
    pub fn disable_encoded(&self, encoded: &str) -> Result<(), DecodeError> {
        slot_disable_encoded(encoded, self.slot)
    }

    /// Replace this instrumentation's *child-only* set from a compact id list
    /// produced by [`crate::codec::encode`]: exactly the decoded ids are put in
    /// child-only mode for it, every other function is made root-capable again.
    ///
    /// A child-only function only produces a span when this instrumentation
    /// already has a recording span on the current call path -- never as a root,
    /// even when enabled. This is orthogonal to
    /// [`set_enabled_encoded`](Self::set_enabled_encoded): a function must be
    /// enabled to trace at all, and being child-only additionally suppresses it
    /// when it would otherwise be a root. Whether a shared function should be
    /// child-only is request-relative, so it's decided here rather than at the
    /// call site.
    pub fn set_child_only_encoded(&self, encoded: &str) -> Result<(), DecodeError> {
        slot_set_child_only_encoded(encoded, self.slot)
    }

    /// Registry keys currently enabled for this instrumentation.
    pub fn enabled_names(&self) -> impl Iterator<Item = &'static str> {
        slot_enabled_names(self.slot)
    }

    /// Registry keys currently in child-only mode for this instrumentation.
    pub fn child_only_names(&self) -> impl Iterator<Item = &'static str> {
        slot_child_only_names(self.slot)
    }
}

impl Drop for Instrumentation {
    fn drop(&mut self) {
        // Clear this slot's bits everywhere first (stops new spans), then drop
        // the tracer -- so no thread can see a set bit with a missing tracer.
        let b = bit(self.slot);
        for site in REGISTRY.iter() {
            site.enabled_mask.fetch_and(!b, Ordering::Relaxed);
            site.child_only_mask.fetch_and(!b, Ordering::Relaxed);
        }
        let slot = self.slot;
        SLOT_TABLE.rcu(|old| {
            let mut new: SlotTable = std::array::from_fn(|i| (**old)[i].clone());
            new[slot as usize] = None;
            new
        });
        // Release the slot only after its bits are clear and its tracer is gone,
        // so the next occupant can't be reached through this one's leftovers.
        SLOTS
            .lock()
            .expect("beatrace slot allocator poisoned")
            .freed
            .push(slot);
    }
}
