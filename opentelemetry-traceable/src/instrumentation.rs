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
//! let checkout = opentelemetry_traceable::instrumentation::Instrumentation::builder()
//!     .name("checkout-debug")
//!     .tracer(provider.tracer("checkout-debug"))
//!     .build()
//!     .expect("a free instrumentation slot");
//! checkout.enable(&["my_crate::checkout::*"])?;
//! // ... dropping `checkout` stops new spans for it and frees its slot for reuse.
//! ```
//!
//! # How isolation works
//!
//! Each `#[traceable]` site carries an enabled bitmask, one bit per slot.
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
//!   By the same token, a span created outside opentelemetry-traceable -- a web framework's
//!   server span, a hand-rolled `tracer.start()` -- is never a parent either.
//! * Outbound `traceparent` headers carry nothing from opentelemetry-traceable, since
//!   propagators inject whatever is in that same span slot.
//!
//! An instrumentation's spans nest only under other `#[traceable]` spans of
//! that same instrumentation.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use arc_swap::ArcSwap;
use opentelemetry::trace::{SpanBuilder, TraceContextExt, Tracer};
use opentelemetry::{Context, KeyValue};
use smallvec::SmallVec;

use crate::registry::{REGISTRY, TraceSite};
use crate::selector::{self, Selection, UnknownKeys};

/// Number of [`Instrumentation`]s that can be live simultaneously -- one bit per
/// slot in each site's mask. Slots are released on `Drop` and reused, so
/// this bounds concurrent instrumentations, not how many may be created over the
/// process lifetime.
pub const MAX_INSTRUMENTATIONS: u32 = 64;

const _: () = assert!(
    MAX_INSTRUMENTATIONS as usize == u64::BITS as usize,
    "MAX_INSTRUMENTATIONS must equal the number of bits in a site's mask"
);

// ---------------------------------------------------------------------------
// Shared helpers for reading and writing one slot's bit across every site. Each
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

