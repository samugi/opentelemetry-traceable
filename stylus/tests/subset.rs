//! Integration tests for the Bloom filter subset encoding.

use stylus::subset;

const UNIVERSE_SIZE: usize = 100_000;
const CHOSEN_COUNT: usize = 100;
const FP_RATE: f64 = 0.01;

fn universe() -> Vec<String> {
    (0..UNIVERSE_SIZE).map(|i| format!("fn_{i}")).collect()
}

fn chosen(universe: &[String]) -> Vec<&str> {
    universe
        .iter()
        .step_by(UNIVERSE_SIZE / CHOSEN_COUNT)
        .take(CHOSEN_COUNT)
        .map(String::as_str)
        .collect()
}

#[test]
fn every_chosen_name_is_a_member_no_false_negatives() {
    let universe = universe();
    let chosen = chosen(&universe);
    let blob = subset::encode(chosen.iter().copied(), FP_RATE);

    for name in &chosen {
        assert!(
            subset::contains(&blob, name).unwrap(),
            "expected `{name}` to be a member, but it wasn't"
        );
    }
}

#[test]
fn false_positive_rate_stays_within_a_generous_bound_of_the_target() {
    let universe = universe();
    let chosen = chosen(&universe);
    let chosen_set: std::collections::HashSet<&str> = chosen.iter().copied().collect();
    let blob = subset::encode(chosen.iter().copied(), FP_RATE);

    let not_chosen: Vec<&str> = universe
        .iter()
        .map(String::as_str)
        .filter(|n| !chosen_set.contains(n))
        .collect();

    let false_positives = not_chosen
        .iter()
        .filter(|name| subset::contains(&blob, name).unwrap())
        .count();
    let rate = false_positives as f64 / not_chosen.len() as f64;

    // Generous bound (5x the target) to avoid the test being sensitive to
    // incidental constant choices in the sizing formula.
    assert!(
        rate < FP_RATE * 5.0,
        "false-positive rate {rate} exceeded 5x the {FP_RATE} target"
    );
}

#[test]
fn blob_size_regression_guard() {
    let universe = universe();
    let chosen = chosen(&universe);
    let blob = subset::encode(chosen.iter().copied(), FP_RATE);

    assert!(
        blob.len() < 300,
        "blob grew to {} chars for {CHOSEN_COUNT} of {UNIVERSE_SIZE} names at {FP_RATE} fp rate",
        blob.len()
    );
}

#[test]
fn encode_by_name_and_by_id_produce_the_same_blob() {
    let universe = universe();
    let chosen = chosen(&universe);
    let by_name = subset::encode(chosen.iter().copied(), FP_RATE);
    let ids: Vec<u64> = chosen.iter().map(|n| subset::id_of(n)).collect();
    let by_id = subset::encode_ids(ids, FP_RATE);

    assert_eq!(by_name, by_id);
}

#[test]
fn corrupt_blob_returns_err_instead_of_panicking() {
    assert!(subset::contains("not valid base64!!", "anything").is_err());

    let universe = universe();
    let chosen = chosen(&universe);
    let blob = subset::encode(chosen.iter().copied(), FP_RATE);
    let truncated = &blob[..blob.len() / 2];
    assert!(subset::contains(truncated, "anything").is_err());
}

#[test]
fn empty_subset_matches_nothing() {
    let blob = subset::encode(std::iter::empty(), FP_RATE);
    for name in ["a", "b", "some::fn"] {
        assert!(!subset::contains(&blob, name).unwrap());
    }
}
