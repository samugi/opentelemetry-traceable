//! Link-time-collected registry of every `#[traceable]` call site.

use std::sync::atomic::AtomicU64;

/// One entry per `#[traceable]` function, contributed by the macro at the
/// call site and collected here via `linkme` before `main()` runs.
///
/// A trace site's **id** (used by [`crate::codec`] to encode a subset, and
/// reported by [`crate::catalog`]) is simply its index in [`REGISTRY`] --
/// dense and incremental, so it isn't stored here.
#[derive(Debug)]
pub struct TraceSite {
    /// The registry key used to enable/disable this function. Defaults to
    /// `module_path!() + "::" + fn_name`, or the macro's `name` argument.
    pub name: &'static str,

    /// Bitmask of which instrumentation slots currently have this function
    /// enabled -- bit `N` corresponds to instrumentation slot `N` (bit 0 is
    /// the always-present default instrumentation driven by `stylus::config`;
    /// bits 1..64 are dynamically-created named [`crate::instrumentation`]s).
    /// A function is traced by a slot iff that slot's bit is set here. The
    /// disabled fast path is a single load of this word compared against zero,
    /// so a function no instrumentation cares about costs one atomic load.
    pub enabled_mask: AtomicU64,
    /// Bitmask of which instrumentation slots have this function in
    /// *child-only* mode: for a slot whose bit is set here, this function only
    /// creates a span when called from within an already-recording span for
    /// that same slot -- never as a root, even when enabled. Set at runtime
    /// (not a source annotation): whether a shared function should be allowed
    /// to root a trace is request-relative, so it's decided when tracing is
    /// configured, avoiding orphan root spans on call paths that aren't traced.
    pub child_only_mask: AtomicU64,
}

impl TraceSite {
    /// Creates a new registry entry for `name`, with every slot disabled and
    /// root-capable (both masks start at zero).
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            enabled_mask: AtomicU64::new(0),
            child_only_mask: AtomicU64::new(0),
        }
    }
}

/// Every `#[traceable]` function linked into the current binary.
#[linkme::distributed_slice]
pub static REGISTRY: [TraceSite] = [..];