/// Applies an already-resolved [`Selection`], so this can't fail: resolution --
/// the only fallible half -- happened before any bit was touched, which is what
/// makes a typo leave tracing state completely untouched.
fn apply(selection: &Selection, field: impl Fn(&TraceSite) -> &AtomicU64, slot: u8, op: BitOp) {
    let b = bit(slot);
    // One flat walk over the registry, matching on the key string. Two call sites
    // sharing a key both match it, so a key with several sites toggles all of them
    // for free -- there's no grouped view to keep aligned. `selection.keys` is
    // sorted, so membership is a binary search.
    for site in REGISTRY.iter() {
        let hit = selection.keys.binary_search(&site.name).is_ok();
        match (op, hit) {
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

/// The keys whose `field` mask has `slot`'s bit set.
///
/// Sorted and deduped explicitly. There's no id order left to inherit, so without
/// this the linker's arbitrary `REGISTRY` order would leak into what callers see;
/// dedup because one key can carry several call sites but is one selectable thing.
fn slot_names(
    slot: u8,
    field: impl Fn(&TraceSite) -> &AtomicU64,
) -> impl Iterator<Item = &'static str> {
    let b = bit(slot);
    let mut names: Vec<&'static str> = REGISTRY
        .iter()
        .filter(|site| field(site).load(Ordering::Relaxed) & b != 0)
        .map(|site| site.name)
        .collect();
    names.sort_unstable();
    names.dedup();
    names.into_iter()
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

/// Everything the slot layer owns: which slot holds which tracer, and which slot
/// numbers are in play. Both live here so every change is one [`ArcSwap`] publish.
///
/// `next` bumps through `0..MAX_INSTRUMENTATIONS` for fresh slots, then `freed`
/// is recycled. So the cap is on *concurrently live* instrumentations, not on how
/// many are created over the process lifetime -- which matters for hot-reload,
/// where an instrumentation is torn down and rebuilt every time its identity
/// (tracer/endpoint) changes.
///
/// Slot reuse leaves one narrow race -- a span mis-attributed to the wrong
/// instrumentation during a reload -- described in full at
/// `docs/multi-instrumentation.md`.
#[derive(Clone)]
struct Slots {
    tracers: [Option<Arc<dyn DynTracer>>; MAX_INSTRUMENTATIONS as usize],
    next: u8,
    freed: Vec<u8>,
}

impl Slots {
    fn empty() -> Self {
        Self {
            tracers: std::array::from_fn(|_| None),
            next: 0,
            freed: Vec::new(),
        }
    }

    /// Takes the next free slot, or `None` when [`MAX_INSTRUMENTATIONS`] are live.
    fn alloc(&mut self) -> Option<u8> {
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

/// Lock-free-readable slot state. Read once per multi-slot call (a single atomic
/// pointer load); swapped wholesale on the rare create/drop of an instrumentation.
static SLOTS: LazyLock<ArcSwap<Slots>> = LazyLock::new(|| ArcSwap::from_pointee(Slots::empty()));

/// Serializes writers so slot allocation is linearizable. Readers never take it.
///
/// This is why mutations don't use [`ArcSwap::rcu`]: `rcu` re-runs its closure when
/// it loses the swap race, and allocating a slot is not idempotent -- a retry would
/// hand out a second slot and leak the first.
static SLOTS_WRITE: Mutex<()> = Mutex::new(());

/// Read-modify-publish the slot state under [`SLOTS_WRITE`]. `f` runs exactly
/// once, so it may allocate; returning `None` abandons the change and publishes
/// nothing.
///
/// Poisoning is tolerated rather than propagated: the guarded data lives in the
/// `ArcSwap`, not in the mutex, so a writer that panicked left the last
/// *published* snapshot intact and there is nothing to recover.
fn update_slots<R>(f: impl FnOnce(&mut Slots) -> Option<R>) -> Option<R> {
    let _guard = SLOTS_WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let mut next = (**SLOTS.load()).clone();
    let out = f(&mut next)?;
    SLOTS.store(Arc::new(next));
    Some(out)
}

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

    // `load_full`, not `load`: an arc-swap guard held across span construction
    // would make `ArcSwap::store` -- i.e. a config reload -- wait on sampler and
    // exporter latency, and nested traced calls would exhaust arc-swap's small
    // per-thread borrow pool. An owned `Arc` costs one refcount bump and neither.
    let slots = SLOTS.load_full();
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
        let Some(tracer) = slots.tracers[slot as usize].as_ref() else {
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

        // One publish: the slot is taken and its tracer installed together, so any
        // later `enable` on this slot is guaranteed to find the tracer.
        let slot = update_slots(|slots| {
            let slot = slots.alloc()?;
            slots.tracers[slot as usize] = Some(tracer);
            Some(slot)
        })
        .ok_or(SlotsExhausted)?;

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
        let b = bit(self.slot);
        for site in REGISTRY.iter() {
            site.enabled_mask.fetch_or(b, Ordering::Relaxed);
        }
    }

    /// Disable every function for this instrumentation.
    pub fn disable_all(&self) {
        let b = bit(self.slot);
        for site in REGISTRY.iter() {
            site.enabled_mask.fetch_and(!b, Ordering::Relaxed);
        }
    }

    /// Replace this instrumentation's enabled set: exactly what `selectors`
    /// resolves to is enabled for it, everything else disabled.
    ///
    /// `selectors` are registry keys, `*` globs, or a mix -- see
    /// [`crate::selector`] for the matching rules. An empty list disables
    /// everything, the same as [`disable_all`](Self::disable_all). The returned
    /// [`Selection`] is what actually matched, including any globs that matched
    /// nothing.
    ///
    /// # Errors
    ///
    /// [`UnknownKeys`] if an exact selector names no `#[traceable]` function. In
    /// that case **nothing is applied** and this instrumentation keeps whatever
    /// set it already had, so a typo can't silently trace a subset of what was
    /// asked for.
    pub fn set_enabled<S: AsRef<str>>(&self, selectors: &[S]) -> Result<Selection, UnknownKeys> {
        let selection = selector::resolve(selectors)?;
        apply(&selection, |s| &s.enabled_mask, self.slot, BitOp::Replace);
        Ok(selection)
    }

    /// Enable whatever `selectors` resolves to, leaving the rest of the enabled
    /// set alone (additive).
    ///
    /// # Errors
    ///
    /// As [`set_enabled`](Self::set_enabled).
    pub fn enable<S: AsRef<str>>(&self, selectors: &[S]) -> Result<Selection, UnknownKeys> {
        let selection = selector::resolve(selectors)?;
        apply(&selection, |s| &s.enabled_mask, self.slot, BitOp::Add);
        Ok(selection)
    }

    /// Disable whatever `selectors` resolves to, leaving the rest of the enabled
    /// set alone.
    ///
    /// # Errors
    ///
    /// As [`set_enabled`](Self::set_enabled).
    pub fn disable<S: AsRef<str>>(&self, selectors: &[S]) -> Result<Selection, UnknownKeys> {
        let selection = selector::resolve(selectors)?;
        apply(&selection, |s| &s.enabled_mask, self.slot, BitOp::Remove);
        Ok(selection)
    }

    /// Replace this instrumentation's *child-only* set: exactly what `selectors`
    /// resolves to is put in child-only mode for it, every other function made
    /// root-capable again.
    ///
    /// A child-only function only produces a span when this instrumentation
    /// already has a recording span on the current call path -- never as a root,
    /// even when enabled. This is orthogonal to
    /// [`set_enabled`](Self::set_enabled): a function must be enabled to trace at
    /// all, and being child-only additionally suppresses it when it would
    /// otherwise be a root. Whether a shared function should be child-only is
    /// request-relative, so it's decided here rather than at the call site.
    ///
    /// # Errors
    ///
    /// As [`set_enabled`](Self::set_enabled).
    pub fn set_child_only<S: AsRef<str>>(&self, selectors: &[S]) -> Result<Selection, UnknownKeys> {
        let selection = selector::resolve(selectors)?;
        apply(
            &selection,
            |s| &s.child_only_mask,
            self.slot,
            BitOp::Replace,
        );
        Ok(selection)
    }

    /// Registry keys currently enabled for this instrumentation.
    pub fn enabled_names(&self) -> impl Iterator<Item = &'static str> {
        slot_names(self.slot, |s| &s.enabled_mask)
    }

    /// Registry keys currently in child-only mode for this instrumentation.
    pub fn child_only_names(&self) -> impl Iterator<Item = &'static str> {
        slot_names(self.slot, |s| &s.child_only_mask)
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
        // One publish: the tracer is removed and the slot released together, so
        // the next occupant can't be reached through this one's leftovers.
        let slot = self.slot;
        update_slots(|slots| {
            slots.tracers[slot as usize] = None;
            slots.freed.push(slot);
            Some(())
        });
    }
}
