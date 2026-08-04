//! Round-trip and robustness tests for the lossless subset codec.

use stylus::codec::{decode, encode};

#[test]
fn round_trips_an_arbitrary_id_set() {
    let mut ids = vec![0u64, 3, 7, 42, 1_000, 1_000_000];
    let expected = ids.clone();
    let encoded = encode(&mut ids);
    assert_eq!(decode(&encoded).unwrap(), expected);
}

#[test]
fn sorts_and_is_order_independent() {
    let a = encode(&mut [9, 1, 5, 3]);
    let b = encode(&mut [1, 3, 5, 9]);
    assert_eq!(a, b, "encoding is independent of input order");
    assert_eq!(decode(&a).unwrap(), vec![1, 3, 5, 9]);
}

#[test]
fn empty_set_round_trips_to_empty() {
    let encoded = encode(&mut []);
    assert!(encoded.is_empty());
    assert_eq!(decode(&encoded).unwrap(), Vec::<u64>::new());
}

#[test]
fn duplicates_encode_as_zero_deltas_and_decode_back_as_repeats() {
    // Zero deltas are legal; apply-side set semantics make the repeats moot.
    let encoded = encode(&mut [4, 4, 4]);
    assert_eq!(decode(&encoded).unwrap(), vec![4, 4, 4]);
}

#[test]
fn stays_compact_for_a_dense_run() {
    // 1000 consecutive ids -> 1000 one-byte deltas -> well under 2 KiB base64.
    let mut ids: Vec<u64> = (0..1000).collect();
    let encoded = encode(&mut ids);
    assert!(
        encoded.len() < 1400,
        "dense run encoded to {} chars",
        encoded.len()
    );
}

#[test]
fn corrupt_input_returns_err_instead_of_panicking() {
    assert!(decode("not valid base64!!").is_err());
    // "gA" is the URL-safe base64 of the single byte 0x80: a LEB128 varint
    // whose continuation bit is set but with no following byte -- malformed.
    assert!(decode("gA").is_err());
}

#[test]
fn is_url_safe_and_unpadded() {
    let encoded = encode(&mut [1, 2, 3, 300, 70_000]);
    assert!(
        encoded
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "must be URL-safe base64 with no padding: {encoded}"
    );
}
