//! Link-time-collected registry of every `#[traceable]` call site.

use std::sync::LazyLock;
use std::sync::atomic::AtomicU64;

/// One entry per `#[traceable]` function, contributed by the macro at the
/// call site and collected here via `linkme` before `main()` runs.
///
/// A trace site is identified by its [`name`](Self::name) -- that key is what
/// configuration selects, exactly or by glob (see [`crate::selector`]). There is
/// no id or index: nothing here is addressed by position.
#[derive(Debug)]
pub struct TraceSite {
    /// The registry key used to enable/disable this function. Defaults to
    /// `module_path!() + "::" + fn_name`, or the macro's `name` argument.
    ///
    /// This is the *whole* identity of a trace site. Note the key isn't
    /// qualified by a surrounding `impl` type, so two methods with the same name
    /// in the same module share a key, and therefore toggle together.
    pub name: &'static str,

    /// Bitmask of which instrumentation slots currently have this function
    /// enabled -- bit `N` corresponds to the slot held by one live
    /// [`crate::instrumentation::Instrumentation`]. A function is traced by a
    /// slot iff that slot's bit is set here. The disabled fast path is a single
    /// load of this word compared against zero, so a function no instrumentation
    /// cares about costs one atomic load.
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

/// Every `#[traceable]` function linked into the current binary, in whatever
/// order the linker produced.
///
/// **A position in here means nothing.** `linkme` gives no ordering guarantee,
/// and in practice the order changes between builds -- editing an unrelated file
/// is enough to shuffle it. Iterate it to find sites; identify them by
/// [`name`](TraceSite::name), never by index. [`keys`] is the sorted view.
#[linkme::distributed_slice]
pub static REGISTRY: [TraceSite] = [..];

/// Every distinct registry key linked into this binary, sorted and deduplicated.
///
/// This is a *discovery* list, not an identity mapping: a key's position here
/// means nothing, and nothing is selected by index. A key is selected by writing
/// it -- exactly, or via a `*` glob -- see [`crate::selector`].
///
/// Sorted so that anything derived from it -- listings, discovery output -- is
/// reproducible run to run rather than inheriting the linker's `REGISTRY` order.
/// Built once; the registry is fixed at link time.
pub fn keys() -> &'static [&'static str] {
    static KEYS: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
        let mut keys: Vec<&'static str> = REGISTRY.iter().map(|site| site.name).collect();
        keys.sort_unstable();
        keys.dedup();
        keys
    });
    &KEYS
}
