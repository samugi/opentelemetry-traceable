//! An [`Instrumentation`] is an intance of a tracing configuration that defines
//! which (of the traceable functions) are enabled for tracing. Each instrumentation
//! maps to one tracer provider (and exporter), so that different exporting
//! configuration can be assigned to different instrumentations.
//!
//! This allows producing multiple isolated traces that can be each exported using
//! their specific configuration (endpoint, sampling strategy, etc).
//!
//! Every instrumentation takes up an available `slot`. There are a total of 64 slots
//! available, therefore there is a maximum number of 64 instrumentations that can be
//! configured at once.
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
//! Each `#[traceable]` site carries an "enabled" bitmask, one bit per slot.
//! The macro loads it once per call: `0` means nobody is tracing,
//! and anything else routes through [`start_spans`], which builds one child span
//! *per active slot* using that slot's own tracer and parent.
//!
//! # Two kinds: in-process and distributed
//!
//! ## In-process (the default)
//!
//! Every parent lives in one `opentelemetry::Context` extension, one entry per
//! active slot. These instrumentations cannot interact with propagators, therefore
//! their traces are contained in the current process (not distributed).
//! There can be many in-process instrumentations configured at once.
//!
//!
//! ## Distributed (at most one)
//!
//! [`InstrumentationBuilder::distributed`] uses the `Context` current-span
//! slot instead of the multi-slot envelope. The "current" span becomes the parent
//! of the new span.
//! The parent can be from an incoming `traceparent` header, created by another library,
//! or its own previously active span.
//! The main advantage of a distributed instrumentation is that it is compatible with
//! Context propagation. A propagator that injects from `Context::current()` results
//! in the new span being parented under the propagated context, as expected.
//!
//! There is exactly one current-span slot per `Context`, so at most one
//! distributed instrumentation may be live. If a second one is generated,
//! [`InstrumentationBuilder::build`] refuses it with [`BuildError::DistributedAlreadyLive`].

use std::sync::atomic::Ordering;
use std::sync::{Arc, LazyLock, Mutex};

use arc_swap::ArcSwap;
use opentelemetry::trace::{SpanBuilder, TraceContextExt, Tracer};
use opentelemetry::{Context, KeyValue};
use smallvec::SmallVec;

use crate::registry::REGISTRY;
use crate::selector::{self, Selection, UnknownKeys};

/// Number of [`Instrumentation`]s that can be live *simultaneously*: one bit per
/// slot in each site's mask.
pub const MAX_INSTRUMENTATIONS: u32 = 64;
const _: () = assert!(
    MAX_INSTRUMENTATIONS as usize == u64::BITS as usize,
    "MAX_INSTRUMENTATIONS must equal the number of bits in a site's mask"
);

// ========================================================================
// Shared helpers for reading and writing one slot's bit across every site.
// ========================================================================

#[inline]
fn bit(slot: u8) -> u64 {
    1u64 << slot
}

/// How a name match updates a slot bit.
#[derive(Clone, Copy)]
enum BitOp {
    /// Set the bit on matches, clear it on non-matches
    Replace,
    /// Set the bit on matches
    Add,
    /// Clear the bit on matches
    Remove,
}

/// Applies an already-resolved [`Selection`] (infallible)
fn apply(selection: &Selection, slot: u8, op: BitOp) {
    let b = bit(slot);
    // Walk the registry, match string keys. `selection.keys` is
    // sorted, so we use binary search.
    for site in REGISTRY.iter() {
        let hit = selection.keys.binary_search(&site.name).is_ok();
        match (op, hit) {
            (BitOp::Replace | BitOp::Add, true) => {
                site.enabled_mask.fetch_or(b, Ordering::Relaxed);
            }
            (BitOp::Replace, false) | (BitOp::Remove, true) => {
                site.enabled_mask.fetch_and(!b, Ordering::Relaxed);
            }
            (BitOp::Add, false) | (BitOp::Remove, false) => {}
        }
    }
}

/// Keys enabled for `slot`
fn slot_names(slot: u8) -> impl Iterator<Item = &'static str> {
    let b = bit(slot);
    let mut names: Vec<&'static str> = REGISTRY
        .iter()
        .filter(|site| site.enabled_mask.load(Ordering::Relaxed) & b != 0)
        .map(|site| site.name)
        .collect();
    names.sort_unstable();
    // dedup because one key can carry multiple call sites
    names.dedup();
    names.into_iter()
}

// ===========================================================================
// Type-erased tracers + slot table
// ===========================================================================

/// Object-safe view of a [`Tracer`]
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

