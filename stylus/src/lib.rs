//! Dynamic, per-function tracing on top of `opentelemetry-rust`.
//!
//! Annotate any `fn`/`async fn` with `#[traceable]` and toggle tracing for it
//! at runtime — individually, in bulk, or as an arbitrary custom subset —
//! via [`config`], without recompiling.

pub mod config;
pub mod registry;
pub mod schema;
pub mod subset;

pub use stylus_macros::traceable;
