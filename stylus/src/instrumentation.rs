//! Multiple, independently-configured tracing *instrumentations* over the same
//! set of `#[traceable]` functions.
//!
//! The always-present **default instrumentation** (slot [`DEFAULT_SLOT`]) is
//! what [`crate::config`]'s free functions drive -- it behaves exactly as
//! stylus always has, and owns downstream `traceparent` propagation. On top of
//! it you can create up to [`MAX_INSTRUMENTATIONS`]`- 1` **named
//! instrumentations** at runtime, each with its own enabled subset, its own
//! isolated span hierarchy built from the same underlying call flow, and its
//! own [`Tracer`] (potentially a different backend):
//!
//! ```ignore
//! let provider = /* some SdkTracerProvider */;
//! let checkout = stylus::instrumentation::Instrumentation::builder()
//!     .name("checkout-debug")
//!     .tracer(provider.tracer("checkout-debug"))
//!     .build()
//!     .expect("a free instrumentation slot");
//! checkout.enable(["my_crate::checkout", "my_crate::db::insert"]);
//! // ... dropping `checkout` stops new spans for it and frees the slot.
//! ```
//!
//! # How isolation works
//!
//! Each `#[traceable]` site carries a `u64` enabled bitmask (one bit per slot).
//! The macro loads it once per call: `0` means nobody is tracing (the
//! single-atomic-load fast path), `1` means only the default slot (today's
//! exact code, no overhead added), and any other value routes through
//! [`start_spans`], which builds one child span *per active slot* using that
//! slot's own tracer and parent. The per-slot parents ride together inside one
//! `opentelemetry::Context` extension so async propagation stays O(1) -- there
//! is exactly one ambient context per thread, so the currently-active per-slot
//! spans must share one propagation envelope rather than N independent ones.
//!
//! # Limitation
//!
//! `opentelemetry::Context` has a single "current span" slot, so only the
//! default instrumentation's span is injected into outbound `traceparent`
//! headers. Named instrumentations are in-process only.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use arc_swap::ArcSwap;
use opentelemetry::trace::{SpanBuilder, TraceContextExt, Tracer};
use opentelemetry::{Context, KeyValue};
use smallvec::SmallVec;

use crate::registry::{REGISTRY, TraceSite};
use crate::subset::{self, DecodeError};

/// Total number of instrumentation slots, including the default one -- one bit
/// per slot in each site's `u64` masks. Bit 0 is [`DEFAULT_SLOT`]; bits 1..64
/// are available for named [`Instrumentation`]s.
pub const MAX_INSTRUMENTATIONS: u32 = 64;

/// The permanently-reserved slot driven by [`crate::config`]'s free functions.
/// Always alive for the process lifetime; never allocated or freed like a named
/// instrumentation.
pub const DEFAULT_SLOT: u8 = 0;

// ---------------------------------------------------------------------------
// Slot-scoped bit operations, shared by `config` (slot 0) and `Instrumentation`
// (its own slot). Each walks the full `REGISTRY` once -- meant to be called
// rarely (config/reload events), never on a hot path.
// ---------------------------------------------------------------------------

#[inline]
fn bit(slot: u8) -> u64 {
    1u64 << slot
}

/// How a name/blob match updates a slot's bit.
#[derive(Clone, Copy)]
enum BitOp {
    /// Set the bit on matches, clear it on non-matches (whole-set replace).
    Replace,
    /// Set the bit on matches only, leave non-matches untouched (additive).
    Add,
    /// Clear the bit on matches only, leave non-matches untouched.
    Remove,
}

