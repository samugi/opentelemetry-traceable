//! Compact, **lossless** encoding of an arbitrary subset of
//! [`crate::registry::REGISTRY`] as a delta-compressed list of trace-site ids.
//!
//! An `id` is a trace-site's index in [`crate::registry::REGISTRY`] (see
//! [`crate::catalog`]) -- ids are assigned densely and incrementally, so a
//! subset of them sorts into a run of mostly-small gaps. Encoding those gaps
//! (deltas) as LEB128 varints keeps the payload tiny even for large
//! registries, and unlike the Bloom filter this replaced there are **no false
//! positives**: exactly the requested trace sites are enabled, nothing else.
//!
//! # Wire format
//!
//! 1. Sort the ids ascending and take successive differences (the first delta
//!    is the id itself, i.e. the gap from an implicit `0`).
//! 2. Write each delta as an unsigned LEB128 varint.
//! 3. base64 the whole byte string (URL-safe, unpadded).
//!
//! # Caveat: ids move when the set of keys changes
//!
//! An id is an index into the sorted set of registry keys (see
//! [`crate::registry::names_by_id`]), so it depends on nothing but the keys
//! themselves -- stable across rebuilds, edits, and profiles. Adding or removing
//! a `#[traceable]` key renumbers the ids after it, which is the one case that
//! invalidates an encoded string: re-encode against a fresh catalog.
//!
//! Keeping ids dense is what keeps this encoding compact; hashing keys into
//! build-independent ids would scatter them across `u64` and bloat every string.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

/// Error decoding a string produced by [`encode`].
#[derive(Debug)]
pub enum DecodeError {
    /// The input wasn't valid base64.
    Base64(base64::DecodeError),
    /// The base64 decoded fine, but the bytes weren't a well-formed run of
    /// LEB128 varints (a varint ran off the end, or overflowed `u64`).
    Leb128,
    /// The decoded deltas summed past `u64::MAX` -- not producible by
    /// [`encode`], so the input is corrupt.
    Overflow,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::Base64(e) => write!(f, "invalid base64: {e}"),
            DecodeError::Leb128 => write!(f, "malformed LEB128 delta stream"),
            DecodeError::Overflow => write!(f, "decoded ids overflow u64"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Encode a set of trace-site ids into a compact, lossless string suitable for
/// [`Instrumentation::set_enabled_encoded`](crate::instrumentation::Instrumentation::set_enabled_encoded)
/// (and friends).
///
/// `ids` is sorted in place. A duplicate id encodes as a zero delta and
/// decodes back as a repeat, so callers that need set semantics (as
/// [`crate::instrumentation`] does) should treat the decoded list as a set.
/// Every id present is guaranteed to be enabled when this string is applied.
pub fn encode(ids: &mut [u64]) -> String {
    ids.sort_unstable();
    let mut buf = Vec::new();
    let mut prev = 0u64;
    for &id in ids.iter() {
        // ids are sorted, so `id >= prev`; the delta never underflows.
        leb128::write::unsigned(&mut buf, id - prev).expect("writing to a Vec is infallible");
        prev = id;
    }
    URL_SAFE_NO_PAD.encode(buf)
}

/// Decode a string produced by [`encode`] back into the sorted list of ids it
/// carries. The inverse of [`encode`]; used by
/// [`crate::instrumentation`]'s `*_encoded` methods to figure out which trace
/// sites to toggle.
pub fn decode(encoded: &str) -> Result<Vec<u64>, DecodeError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(DecodeError::Base64)?;
    let mut reader = &bytes[..];
    let mut ids = Vec::new();
    let mut acc = 0u64;
    while !reader.is_empty() {
        let delta = leb128::read::unsigned(&mut reader).map_err(|_| DecodeError::Leb128)?;
        acc = acc.checked_add(delta).ok_or(DecodeError::Overflow)?;
        ids.push(acc);
    }
    Ok(ids)
}
