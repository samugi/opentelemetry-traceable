//! An [`Instrumentation`] is an intance of a tracing configuration that defines
//! which (of the traceable functions) are enabled for tracing. Each instrumentation
//! maps to one tracer provider (and exporter), so that different exporting
//! configuration can be assigned to different instrumentations.
//!
//! This allows producing multiple isolated traces that can each be exported using
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
//! Each function marked by the `#[traceable]` attribute constitutes a traceable
//! site. Each traceable site carries an "enabled" bitmask, one bit per slot.
//! The macro loads it once per call: `0` means nothing is tracing,
//! anything else routes through [`start_spans`], which builds one child span
//! *per active slot* using that slot's own tracer and parent.
//!
//! # Two kinds of instrumentation: in-process and distributed
//!
//! ## In-process (the default)
//!
//! Every parent lives in one `opentelemetry::Context` extension, one entry per
//! active slot. These instrumentations cannot interact with propagators, therefore
//! their traces are contained in the current process (not distributed).
//! There can be many in-process instrumentations configured at once.
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

use std::sync::Arc;
use std::sync::atomic::Ordering;

use opentelemetry::trace::{SpanBuilder, Tracer};
use opentelemetry::{Context, KeyValue};
use smallvec::SmallVec;

mod context;
mod helpers;
mod slots;
mod tracer;

use crate::instrumentation::context::InProcessParents;
use crate::instrumentation::helpers::{BitOp, apply, slot_names};
use crate::instrumentation::slots::{SLOTS, update_slots};
use crate::instrumentation::tracer::DynTracer;
use crate::registry::REGISTRY;
use crate::selector::{self, Selection, UnknownKeys};
use helpers::bit;

/// Number of [`Instrumentation`]s that can be live *simultaneously*: one bit per
/// slot in each site's mask.
pub const MAX_INSTRUMENTATIONS: u32 = 64;
const _: () = assert!(
    MAX_INSTRUMENTATIONS as usize == u64::BITS as usize,
    "MAX_INSTRUMENTATIONS must equal the number of bits in a site's mask"
);

fn span_builder(name: &'static str, attrs: &[KeyValue]) -> SpanBuilder {
    if attrs.is_empty() {
        SpanBuilder::from_name(name)
    } else {
        SpanBuilder::from_name(name).with_attributes(attrs.to_vec())
    }
}

/// Called by the `#[traceable]` macro whenever the mask is non-zero.
///
/// Build one child span per active slot in `enabled_slots` and return the updated
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
    enabled_slots: u64,
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
        .filter(|slot| enabled_slots & bit(*slot) != 0);
    if let Some(tracer) = distributed.and_then(|slot| slots.tracers[slot as usize].as_ref()) {
        // here parent is whatever span is current (could be remote, another
        // library's span, or this instrumentation's own previous span) the
        // child lands in that same slot.
        cx = tracer.start_in(span_builder(span_name, &attrs), &cx);
        created_any = true;
    }

    // Everything else is in-process: parented within the envelope.
    let in_process_mask = enabled_slots & !distributed.map_or(0, bit);
    if in_process_mask != 0 {
        let mut envelope: SmallVec<[(u8, Context); 4]> = cx
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
            // load the parent from the corresponding slot
            // TODO: can we make this faster
            let parent = envelope.iter().find(|(s, _)| *s == slot).map(|(_, c)| c);
            let base = parent.cloned().unwrap_or_default();
            let child = tracer.start_in(span_builder(span_name, &attrs), &base);
            match envelope.iter_mut().find(|(s, _)| *s == slot) {
                Some(entry) => entry.1 = child,
                None => envelope.push((slot, child)),
            }
            env_changed = true;
        }

        if env_changed {
            // One envelope carrying every active in-process slot's new tip.
            cx = cx.with_value(InProcessParents(envelope));
            created_any = true;
        }
    }

    created_any.then_some(cx)
}

/// `Instrumentaiton` builder errors
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildError {
    /// `MAX_INSTRUMENTATIONS` reached.
    SlotsExhausted,
    /// A distributed instrumentation already exists.
    DistributedAlreadyLive,
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SlotsExhausted => write!(
                f,
                "no free instrumentation slot ({MAX_INSTRUMENTATIONS} reached)"
            ),
            Self::DistributedAlreadyLive => {
                write!(f, "a distributed instrumentation is already configured")
            }
        }
    }
}

impl std::error::Error for BuildError {}

/// A dynamic, isolated tracing instrumentation.
/// Each instrumentation takes a dedicated slot.
/// The slot number identifies which slot this instrumentation uses.
/// The slot number is used to map to that slot in `SLOTS`, and in
/// the OTel Context to identify which (active) span belongs to the slot.
#[derive(Debug)]
pub struct Instrumentation {
    slot_number: u8,
}

