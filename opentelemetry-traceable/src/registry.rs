//! Link-time-collected registry of every `#[traceable]` function.

use std::sync::LazyLock;
use std::sync::atomic::AtomicU64;

/// Each function marked with `#[traceable]` is a trace site.
///
/// A trace site is identified by its [`name`](Self::name). The `name` is the key
/// used to dynamically enable/disable tracing for trace sites (see [`crate::selector`]).
#[derive(Debug)]
pub struct TraceSite {
    /// The registry key used to enable/disable this function. Defaults to
    /// `module_path!() + "::" + fn_name`, or the macro's `name` argument.
    pub name: &'static str,

    /// The site's `doc` (from the macro's `doc` argument), or `""` if it has none.
    pub doc: &'static str,

    /// Bitmask of which instrumentation slots currently have this function
    /// **enabled for tracing**. bit `N` corresponds to slot number `N` in the
    /// [`crate::instrumentation::Instrumentation`].
    pub enabled_slots: AtomicU64,
}

impl TraceSite {
    /// Creates a new registry entry for `name` and `doc`, with every slot
    /// disabled.
    pub const fn new(name: &'static str, doc: &'static str) -> Self {
        Self {
            name,
            doc,
            enabled_slots: AtomicU64::new(0),
        }
    }
}

/// Every `#[traceable]` function linked into the current binary.
#[linkme::distributed_slice]
pub static REGISTRY: [TraceSite] = [..];

/// One trace site as reported by [`sites`]: its registry key and its doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SiteInfo {
    /// The registry key used to enable/disable this site.
    pub key: &'static str,
    /// The site's `doc`, or `""` if it has none.
    pub doc: &'static str,
}

/// Every distinct registry key linked into this binary, sorted and deduplicated.
pub fn keys() -> &'static [&'static str] {
    static KEYS: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
        let mut keys: Vec<&'static str> = REGISTRY.iter().map(|site| site.name).collect();
        keys.sort_unstable();
        keys.dedup();
        keys
    });
    &KEYS
}

/// Every `#[traceable]` site linked into this binary: its registry key and its
/// doc, together. Sorted and deduplicated by key, like [`keys`].
///
/// Whatever decides which sites to enable (a person, a script, ...) starts from here.
pub fn sites() -> &'static [SiteInfo] {
    static SITES: LazyLock<Vec<SiteInfo>> = LazyLock::new(|| {
        let mut sites: Vec<SiteInfo> = REGISTRY
            .iter()
            .map(|site| SiteInfo {
                key: site.name,
                doc: site.doc,
            })
            .collect();
        sites.sort_unstable_by_key(|site| site.key);
        sites.dedup_by_key(|site| site.key);
        sites
    });
    &SITES
}
