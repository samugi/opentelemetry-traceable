//! Runtime API to enable/disable `#[traceable]` functions by registry key.
//!
//! This drives the always-present **default instrumentation** (see
//! [`crate::instrumentation`]) -- exactly the behavior stylus has always had.
//! For multiple, independently-configured tracing configurations over the same
//! functions, create named [`crate::instrumentation::Instrumentation`]s.
//!
//! All of these walk the full `REGISTRY` (bounded by the number of
//! `#[traceable]` call sites in the program, not by call frequency) and are
//! meant to be called rarely — e.g. on startup or on a config-reload event —
//! not on any hot path.

use crate::instrumentation::{self as instr, DEFAULT_SLOT};
use crate::registry::REGISTRY;

// The lossless subset codec lives in [`crate::codec`]; re-export its surface
// here so the whole enable/disable-by-id workflow is reachable through
// `stylus::config` (`encode` to build a string, `set_enabled_encoded` to apply
// one, `decode` to inspect one).
pub use crate::codec::{DecodeError, decode, encode};

/// Replace the active set: exactly the given names are enabled, everything
/// else is disabled.
pub fn set_enabled<'a, I: IntoIterator<Item = &'a str>>(names: I) {
    instr::slot_set_enabled(names, DEFAULT_SLOT);
}

/// Enable the given names, leaving the rest of the current set untouched.
pub fn enable<'a, I: IntoIterator<Item = &'a str>>(names: I) {
    instr::slot_enable(names, DEFAULT_SLOT);
}

/// Disable the given names, leaving the rest of the current set untouched.
pub fn disable<'a, I: IntoIterator<Item = &'a str>>(names: I) {
    instr::slot_disable(names, DEFAULT_SLOT);
}

/// Enable every known `#[traceable]` function.
pub fn enable_all() {
    instr::slot_enable_all(DEFAULT_SLOT);
}

/// Disable every known `#[traceable]` function.
pub fn disable_all() {
    instr::slot_disable_all(DEFAULT_SLOT);
}

/// Whether a given registry key is currently enabled.
pub fn is_enabled(name: &str) -> bool {
    instr::slot_is_enabled(name, DEFAULT_SLOT)
}

/// All registry keys known so far — every `#[traceable]` function linked
/// into the current binary.
pub fn all_names() -> impl Iterator<Item = &'static str> {
    REGISTRY.iter().map(|site| site.name)
}

/// Registry keys that are currently enabled.
pub fn enabled_names() -> impl Iterator<Item = &'static str> {
    instr::slot_enabled_names(DEFAULT_SLOT)
}

/// Replace the active set from a string produced by [`encode`] (see
/// [`set_enabled`] for exact-name semantics; this is the same, decoded from a
/// compact id list instead).
pub fn set_enabled_encoded(encoded: &str) -> Result<(), DecodeError> {
    instr::slot_set_enabled_encoded(encoded, DEFAULT_SLOT)
}

/// Enable whatever's in the encoded id list, leaving the rest of the current
/// set untouched (see [`enable`]).
pub fn enable_encoded(encoded: &str) -> Result<(), DecodeError> {
    instr::slot_enable_encoded(encoded, DEFAULT_SLOT)
}

/// Disable whatever's in the encoded id list, leaving the rest of the current
/// set untouched (see [`disable`]).
pub fn disable_encoded(encoded: &str) -> Result<(), DecodeError> {
    instr::slot_disable_encoded(encoded, DEFAULT_SLOT)
}

/// Replace the *child-only* set: exactly the given names are put in child-only
/// mode, every other function is made root-capable again.
///
/// A child-only function only produces a span when called from within an
/// already-recording span -- never as a root, even when enabled. This is
/// orthogonal to [`set_enabled`]: a function must be enabled to trace at all,
/// and being child-only additionally suppresses it when it would otherwise be
/// a root. Whether a shared function should be child-only is request-relative,
/// so it's decided here rather than at the call site.
pub fn set_child_only<'a, I: IntoIterator<Item = &'a str>>(names: I) {
    instr::slot_set_child_only(names, DEFAULT_SLOT);
}

/// Replace the child-only set from a string produced by [`encode`] (see
/// [`set_child_only`] for the semantics; this is the same, decoded from a
/// compact id list instead).
pub fn set_child_only_encoded(encoded: &str) -> Result<(), DecodeError> {
    instr::slot_set_child_only_encoded(encoded, DEFAULT_SLOT)
}

/// Registry keys currently in child-only mode.
pub fn child_only_names() -> impl Iterator<Item = &'static str> {
    instr::slot_child_only_names(DEFAULT_SLOT)
}
