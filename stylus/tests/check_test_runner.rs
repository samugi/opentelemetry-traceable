//! Asserts this suite is run with `cargo nextest run`, not plain `cargo test`.
//!
//! Several tests in this crate mutate process-global state (the OTel tracer
//! provider, the `linkme`-collected registry) and rely on nextest's
//! process-per-test isolation to avoid interfering with each other --
//! `cargo test`'s thread-per-test model would let that state leak between
//! tests.

#[test]
fn check_test_runner() {
    assert_eq!(
        std::env::var("NEXTEST").unwrap_or_default(),
        "1",
        "run this suite with `cargo nextest run` (e.g. `mise run test`), not `cargo test`"
    );
}