#[derive(Clone)]
struct Slots {
    tracers: [Option<Arc<dyn DynTracer>>; MAX_INSTRUMENTATIONS as usize],
    /// The slot held by the live distributed instrumentation
    distributed: Option<u8>,
    next: u8,
    freed: Vec<u8>,
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

/// Lock free (read) slot state. ArcSwap allows atomic swap on the (rare)
/// create/drop of an instrumentation.
/// We don't use RwLock here because reads are the hot path and ArcSwap is lock free + wait free
static SLOTS: LazyLock<ArcSwap<Slots>> = LazyLock::new(|| ArcSwap::from_pointee(Slots::default()));

/// Write lock, so that writes happen sequentially
static SLOTS_WRITE: Mutex<()> = Mutex::new(());

fn update_slots<T, E>(f: impl FnOnce(&mut Slots) -> Result<T, E>) -> Result<T, E> {
    let _guard = SLOTS_WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let mut next = (**SLOTS.load()).clone();
    let out = f(&mut next)?;
    SLOTS.store(Arc::new(next));
    Ok(out)
}

// ===========================================================================
// The multi slot state carried inside the OpenTelemetry `Context`
// ===========================================================================

/// The active slots' parent contexts, carried inside a `Context` extension.
///
/// The distributed slot is missing from this one: the parent in that cae is "current"
/// span, which `Context` has a dedicated field for.
#[derive(Clone)]
struct InProcessParents(SmallVec<[(u8, Context); 4]>);

fn span_builder(name: &'static str, attrs: &[KeyValue]) -> SpanBuilder {
    if attrs.is_empty() {
        SpanBuilder::from_name(name)
    } else {
        SpanBuilder::from_name(name).with_attributes(attrs.to_vec())
    }
}

/// Called by the `#[traceable]` macro whenever the mask is non-zero.
///
/// Build one child span per active slot in `enabled_mask` and return the updated
/// `Context`, or `None` if no span was created.
///
/// This leverages OpenTelemetry's `Context` threadl-local storage which uses
/// RAII to guarantee the content of the context is always up to date and reflects
/// the current state for the duration of a particular function call.
///
/// The distributed slot uses the `span` field of the context to store its current
/// span, and it inherits the parent from the current `span` field of the context.
///
/// Every other slot uses the [`InProcessParents`] envelope of the `Context`,
/// a generic slot's parent is bound to (and uses the tracer from) a dedicated slot
/// in the envelope. Every slot follows a specific call path.
#[doc(hidden)]
pub fn start_spans(
    enabled_mask: u64,
    span_name: &'static str,
    attrs: Vec<KeyValue>,
) -> Option<Context> {
    // we use `load_full` to avoid the arc-swap lock of `load` which would
    // be blocking for a `store` (i.e. a config reload)
    let slots = SLOTS.load_full();
    let mut cx = Context::current();
    let mut created_any = false;

    // The distributed slot, if it has this site enabled.
    let distributed = slots
        .distributed
        .filter(|slot| enabled_mask & bit(*slot) != 0);
    if let Some(tracer) = distributed.and_then(|slot| slots.tracers[slot as usize].as_ref()) {
        // here parent is whatever span is current (could be remote, another
        // library's span, or this instrumentation's own previous span) the
        // child lands in that same slot.
        cx = tracer.start_in(span_builder(span_name, &attrs), &cx);
        created_any = true;
    }

    // Everything else is in-process: parented within the envelope.
    let in_process_mask = enabled_mask & !distributed.map_or(0, bit);
    if in_process_mask != 0 {
        let mut env: SmallVec<[(u8, Context); 4]> = cx
            .get::<InProcessParents>()
            .map(|s| s.0.clone())
            .unwrap_or_default();
        let mut env_changed = false;

        let mut bits = in_process_mask;
        while bits != 0 {
            let slot = bits.trailing_zeros() as u8;
            bits &= bits - 1;

            let Some(tracer) = slots.tracers[slot as usize].as_ref() else {
                // Instrumentation was dropped mid-flight; just skip its slot.
                continue;
            };
            // TODO: can we make this faster
            let parent = env.iter().find(|(s, _)| *s == slot).map(|(_, c)| c);
            let base = parent.cloned().unwrap_or_default();
            let child = tracer.start_in(span_builder(span_name, &attrs), &base);
            match env.iter_mut().find(|(s, _)| *s == slot) {
                Some(entry) => entry.1 = child,
                None => env.push((slot, child)),
            }
            env_changed = true;
        }

        if env_changed {
            // One envelope carrying every active in-process slot's new tip.
            cx = cx.with_value(InProcessParents(env));
            created_any = true;
        }
    }

    created_any.then_some(cx)
}

// ===========================================================================
// Public handle + builder.
// ===========================================================================

/// Why [`InstrumentationBuilder::build`] could not produce an [`Instrumentation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildError {
    /// [`MAX_INSTRUMENTATIONS`] are already live. Dropping any of them frees its
    /// slot for reuse.
    SlotsExhausted,
    /// A distributed instrumentation is already live, and there can only be one:
    /// they share `Context`'s single current-span slot, so a second would
    /// overwrite the first's span and merge the two traces. Dropping the live one
    /// releases the role.
    DistributedAlreadyLive,
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SlotsExhausted => write!(
                f,
                "no free instrumentation slot ({MAX_INSTRUMENTATIONS} already live)"
            ),
            Self::DistributedAlreadyLive => {
                write!(f, "a distributed instrumentation is already live")
            }
        }
    }
}

