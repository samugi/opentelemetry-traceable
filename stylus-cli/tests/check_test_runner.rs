//! Asserts this suite is run with `cargo nextest run`, not plain `cargo test`.

#[test]
fn check_test_runner() {
    assert_eq!(
        std::env::var("NEXTEST").unwrap_or_default(),
        "1",
        "run this suite with `cargo nextest run` (e.g. `mise run test`), not `cargo test`"
    );
}
