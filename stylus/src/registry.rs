//! Link-time-collected registry of every `#[traceable]` call site.

use std::sync::atomic::AtomicBool;

/// One entry per `#[traceable]` function, contributed by the macro at the
/// call site and collected here via `linkme` before `main()` runs.
#[derive(Debug)]
pub struct TraceSite {
    /// The registry key used to enable/disable this function. Defaults to
    /// `module_path!() + "::" + fn_name`, or the macro's `name` argument.
    pub name: &'static str,
    /// Whether tracing is currently turned on for this function.
    pub enabled: AtomicBool,
    /// Whether this function is in *child-only* mode: it only creates a span
    /// when called from within an already-active (recording) span -- never as
    /// a root, even when `enabled`. Set at runtime via `stylus::config` (not a
    /// source annotation): whether a shared function should be allowed to root
    /// a trace is request-relative, so this is decided when tracing is
    /// configured, avoiding orphan root spans on call paths that aren't traced.
    pub child_only: AtomicBool,
}

impl TraceSite {
    /// Creates a new registry entry for `name`, disabled and root-capable by
    /// default (both `enabled` and `child_only` start `false`).
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            enabled: AtomicBool::new(false),
            child_only: AtomicBool::new(false),
        }
    }
}

/// Every `#[traceable]` function linked into the current binary.
#[linkme::distributed_slice]
pub static REGISTRY: [TraceSite] = [..];