impl std::error::Error for BuildError {}

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
    distributed: bool,
}

impl std::fmt::Debug for InstrumentationBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstrumentationBuilder")
            .field("name", &self.name)
            .field("tracer", &self.tracer.as_ref().map(|_| "<tracer>"))
            .field("distributed", &self.distributed)
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

    /// Make this a *distributed* instrumentation instead of the default
    /// in-process one: its spans use `opentelemetry::Context`'s current-span
    /// slot, so they join an incoming `traceparent`, nest under spans created by
    /// other libraries, and are picked up by a propagator injecting outbound.
    ///
    /// At most one may be live at a time -- see [`BuildError::DistributedAlreadyLive`]
    /// and the module docs.
    #[must_use]
    pub fn distributed(mut self) -> Self {
        self.distributed = true;
        self
    }

    /// Allocate a slot and register the tracer, returning a live handle.
    ///
    /// # Errors
    ///
    /// [`BuildError::SlotsExhausted`] if [`MAX_INSTRUMENTATIONS`] are already
    /// live, or [`BuildError::DistributedAlreadyLive`] if this is a distributed
    /// instrumentation and one already exists. Either way nothing is allocated.
    ///
    /// # Panics
    ///
    /// Panics if no tracer was set via [`tracer`](Self::tracer).
    pub fn build(self) -> Result<Instrumentation, BuildError> {
        let tracer = self
            .tracer
            .expect("InstrumentationBuilder::build requires a tracer");

        // One publish: the slot is taken, its tracer installed and the distributed
        // role claimed together, so any later `enable` on this slot is guaranteed
        // to find the tracer, and two threads can't both claim the role.
        let slot = update_slots(|slots| {
            if self.distributed && slots.distributed.is_some() {
                return Err(BuildError::DistributedAlreadyLive);
            }
            let slot = slots.alloc().ok_or(BuildError::SlotsExhausted)?;
            slots.tracers[slot as usize] = Some(tracer);
            if self.distributed {
                slots.distributed = Some(slot);
            }
            Ok(slot)
        })?;

        Ok(Instrumentation { slot })
    }
}

impl Instrumentation {
    /// Start building a new instrumentation.
    #[must_use]
    pub fn builder() -> InstrumentationBuilder {
        InstrumentationBuilder::default()
    }

    /// Whether this is the distributed instrumentation (see
    /// [`InstrumentationBuilder::distributed`]).
    #[must_use]
    pub fn is_distributed(&self) -> bool {
        SLOTS.load().distributed == Some(self.slot)
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
        apply(&selection, self.slot, BitOp::Replace);
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
        apply(&selection, self.slot, BitOp::Add);
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
        apply(&selection, self.slot, BitOp::Remove);
        Ok(selection)
    }

    /// Registry keys currently enabled for this instrumentation.
    pub fn enabled_names(&self) -> impl Iterator<Item = &'static str> {
        slot_names(self.slot)
    }
}

impl Drop for Instrumentation {
    fn drop(&mut self) {
        // Clear this slot's bit everywhere first (stops new spans), then drop
        // the tracer -- so no thread can see a set bit with a missing tracer.
        let b = bit(self.slot);
        for site in REGISTRY.iter() {
            site.enabled_mask.fetch_and(!b, Ordering::Relaxed);
        }
        // One publish: the tracer is removed, the distributed role (if this held
        // it) released and the slot freed together, so the next occupant can't be
        // reached through this one's leftovers.
        let slot = self.slot;
        let _: Result<(), ()> = update_slots(|slots| {
            slots.tracers[slot as usize] = None;
            if slots.distributed == Some(slot) {
                slots.distributed = None;
            }
            slots.freed.push(slot);
            Ok(())
        });
    }
}
