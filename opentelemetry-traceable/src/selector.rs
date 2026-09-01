//! Selecting `#[traceable]` functions by name, using exact match, or `*`
//! pattern match.
//!
//! A trace site's registry key -- `module_path!() + "::" + fn_name`, or the
//! macro's `name` argument -- *is* its identity. There is no id, index, or
//! encoded form: configuration names the functions it wants, the way DTrace
//! names probes, over a probe set that happens to be declared at compile time.
//!
//! # Globs
//!
//! `*` matches any run of characters, **including `::`**. That is the only
//! wildcard, and it has no `**` counterpart: how deeply a function is nested is
//! an implementation detail of the code being traced, not something the operator
//! selecting it should have to track.
//!
//! ```text
//! my_app::domain::db::*   matches  my_app::domain::db::users::insert
//!                         matches  my_app::domain::db::query
//!                     does NOT match  my_app::domain::db      (nothing after `::`)
//! *::db::*                matches  any db function, in any crate or module
//! *                       matches  everything
//! ```
//!
//! # Typos are errors, empty globs are warnings
//!
//! An *exact* selector that matches nothing is almost certainly a typo, so
//! [`resolve`] fails ([`UnknownKeys`]) and callers apply nothing -- tracing an
//! unnoticed subset of what was asked for is worse than refusing. A *glob* that
//! matches nothing is only reported ([`Selection::unmatched_globs`]), since it
//! can legitimately match nothing in a build where those functions were compiled
//! out.

use crate::registry;

/// Whether `selector` is a glob (contains `*`) rather than an exact registry key.
pub fn is_glob(selector: &str) -> bool {
    selector.contains('*')
}

/// Whether `pattern` matches `key`, treating `*` as any run of characters
/// including `::`. With no `*`, this is plain equality.
pub fn matches(pattern: &str, key: &str) -> bool {
    // The literals between the `*`s, in order. The first is anchored to the
    // start, the last to the end, and everything between just has to appear in
    // order.
    let mut literals = pattern.split('*');
    let first = literals
        .next()
        .expect("`split` always yields at least one part");
    let Some(mut rest) = key.strip_prefix(first) else {
        return false;
    };
    let Some(last) = literals.next_back() else {
        // No `*` anywhere, so `first` was the whole pattern and had to consume
        // the whole key.
        return rest.is_empty();
    };
    for middle in literals {
        // Leftmost match is optimal here: every middle literal is flanked by
        // unbounded `*`, so consuming as little as possible leaves the most for
        // what follows. Adjacent `*`s yield an empty literal, which `find`
        // trivially matches at 0 -- a no-op, as it should be.
        match rest.find(middle) {
            Some(at) => rest = &rest[at + middle.len()..],
            None => return false,
        }
    }
    // Anchoring the trailing literal is what keeps overlap out: `*aa*aa` must
    // not match `aaa`, even though both `aa`s can be found in it.
    rest.ends_with(last)
}

/// What a list of selectors resolved to against this binary's registry.
#[derive(Debug, Clone, Default)]
pub struct Selection {
    /// The distinct registry keys matched, sorted -- so anything derived from a
    /// `Selection` is reproducible run to run.
    pub keys: Vec<&'static str>,
    /// Globs that matched no key. Reported rather than fatal: a glob can
    /// legitimately match nothing in a build where those functions were compiled
    /// out.
    pub unmatched_globs: Vec<String>,
}

/// Exact selectors that match no `#[traceable]` function in this binary.
///
/// Returned by [`resolve`] instead of a partial result, so a typo leaves tracing
/// state untouched rather than silently enabling a subset of what was asked for.
#[derive(Debug, Clone)]
pub struct UnknownKeys {
    /// The unrecognized selectors, in the order they were given.
    pub keys: Vec<String>,
}

impl std::fmt::Display for UnknownKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown #[traceable] key(s): {}", self.keys.join(", "))
    }
}

impl std::error::Error for UnknownKeys {}

/// Resolve `selectors` -- exact registry keys, `*` globs, or a mix -- against
/// every `#[traceable]` function linked into this binary.
///
/// Selectors are trimmed, and blank ones are ignored. Nothing is applied here,
/// which also makes this the dry-run entry point: resolve first to preview what a
/// glob would select. To apply a selection, hand the selectors to
/// [`Instrumentation::set_enabled`](crate::instrumentation::Instrumentation::set_enabled)
/// and friends.
///
/// # Errors
///
/// [`UnknownKeys`] if any *exact* selector matches no function. Unmatched *globs*
/// come back in [`Selection::unmatched_globs`] instead.
pub fn resolve<S: AsRef<str>>(selectors: &[S]) -> Result<Selection, UnknownKeys> {
    let keys = registry::keys();
    let mut matched: Vec<&'static str> = Vec::new();
    let mut unknown: Vec<String> = Vec::new();
    let mut unmatched_globs: Vec<String> = Vec::new();

    for selector in selectors {
        let selector = selector.as_ref().trim();
        if selector.is_empty() {
            continue;
        }
        if is_glob(selector) {
            // Comparing lengths is a sound "matched nothing" test even when an
            // earlier selector already covered the same keys: duplicates are
            // pushed here and only collapsed once, at the end.
            let before = matched.len();
            matched.extend(keys.iter().copied().filter(|key| matches(selector, key)));
            if matched.len() == before {
                unmatched_globs.push(selector.to_string());
            }
        } else {
            // `keys` is sorted, so an exact selector is one binary search.
            match keys.binary_search_by(|candidate| (**candidate).cmp(selector)) {
                Ok(at) => matched.push(keys[at]),
                Err(_) => unknown.push(selector.to_string()),
            }
        }
    }

    // All of them, not just the first -- one save should surface every typo.
    if !unknown.is_empty() {
        return Err(UnknownKeys { keys: unknown });
    }

    matched.sort_unstable();
    matched.dedup();
    Ok(Selection {
        keys: matched,
        unmatched_globs,
    })
}
