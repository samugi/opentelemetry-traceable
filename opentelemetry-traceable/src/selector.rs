//! Selecting `#[traceable]` functions by name/key, using exact match,
//! or `*` pattern match.
//!
//! Name/key is either:
//! * `module_path!() + "::" + fn_name`
//! * the macro's `name` argument
//!
//! # Patterns
//!
//! `*` matches any sequence of characters. It acts as a wildcard.
//!
//! ```text
//! my_app::domain::db::*   matches  my_app::domain::db::users::insert
//!                         matches  my_app::domain::db::query
//!                         does NOT match  my_app::domain::db
//! *::db::*                matches  any db function, in any crate or module
//! *                       matches  everything
//! ```
//!
//! # Typos are errors, empty patterns are warnings
//!
//! An *exact* selector that matches nothing is almost certainly a typo, so
//! [`resolve`] fails ([`UnknownKeys`]) and callers apply nothing. A *pattern* that
//! matches nothing is only reported ([`Selection::unmatched_patterns`]).

use crate::registry;

/// Whether `selector` is a pattern (contains `*`) rather than an exact key.
pub fn is_pattern(selector: &str) -> bool {
    selector.contains('*')
}

/// Whether `pattern` matches `key`.
pub fn matches(pattern: &str, key: &str) -> bool {
    let mut pattern_segments = pattern.split('*');
    let first_pattern_segment = pattern_segments
        .next()
        .expect("`split` always yields at least one part");

    let Some(mut key_after_prefix) = key.strip_prefix(first_pattern_segment) else {
        // key does not start with `first_pattern_segment` --> no match
        return false;
    };
    let Some(last_pattern_segment) = pattern_segments.next_back() else {
        // `first_pattern_segment` was the whole pattern, so it must
        // be equal to the whole key for a match.
        return key_after_prefix.is_empty();
    };
    for mid_pattern_segment in pattern_segments {
        // Find each segment and slide the key.
        // If segments are all found in order we have a match.
        match key_after_prefix.find(mid_pattern_segment) {
            Some(at) => key_after_prefix = &key_after_prefix[at + mid_pattern_segment.len()..],
            None => return false,
        }
    }

    key_after_prefix.ends_with(last_pattern_segment)
}

/// Result of a selection (match of a list of selectors).
#[derive(Debug, Clone, Default)]
pub struct Selection {
    /// Registry keys that matched.
    pub keys: Vec<&'static str>,
    /// Patterns that matched no key.
    pub unmatched_patterns: Vec<String>,
}

/// Exact selectors that match no `#[traceable]` function in this binary.
#[derive(Debug, Clone)]
pub struct UnknownKeys {
    /// The unrecognized selectors.
    pub keys: Vec<String>,
}

impl std::fmt::Display for UnknownKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown #[traceable] key(s): {}", self.keys.join(", "))
    }
}

impl std::error::Error for UnknownKeys {}

/// Resolve `selectors`:
///
///  * exact registry keys
///  * `*` patterns
///  * mix of the above
///
/// against every `#[traceable]` function linked into this binary.
///
/// Unmatched *patterns* are returned in [`Selection::unmatched_patterns`].
///
/// # Errors
///
/// [`UnknownKeys`] if any *exact* selector matches no function.
pub fn resolve<S: AsRef<str>>(selectors: &[S]) -> Result<Selection, UnknownKeys> {
    let keys = registry::keys();
    let mut matched: Vec<&'static str> = Vec::new();
    let mut unknown: Vec<String> = Vec::new();
    let mut unmatched_patterns: Vec<String> = Vec::new();

    for selector in selectors {
        let selector = selector.as_ref().trim();
        if selector.is_empty() {
            continue;
        }
        if is_pattern(selector) {
            let before = matched.len();
            matched.extend(keys.iter().copied().filter(|key| matches(selector, key)));
            if matched.len() == before {
                unmatched_patterns.push(selector.to_string());
            }
        } else {
            // `keys` is sorted
            match keys.binary_search_by(|candidate| (**candidate).cmp(selector)) {
                Ok(at) => matched.push(keys[at]),
                Err(_) => unknown.push(selector.to_string()),
            }
        }
    }

    if !unknown.is_empty() {
        return Err(UnknownKeys { keys: unknown });
    }

    matched.sort_unstable();
    matched.dedup();
    Ok(Selection {
        keys: matched,
        unmatched_patterns,
    })
}