fn apply_names<'a>(
    names: impl IntoIterator<Item = &'a str>,
    field: impl Fn(&TraceSite) -> &AtomicU64,
    slot: u8,
    op: BitOp,
) {
    let b = bit(slot);
    let wanted: HashSet<&str> = names.into_iter().collect();
    for site in REGISTRY.iter() {
        let hit = wanted.contains(site.name);
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

fn apply_blob(
    blob: &str,
    field: impl Fn(&TraceSite) -> &AtomicU64,
    slot: u8,
    op: BitOp,
) -> Result<(), DecodeError> {
    let filter = subset::decode(blob)?;
    let b = bit(slot);
    for site in REGISTRY.iter() {
        let hit = filter.contains_id(subset::id_of(site.name));
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
    Ok(())
}

pub(crate) fn slot_set_enabled<'a>(names: impl IntoIterator<Item = &'a str>, slot: u8) {
    apply_names(names, |s| &s.enabled_mask, slot, BitOp::Replace);
}

pub(crate) fn slot_enable<'a>(names: impl IntoIterator<Item = &'a str>, slot: u8) {
    apply_names(names, |s| &s.enabled_mask, slot, BitOp::Add);
}

pub(crate) fn slot_disable<'a>(names: impl IntoIterator<Item = &'a str>, slot: u8) {
    apply_names(names, |s| &s.enabled_mask, slot, BitOp::Remove);
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

pub(crate) fn slot_set_enabled_encoded(blob: &str, slot: u8) -> Result<(), DecodeError> {
    apply_blob(blob, |s| &s.enabled_mask, slot, BitOp::Replace)
}

pub(crate) fn slot_enable_encoded(blob: &str, slot: u8) -> Result<(), DecodeError> {
    apply_blob(blob, |s| &s.enabled_mask, slot, BitOp::Add)
}

pub(crate) fn slot_disable_encoded(blob: &str, slot: u8) -> Result<(), DecodeError> {
    apply_blob(blob, |s| &s.enabled_mask, slot, BitOp::Remove)
}

pub(crate) fn slot_is_enabled(name: &str, slot: u8) -> bool {
    let b = bit(slot);
    REGISTRY
        .iter()
        .find(|site| site.name == name)
        .is_some_and(|site| site.enabled_mask.load(Ordering::Relaxed) & b != 0)
}

pub(crate) fn slot_enabled_names(slot: u8) -> impl Iterator<Item = &'static str> {
    let b = bit(slot);
    REGISTRY
        .iter()
        .filter(move |site| site.enabled_mask.load(Ordering::Relaxed) & b != 0)
        .map(|site| site.name)
}

pub(crate) fn slot_set_child_only<'a>(names: impl IntoIterator<Item = &'a str>, slot: u8) {
    apply_names(names, |s| &s.child_only_mask, slot, BitOp::Replace);
}

pub(crate) fn slot_enable_child_only<'a>(names: impl IntoIterator<Item = &'a str>, slot: u8) {
    apply_names(names, |s| &s.child_only_mask, slot, BitOp::Add);
}

pub(crate) fn slot_disable_child_only<'a>(names: impl IntoIterator<Item = &'a str>, slot: u8) {
    apply_names(names, |s| &s.child_only_mask, slot, BitOp::Remove);
}

pub(crate) fn slot_set_child_only_encoded(blob: &str, slot: u8) -> Result<(), DecodeError> {
    apply_blob(blob, |s| &s.child_only_mask, slot, BitOp::Replace)
}

pub(crate) fn slot_child_only_names(slot: u8) -> impl Iterator<Item = &'static str> {
    let b = bit(slot);
    REGISTRY
        .iter()
        .filter(move |site| site.child_only_mask.load(Ordering::Relaxed) & b != 0)
        .map(|site| site.name)
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

/// Next slot to hand out. Bump-allocated (slot 0 reserved for the default);
/// slots are never reused for the lifetime of the process, which sidesteps the
/// ABA hazard a free-list would create (a drop clears a slot's bit across many
/// sites non-atomically, so reusing the number could mis-attribute a straggling
/// span to the next occupant).
static NEXT_SLOT: Mutex<u8> = Mutex::new(1);

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
/// `Context` to attach for the wrapped call, or `None` if every active slot was
/// suppressed by child-only mode (in which case the caller runs the body with
/// no context machinery at all).
///
/// Not part of the stable API -- called by the `#[traceable]` macro's
/// multi-instrumentation branch. `enabled_mask` here always has at least one
/// bit other than [`DEFAULT_SLOT`] set (the macro handles the `== 0` and
/// `== 1` cases inline, so this never pays the extension-map cost for
/// default-only callers).
#[doc(hidden)]
pub fn start_spans(
    enabled_mask: u64,
    child_only_mask: u64,
    span_name: &'static str,
    default_tracer_name: &'static str,
    attrs: Vec<KeyValue>,
) -> Option<Context> {
    let cur = Context::current();
    let mut env: SmallVec<[(u8, Context); 4]> = cur
        .get::<MultiInstrumentState>()
        .map(|s| s.0.clone())
        .unwrap_or_default();

    let table = SLOT_TABLE.load();
    let mut slot0_cx: Option<Context> = None;
    let mut created_any = false;

    let mut bits = enabled_mask;
    while bits != 0 {
        let slot = bits.trailing_zeros() as u8;
        bits &= bits - 1;
        let is_child_only = child_only_mask & bit(slot) != 0;

        if slot == DEFAULT_SLOT {
            // The default slot parents off the ambient span -- today's behavior.
            if is_child_only && !cur.span().is_recording() {
                continue;
            }
            let tracer = opentelemetry::global::tracer(default_tracer_name);
            slot0_cx = Some(tracer.start_in(span_builder(span_name, &attrs), &cur));
            created_any = true;
        } else {
            let parent = env.iter().find(|(s, _)| *s == slot).map(|(_, c)| c);
            let recording = parent.is_some_and(|c| c.span().is_recording());
            if is_child_only && !recording {
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
    }

    if !created_any {
        return None;
    }

    // One physical context to attach: the default slot's span (if any) in the
    // fast `.span` slot, the named slots in a single `with_value` envelope.
    let base_cx = slot0_cx.unwrap_or_else(|| cur.clone());
    Some(base_cx.with_value(MultiInstrumentState(env)))
}

// ---------------------------------------------------------------------------
// Public handle + builder.
// ---------------------------------------------------------------------------

/// Returned by [`InstrumentationBuilder::build`] when all
/// [`MAX_INSTRUMENTATIONS`] slots have been used up over the process lifetime.
#[derive(Debug)]
pub struct SlotsExhausted;

impl std::fmt::Display for SlotsExhausted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "no free instrumentation slot ({MAX_INSTRUMENTATIONS} created over this process's lifetime)"
        )
    }
}