/// Builder for an [`Instrumentation`].
#[derive(Default)]
pub struct InstrumentationBuilder {
    tracer: Option<Arc<dyn DynTracer>>,
    distributed: bool,
}

impl std::fmt::Debug for InstrumentationBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstrumentationBuilder")
            .field("tracer", &self.tracer.as_ref().map(|_| "<tracer>"))
            .field("distributed", &self.distributed)
            .finish()
    }
}

impl InstrumentationBuilder {
    /// Set the [`Tracer`] this instrumentation's spans are created with.
    #[must_use]
    pub fn tracer<T>(mut self, tracer: T) -> Self
    where
        T: Tracer + Send + Sync + 'static,
        T::Span: Send + Sync + 'static,
    {
        self.tracer = Some(Arc::new(tracer));
        self
    }

    /// Make this a _distributed_ instrumentation instead of the default _in-process_.
    /// A distributed instrumentation is compatible with distributed tracing (
    /// it can inherit a parent span via Context propagation, e.g. traceparent header).
    /// At most one _distributed_ instrumentation can be live at a time.
    #[must_use]
    pub fn distributed(mut self) -> Self {
        self.distributed = true;
        self
    }

    /// Build the instrumentation.
    /// Allocates a slot and registers the tracer, returning a handle.
    ///
    /// # Errors
    ///
    /// [`BuildError::SlotsExhausted`] if [`MAX_INSTRUMENTATIONS`] are already
    /// live, or [`BuildError::DistributedAlreadyLive`] if this is a distributed
    /// instrumentation and one already exists.
    ///
    /// # Panics
    ///
    /// Panics if no tracer was set via [`tracer`](Self::tracer).
    pub fn build(self) -> Result<Instrumentation, BuildError> {
        let tracer = self
            .tracer
            .expect("InstrumentationBuilder::build requires a tracer");

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

        Ok(Instrumentation { slot_number: slot })
    }
}

impl Instrumentation {
    /// Create a new `InstrumentationBuilder`.
    #[must_use]
    pub fn builder() -> InstrumentationBuilder {
        InstrumentationBuilder::default()
    }

    /// Whether this is the distributed instrumentation (see
    /// [`InstrumentationBuilder::distributed`]).
    #[must_use]
    pub fn is_distributed(&self) -> bool {
        SLOTS.load().distributed == Some(self.slot_number)
    }

    /// Enable every known `#[traceable]` function for this instrumentation.
    pub fn enable_all(&self) {
        let b = bit(self.slot_number);
        for site in REGISTRY.iter() {
            site.enabled_slots.fetch_or(b, Ordering::Relaxed);
        }
    }

    /// Disable every function for this instrumentation.
    pub fn disable_all(&self) {
        let b = bit(self.slot_number);
        for site in REGISTRY.iter() {
            site.enabled_slots.fetch_and(!b, Ordering::Relaxed);
        }
    }

    /// Replace this instrumentation's enabled sites with the ones the
    /// provided `selectors` resolve to.
    ///
    /// # Errors
    ///
    /// [`UnknownKeys`] does not resolve to any `#[traceable]` function. In
    /// that case **nothing is applied** and this instrumentation keeps the
    /// set it already had.
    pub fn set_enabled<S: AsRef<str>>(&self, selectors: &[S]) -> Result<Selection, UnknownKeys> {
        let selection = selector::resolve(selectors)?;
        apply(&selection, self.slot_number, BitOp::Replace);
        Ok(selection)
    }

    /// Enable everything that `selectors` resolves to, leaving the rest of the enabled
    /// set unchanged (additive).
    ///
    /// # Errors
    ///
    /// As [`set_enabled`](Self::set_enabled).
    pub fn enable<S: AsRef<str>>(&self, selectors: &[S]) -> Result<Selection, UnknownKeys> {
        let selection = selector::resolve(selectors)?;
        apply(&selection, self.slot_number, BitOp::Add);
        Ok(selection)
    }

    /// Disable everything that `selectors` resolves to, leaving the rest of the enabled
    /// set unchanged.
    ///
    /// # Errors
    ///
    /// As [`set_enabled`](Self::set_enabled).
    pub fn disable<S: AsRef<str>>(&self, selectors: &[S]) -> Result<Selection, UnknownKeys> {
        let selection = selector::resolve(selectors)?;
        apply(&selection, self.slot_number, BitOp::Remove);
        Ok(selection)
    }

    /// Registry keys currently enabled for this instrumentation.
    pub fn enabled_names(&self) -> impl Iterator<Item = &'static str> {
        slot_names(self.slot_number)
    }
}

impl Drop for Instrumentation {
    fn drop(&mut self) {
        // Clear this slot's bit everywhere first (stops new spans), then drop
        // the tracer, so no thread can see a set bit with a missing tracer.
        let b = bit(self.slot_number);
        for site in REGISTRY.iter() {
            site.enabled_slots.fetch_and(!b, Ordering::Relaxed);
        }

        let slot = self.slot_number;
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
