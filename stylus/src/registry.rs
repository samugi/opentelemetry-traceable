//! Link-time-collected registry of every `#[traceable]` call site, and the
//! stable id ordering derived from it.

use std::sync::LazyLock;
use std::sync::atomic::AtomicU64;

/// One entry per `#[traceable]` function, contributed by the macro at the
/// call site and collected here via `linkme` before `main()` runs.
///
/// A trace site's **id** (used by [`crate::codec`] to encode a subset, and
/// reported by [`crate::catalog`]) is derived from its [`name`](Self::name)
/// alone -- see [`names_by_id`].
#[derive(Debug)]
pub struct TraceSite {
    /// The registry key used to enable/disable this function. Defaults to
    /// `module_path!() + "::" + fn_name`, or the macro's `name` argument.
    ///
    /// This is the *only* thing a site's id depends on. Note the key isn't
    /// qualified by a surrounding `impl` type, so two methods with the same name
    /// in the same module share a key -- and therefore an id, toggling together.
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
/// **Don't use a position in here as an id.** `linkme` gives no ordering
/// guarantee, and in practice the order changes between builds -- editing an
/// unrelated file is enough to shuffle it. Ids come from [`names_by_id`].
#[linkme::distributed_slice]
pub static REGISTRY: [TraceSite] = [..];

/// The distinct registry keys in this binary, sorted. **A key's id is its index
/// here.**
///
/// Deriving ids from the sorted set of keys is what makes them stable: an id
/// depends on nothing but the keys themselves, so it survives rebuilds, edits
/// anywhere in the source, moving functions around, and differing profiles
/// (debug vs release) alike.
///
/// Ids stay dense `0..n`, so [`crate::codec`]'s delta encoding stays compact --
/// the alternative, hashing each key into a build-independent id, would have
/// scattered ids across `u64` and bloated every encoded string. Sorting by key
/// also clusters related functions (a module's functions land on consecutive
/// ids), which makes a module-shaped subset encode especially small.
///
/// Adding or removing a `#[traceable]` *key* renumbers the ids after it, since
/// that genuinely changes the set being indexed. That's the only thing that
/// requires re-encoding.
pub fn names_by_id() -> &'static [&'static str] {
    &groups().0
}

/// The registry key with the given id, or `None` if the id is out of range for
/// this binary.
#[must_use]
pub fn name_by_id(id: u64) -> Option<&'static str> {
    usize::try_from(id)
        .ok()
        .and_then(|id| names_by_id().get(id))
        .copied()
}

/// The id of a registry key, or `None` if this binary has no such key.
#[must_use]
pub fn id_of_name(name: &str) -> Option<u64> {
    names_by_id().binary_search(&name).ok().map(|id| id as u64)
}

/// Every trace site paired with its id, grouped so that sites sharing a key
/// share an id and are always toggled together.
///
/// Used by [`crate::instrumentation`] to apply a decoded id set: it walks this
/// once, so a key with two call sites flips both bits.
pub(crate) fn sites_by_id() -> &'static [Vec<&'static TraceSite>] {
    &groups().1
}

/// The sorted distinct keys and, positionally aligned with them, the sites
/// carrying each key. Built once; the registry is fixed at link time.
fn groups() -> &'static (Vec<&'static str>, Vec<Vec<&'static TraceSite>>) {
    static GROUPS: LazyLock<(Vec<&'static str>, Vec<Vec<&'static TraceSite>>)> =
        LazyLock::new(|| {
            let mut names: Vec<&'static str> = REGISTRY.iter().map(|site| site.name).collect();
            names.sort_unstable();
            names.dedup();
            let sites = names
                .iter()
                .map(|name| REGISTRY.iter().filter(|s| s.name == *name).collect())
                .collect();
            (names, sites)
        });
    &GROUPS
}
