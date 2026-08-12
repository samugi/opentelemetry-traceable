//! Dynamic, per-function tracing on top of `opentelemetry-rust`.
//!
//! Annotate any `fn`/`async fn` with `#[traceable]`, then create an
//! [`instrumentation::Instrumentation`] and toggle tracing for those functions
//! at runtime — individually, in bulk, or as an arbitrary custom subset —
//! without recompiling.
//!
//! An `Instrumentation` is the only way to turn tracing on: there is no global
//! or default instrumentation and no process-wide tracer. Each one carries its
//! own [`opentelemetry::trace::Tracer`], its own enabled subset, and its own
//! isolated span hierarchy over the same call flow, so several can run in
//! parallel over the same functions. Subsets are applied as compact,
//! [`codec`]-encoded trace-site id lists.

pub mod catalog;
pub mod codec;
pub mod instrumentation;
pub mod registry;

pub use opentelemetry_traceable_macros::traceable;

/// Re-exported so downstream crates never need their own `opentelemetry` dependency
/// (for `#[traceable]`'s expansion or for `Instrumentation::tracer`/`Context`/`KeyValue`),
/// and so it's structurally impossible for their code to resolve a different
/// `opentelemetry` than this crate was built against.
pub use opentelemetry;

#[doc(hidden)]
pub mod __private {
    pub use linkme;
}
