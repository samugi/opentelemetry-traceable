//! Dynamic, per-function tracing on top of `opentelemetry-rust`.
//!
//! Annotate any `fn`/`async fn` with `#[traceable]`, then create an
//! [`instrumentation::Instrumentation`] and toggle tracing for those functions
//! at runtime.

pub mod instrumentation;
pub mod registry;
pub mod selector;

pub use opentelemetry;
pub use opentelemetry_traceable_macros::traceable;

#[cfg(feature = "sdk")]
pub use opentelemetry_sdk;

#[cfg(feature = "otlp")]
pub use opentelemetry_otlp;

#[doc(hidden)]
pub mod __private {
    pub use linkme;
}