impl std::error::Error for SlotsExhausted {}

/// A dynamically-created, independently-configured tracing instrumentation.
///
/// Enable/disable functions for it exactly as with [`crate::config`], but
/// scoped to this instrumentation -- its span tree, tracer, and enabled set are
/// isolated from the default instrumentation and from every other named one.
/// Dropping the handle stops new spans for it and frees nothing reusable (see
/// [`MAX_INSTRUMENTATIONS`]); spans already in flight finish and export
/// normally.
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

        let mut next = NEXT_SLOT.lock().expect("stylus slot allocator poisoned");
        if u32::from(*next) >= MAX_INSTRUMENTATIONS {
            return Err(SlotsExhausted);
        }
        let slot = *next;
        *next += 1;
        drop(next);

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

    /// Replace this instrumentation's enabled set with exactly `names`.
    pub fn set_enabled<'a>(&self, names: impl IntoIterator<Item = &'a str>) {
        slot_set_enabled(names, self.slot);
    }

    /// Enable `names` for this instrumentation (additive).
    pub fn enable<'a>(&self, names: impl IntoIterator<Item = &'a str>) {
        slot_enable(names, self.slot);
    }

    /// Disable `names` for this instrumentation, leaving the rest untouched.
    pub fn disable<'a>(&self, names: impl IntoIterator<Item = &'a str>) {
        slot_disable(names, self.slot);
    }

    /// Enable every known `#[traceable]` function for this instrumentation.
    pub fn enable_all(&self) {
        slot_enable_all(self.slot);
    }

    /// Disable every function for this instrumentation.
    pub fn disable_all(&self) {
        slot_disable_all(self.slot);
    }

    /// Replace the enabled set from a compact blob (see
    /// [`crate::config::set_enabled_encoded`]).
    pub fn set_enabled_encoded(&self, blob: &str) -> Result<(), DecodeError> {
        slot_set_enabled_encoded(blob, self.slot)
    }

    /// Enable whatever's in the blob (additive).
    pub fn enable_encoded(&self, blob: &str) -> Result<(), DecodeError> {
        slot_enable_encoded(blob, self.slot)
    }

    /// Disable whatever's in the blob.
    pub fn disable_encoded(&self, blob: &str) -> Result<(), DecodeError> {
        slot_disable_encoded(blob, self.slot)
    }

    /// Replace this instrumentation's child-only set with exactly `names` (see
    /// [`crate::config::set_child_only`]).
    pub fn set_child_only<'a>(&self, names: impl IntoIterator<Item = &'a str>) {
        slot_set_child_only(names, self.slot);
    }

    /// Add `names` to this instrumentation's child-only set.
    pub fn enable_child_only<'a>(&self, names: impl IntoIterator<Item = &'a str>) {
        slot_enable_child_only(names, self.slot);
    }

    /// Remove `names` from this instrumentation's child-only set.
    pub fn disable_child_only<'a>(&self, names: impl IntoIterator<Item = &'a str>) {
        slot_disable_child_only(names, self.slot);
    }

    /// Replace the child-only set from a compact blob.
    pub fn set_child_only_encoded(&self, blob: &str) -> Result<(), DecodeError> {
        slot_set_child_only_encoded(blob, self.slot)
    }

    /// Whether `name` is enabled for this instrumentation.
    #[must_use]
    pub fn is_enabled(&self, name: &str) -> bool {
        slot_is_enabled(name, self.slot)
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
    }
}
