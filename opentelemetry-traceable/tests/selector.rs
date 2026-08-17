//! Tests for selecting trace sites by key and by `*` glob.
//!
//! [`matches`] is pure, so most of this is a table. The [`resolve`] tests run
//! against this test binary's own registry -- each integration test file links its
//! own `#[traceable]` set, so the fixtures below are the *only* keys here, which
//! makes exact assertions about the whole key list possible.
//!
//! [`matches`]: opentelemetry_traceable::selector::matches
//! [`resolve`]: opentelemetry_traceable::selector::resolve

use opentelemetry_traceable::selector::{self, is_glob, matches};
use opentelemetry_traceable::{registry, traceable};

#[traceable(name = "sel::a::one")]
fn a_one() -> u64 {
    1
}

#[traceable(name = "sel::a::two")]
fn a_two() -> u64 {
    2
}

#[traceable(name = "sel::a::deep::three")]
fn a_deep_three() -> u64 {
    3
}

#[traceable(name = "sel::b::four")]
fn b_four() -> u64 {
    4
}

/// Every key this binary knows, in the sorted order `registry::keys()` promises.
fn all_keys() -> Vec<&'static str> {
    vec![
        "sel::a::deep::three",
        "sel::a::one",
        "sel::a::two",
        "sel::b::four",
    ]
}

#[test]
fn fixtures_register_exactly_the_expected_keys() {
    assert_eq!(a_one() + a_two() + a_deep_three() + b_four(), 10);
    assert_eq!(registry::keys().to_vec(), all_keys());
}

// ---------------------------------------------------------------------------
// matches
// ---------------------------------------------------------------------------

#[test]
fn a_glob_spans_module_separators() {
    // The whole point of the single-`*` rule: nesting depth is invisible to
    // whoever is selecting.
    assert!(matches("sel::a::*", "sel::a::one"));
    assert!(matches("sel::a::*", "sel::a::deep::three"));
    assert!(matches("sel::*", "sel::a::deep::three"));
    // `sel::a::*` requires *something* after the separator.
    assert!(!matches("sel::a::*", "sel::a"));
    assert!(!matches("sel::a::*", "sel::b::four"));
}

#[test]
fn globs_match_in_the_middle_and_alone() {
    assert!(matches("*::db::*", "crate::domain::db::users::insert"));
    assert!(matches("sel::*::three", "sel::a::deep::three"));
    assert!(matches("*", "anything::at::all"));
    assert!(matches("*", ""));
}

#[test]
fn a_selector_without_a_glob_is_plain_equality() {
    assert!(matches("custom.span", "custom.span"));
    assert!(!matches("custom.span", "custom.spanX"));
    assert!(!matches("custom.span", "Xcustom.span"));
    assert!(matches("", ""));
    assert!(!matches("", "x"));
}

#[test]
fn the_literals_around_a_glob_are_anchored_at_both_ends() {
    assert!(matches("a*c", "abc"));
    assert!(matches("a*c", "ac"));
    assert!(!matches("a*c", "abcd"), "trailing literal is anchored");
    assert!(!matches("a*c", "xabc"), "leading literal is anchored");
}

#[test]
fn the_trailing_literal_cannot_overlap_an_earlier_one() {
    // Both `aa`s are findable in `aaa`, but they'd have to share a character.
    assert!(!matches("*aa*aa", "aaa"));
    assert!(matches("*aa*aa", "aaaa"));
    assert!(matches("*ab*ab", "abab"));
}

#[test]
fn adjacent_globs_are_a_no_op() {
    assert!(matches("a**b", "aXb"));
    assert!(matches("a**b", "ab"));
}

#[test]
fn is_glob_detects_the_wildcard() {
    assert!(is_glob("sel::a::*"));
    assert!(is_glob("*"));
    assert!(!is_glob("sel::a::one"));
    assert!(!is_glob(""));
}

// ---------------------------------------------------------------------------
// resolve
// ---------------------------------------------------------------------------

#[test]
fn resolve_expands_a_glob_into_sorted_keys() {
    let selection = selector::resolve(&["sel::a::*"]).expect("every selector is known");
    assert_eq!(
        selection.keys,
        vec!["sel::a::deep::three", "sel::a::one", "sel::a::two"]
    );
    assert!(selection.unmatched_globs.is_empty());
}

#[test]
fn a_bare_glob_selects_every_key() {
    let selection = selector::resolve(&["*"]).expect("every selector is known");
    assert_eq!(selection.keys, all_keys());
}

#[test]
fn resolve_names_every_unknown_exact_key_not_just_the_first() {
    let error = selector::resolve(&["sel::a::one", "sel::nope", "sel::also_nope"])
        .expect_err("two selectors are typos");
    assert_eq!(error.keys, vec!["sel::nope", "sel::also_nope"]);
    let rendered = error.to_string();
    assert!(rendered.contains("sel::nope"), "{rendered}");
    assert!(rendered.contains("sel::also_nope"), "{rendered}");
}

#[test]
fn a_glob_matching_nothing_is_reported_but_not_an_error() {
    let selection =
        selector::resolve(&["sel::nothing::*"]).expect("a glob is never an error, only a report");
    assert!(selection.keys.is_empty());
    assert_eq!(selection.unmatched_globs, vec!["sel::nothing::*"]);
}

#[test]
fn a_mixed_glob_list_still_applies_the_half_that_matched() {
    let selection =
        selector::resolve(&["sel::b::*", "sel::nothing::*"]).expect("globs never error");
    assert_eq!(selection.keys, vec!["sel::b::four"]);
    assert_eq!(selection.unmatched_globs, vec!["sel::nothing::*"]);
}

#[test]
fn overlapping_selectors_yield_each_key_once() {
    let selection =
        selector::resolve(&["*", "sel::a::*", "sel::a::one"]).expect("every selector is known");
    assert_eq!(selection.keys, all_keys());
    assert!(selection.unmatched_globs.is_empty());
}

#[test]
fn blank_selectors_are_ignored_rather_than_unknown() {
    let selection =
        selector::resolve(&["", "   ", "sel::b::four"]).expect("blanks are skipped, not resolved");
    assert_eq!(selection.keys, vec!["sel::b::four"]);
    assert!(selection.unmatched_globs.is_empty());
}

#[test]
fn selectors_are_trimmed() {
    let selection = selector::resolve(&["  sel::b::four  "]).expect("trimmed before lookup");
    assert_eq!(selection.keys, vec!["sel::b::four"]);
}

#[test]
fn resolving_nothing_selects_nothing() {
    let selection = selector::resolve::<&str>(&[]).expect("an empty list is valid");
    assert!(selection.keys.is_empty());
    assert!(selection.unmatched_globs.is_empty());
}
