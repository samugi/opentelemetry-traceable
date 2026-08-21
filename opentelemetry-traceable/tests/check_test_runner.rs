//! Asserts this suite is run with `cargo nextest run`, not plain `cargo test`.
//!
//! A `#[traceable]` function's enabled bit lives in the
//! `linkme`-collected registry, which is process-global. Since the tests share
//! the same traced functions, one test enabling a function means *any*
//! concurrently-running test that calls it also emits a span into the first
//! test's exporter. These tests therefore rely on nextest's process-per-test
//! isolation; `cargo test`'s thread-per-test model lets those bits leak between
//! tests and reports spurious extra spans.

#[test]
fn check_test_runner() {
    assert_eq!(
        std::env::var("NEXTEST").unwrap_or_default(),
        "1",
        "run this suite with `cargo nextest run` (e.g. `mise run test`), not `cargo test`"
    );
}
