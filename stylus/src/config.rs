//! Runtime API to enable/disable `#[traceable]` functions by registry key.
//!
//! All of these walk the full `REGISTRY` (bounded by the number of
//! `#[traceable]` call sites in the program, not by call frequency) and are
//! meant to be called rarely — e.g. on startup or on a config-reload event —
//! not on any hot path.

use std::collections::HashSet;
use std::sync::atomic::Ordering;

use crate::registry::REGISTRY;
use crate::subset::{self, DecodeError};

/// Replace the active set: exactly the given names are enabled, everything
/// else is disabled.
pub fn set_enabled<'a, I: IntoIterator<Item = &'a str>>(names: I) {
    let wanted: HashSet<&str> = names.into_iter().collect();
    for site in REGISTRY.iter() {
        site.enabled
            .store(wanted.contains(site.name), Ordering::Relaxed);
    }
}

/// Enable the given names, leaving the rest of the current set untouched.
pub fn enable<'a, I: IntoIterator<Item = &'a str>>(names: I) {
    let wanted: HashSet<&str> = names.into_iter().collect();
    for site in REGISTRY.iter() {
        if wanted.contains(site.name) {
            site.enabled.store(true, Ordering::Relaxed);
        }
    }
}

/// Disable the given names, leaving the rest of the current set untouched.
pub fn disable<'a, I: IntoIterator<Item = &'a str>>(names: I) {
    let wanted: HashSet<&str> = names.into_iter().collect();
    for site in REGISTRY.iter() {
        if wanted.contains(site.name) {
            site.enabled.store(false, Ordering::Relaxed);
        }
    }
}

/// Enable every known `#[traceable]` function.
pub fn enable_all() {
    for site in REGISTRY.iter() {
        site.enabled.store(true, Ordering::Relaxed);
    }
}

/// Disable every known `#[traceable]` function.
pub fn disable_all() {
    for site in REGISTRY.iter() {
        site.enabled.store(false, Ordering::Relaxed);
    }
}

/// Whether a given registry key is currently enabled.
pub fn is_enabled(name: &str) -> bool {
    REGISTRY
        .iter()
        .find(|site| site.name == name)
        .is_some_and(|site| site.enabled.load(Ordering::Relaxed))
}

/// All registry keys known so far — every `#[traceable]` function linked
/// into the current binary.
pub fn all_names() -> impl Iterator<Item = &'static str> {
    REGISTRY.iter().map(|site| site.name)
}

/// Registry keys that are currently enabled.
pub fn enabled_names() -> impl Iterator<Item = &'static str> {
    REGISTRY
        .iter()
        .filter(|site| site.enabled.load(Ordering::Relaxed))
        .map(|site| site.name)
}

/// Replace the active set from a blob produced by [`crate::subset::encode`]/
/// [`crate::subset::encode_ids`] (see [`set_enabled`] for exact-name
/// semantics; this is the same, decoded from a compact blob instead).
pub fn set_enabled_encoded(blob: &str) -> Result<(), DecodeError> {
    let filter = subset::decode(blob)?;
    for site in REGISTRY.iter() {
        site.enabled.store(
            filter.contains_id(subset::id_of(site.name)),
            Ordering::Relaxed,
        );
    }
    Ok(())
}

/// Enable whatever's in the blob, leaving the rest of the current set
/// untouched (see [`enable`]).
pub fn enable_encoded(blob: &str) -> Result<(), DecodeError> {
    let filter = subset::decode(blob)?;
    for site in REGISTRY.iter() {
        if filter.contains_id(subset::id_of(site.name)) {
            site.enabled.store(true, Ordering::Relaxed);
        }
    }
    Ok(())
}

/// Disable whatever's in the blob, leaving the rest of the current set
/// untouched (see [`disable`]).
pub fn disable_encoded(blob: &str) -> Result<(), DecodeError> {
    let filter = subset::decode(blob)?;
    for site in REGISTRY.iter() {
        if filter.contains_id(subset::id_of(site.name)) {
            site.enabled.store(false, Ordering::Relaxed);
        }
    }
    Ok(())
}
