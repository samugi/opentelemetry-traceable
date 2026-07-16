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
    /// Whether this function only creates a span when called from within an
    /// already-active (recording) span -- set by `#[traceable(child_only)]`.
    /// Never a root, even when `enabled`; avoids orphan root spans for
    /// functions shared across multiple call paths where some paths aren't
    /// traced.
    pub child_only: bool,
}

impl TraceSite {
    /// Creates a new, disabled-by-default registry entry for `name`.
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            enabled: AtomicBool::new(false),
            child_only: false,
        }
    }

    /// Same as [`TraceSite::new`], but for `#[traceable(child_only)]` sites.
    pub const fn new_child_only(name: &'static str) -> Self {
        Self {
            name,
            enabled: AtomicBool::new(false),
            child_only: true,
        }
    }
}

/// Every `#[traceable]` function linked into the current binary.
#[linkme::distributed_slice]
pub static REGISTRY: [TraceSite] = [..];
