//! Dynamic, per-function tracing on top of `opentelemetry-rust`.
//!
//! Annotate any `fn`/`async fn` with `#[traceable]`, then create an
//! [`instrumentation::Instrumentation`] and toggle tracing for those functions
//! at runtime.

pub mod instrumentation;
pub mod registry;
pub mod selector;

pub use opentelemetry_traceable_macros::traceable;

#[doc(hidden)]
pub mod __private {
    pub use linkme;
}
