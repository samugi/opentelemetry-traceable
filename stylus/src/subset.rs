//! Compact encoding of an arbitrary subset of [`crate::registry::REGISTRY`] as a
//! Bloom filter, so enabling e.g. 100 of 100,000 functions doesn't require
//! shipping 100 fully-qualified names around.
//!
//! # Why a Bloom filter
//!
//! Membership is tested directly against each function's own name string --
//! there's no positional index and no manifest to keep in sync with the
//! registry, so a blob stays valid even if the registry's size or link order
//! changes between builds. The tradeoff: a Bloom filter guarantees **zero
//! false negatives** (every requested function really is enabled) at the cost
//! of a small, tunable chance of **false positives** (a handful of other
//! functions also get enabled).
//!
//! # Reproducing this without Rust
//!
//! Everything below is intentionally simple enough to reimplement from a short
//! script in any language, given [`crate::catalog::catalog_json`]'s `{name, id}`
//! list:
//!
//! 1. **Hash**: 64-bit FNV-1a over the name's UTF-8 bytes (offset basis
//!    `0xcbf29ce484222325`, prime `0x100000001b3`). This is the `id` in the
//!    catalog -- [`id_of`] is exactly this.
//! 2. **Sizing**: given `n` items to encode and a target false-positive rate
//!    `p`, `m = ceil(-n * ln(p) / ln(2)^2)` bits, `k = round((m / n) * ln(2))`
//!    hash rounds.
//! 3. **Bit positions**: split the 64-bit id into two 32-bit halves,
//!    `h1 = id as u32`, `h2 = (id >> 32) as u32`. For `i` in `0..k`:
//!    `idx_i = h1.wrapping_add(i * h2) % m` (Kirsch-Mitzenmacher double
//!    hashing) -- set/check bit `idx_i`.
//! 4. **Wire format**: `[u32 m, little-endian][u8 k][ceil(m/8) bytes, bit i
//!    stored at byte `i/8` bit `i%8`]`, then base64 (URL-safe, unpadded).

use std::f64::consts::LN_2;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// The 64-bit FNV-1a hash of `name` -- the same `id` used internally by
/// [`encode`]/[`encode_ids`] and reported by [`crate::catalog::catalog`].
pub fn id_of(name: &str) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    for byte in name.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Error decoding a blob produced by [`encode`]/[`encode_ids`].
#[derive(Debug)]
pub enum DecodeError {
    /// The blob wasn't valid base64.
    Base64(base64::DecodeError),
    /// The decoded bytes are shorter than a header even needs.
    Truncated,
    /// The decoded bit-array's length doesn't match what the header's `m`
    /// (bit count) implies.
    LengthMismatch {
        /// Bytes the header's `m` implies the bit array should be.
        expected: usize,
        /// Bytes actually present after the header.
        actual: usize,
    },
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::Base64(e) => write!(f, "invalid base64: {e}"),
            DecodeError::Truncated => write!(f, "blob is shorter than its header claims"),
            DecodeError::LengthMismatch { expected, actual } => write!(
                f,
                "blob bit-array length mismatch: header implies {expected} bytes, got {actual}"
            ),
        }
    }
}

impl std::error::Error for DecodeError {}

/// A decoded Bloom filter, used internally to test membership.
pub(crate) struct Filter {
    m: u32,
    k: u8,
    bits: Vec<u8>,
}

impl Filter {
    pub(crate) fn contains_id(&self, id: u64) -> bool {
        let h1 = id as u32;
        let h2 = (id >> 32) as u32;
        for i in 0..u32::from(self.k) {
            let idx = h1.wrapping_add(i.wrapping_mul(h2)) % self.m;
            if !get_bit(&self.bits, idx) {
                return false;
            }
        }
        true
    }
}

fn get_bit(bits: &[u8], idx: u32) -> bool {
    let idx = idx as usize;
    (bits[idx / 8] >> (idx % 8)) & 1 == 1
}

fn set_bit(bits: &mut [u8], idx: u32) {
    let idx = idx as usize;
    bits[idx / 8] |= 1 << (idx % 8);
}

/// Number of bits (`m`) and hash rounds (`k`) for `n` items at false-positive
/// rate `p`, using the standard Bloom-filter sizing formulas. `p` is clamped
/// to a sane range so pathological input can't produce a degenerate filter.
fn params(n: usize, p: f64) -> (u32, u8) {
    if n == 0 {
        return (8, 1);
    }
    let p = p.clamp(1e-6, 0.5);
    let n = n as f64;
    let m = (-n * p.ln() / (LN_2 * LN_2)).ceil().max(8.0);
    let k = ((m / n) * LN_2).round().clamp(1.0, 32.0);
    (m as u32, k as u8)
}

fn build(ids: &[u64], false_positive_rate: f64) -> String {
    let (m, k) = params(ids.len(), false_positive_rate);
    let mut bits = vec![0u8; m.div_ceil(8) as usize];
    for &id in ids {
        let h1 = id as u32;
        let h2 = (id >> 32) as u32;
        for i in 0..u32::from(k) {
            let idx = h1.wrapping_add(i.wrapping_mul(h2)) % m;
            set_bit(&mut bits, idx);
        }
    }
    let mut blob = Vec::with_capacity(5 + bits.len());
    blob.extend_from_slice(&m.to_le_bytes());
    blob.push(k);
    blob.extend_from_slice(&bits);
    URL_SAFE_NO_PAD.encode(blob)
}

/// Encode a subset of function names into a compact, self-describing blob.
///
/// `false_positive_rate` trades size for precision: lower rates produce
/// larger blobs but reduce the chance of enabling unrequested functions.
/// Every named function is always guaranteed to be enabled when this blob is
/// applied -- there are no false negatives.
pub fn encode<'a>(names: impl IntoIterator<Item = &'a str>, false_positive_rate: f64) -> String {
    let ids: Vec<u64> = names.into_iter().map(id_of).collect();
    build(&ids, false_positive_rate)
}

/// Same as [`encode`], but starting from already-computed ids (e.g. read
/// directly from [`crate::catalog::catalog`] instead of hashing names again).
pub fn encode_ids(ids: impl IntoIterator<Item = u64>, false_positive_rate: f64) -> String {
    let ids: Vec<u64> = ids.into_iter().collect();
    build(&ids, false_positive_rate)
}

/// A blob that matches every id -- for "enable/disable everything" without
/// enumerating every function's id. Takes no false-positive rate: there's
/// nothing to tune, it's deterministically "always a member".
///
/// A Bloom filter with every bit set is unconditionally a superset of
/// everything: for any id, whatever bit position `(h1 + i*h2) % m` computes
/// to, that bit is already `1` by construction. So this is just the
/// smallest possible valid blob (`m = 8`, `k = 1`, one all-ones byte) --
/// no special-casing needed anywhere it's consumed (`decode`, `contains_id`,
/// `stylus::config::set_enabled_encoded`/`enable_encoded`/`disable_encoded`
/// all just treat it as an ordinary blob that happens to match everything).
pub fn encode_all() -> String {
    let mut blob = Vec::with_capacity(6);
    blob.extend_from_slice(&8u32.to_le_bytes());
    blob.push(1u8);
    blob.push(0xFF);
    URL_SAFE_NO_PAD.encode(blob)
}

/// Whether `name` is a member of the subset encoded in `blob` -- exposed
/// standalone (not just via `stylus::config`) so a blob can be inspected or
/// tested against a synthetic set of names, without touching the real
/// registry.
pub fn contains(blob: &str, name: &str) -> Result<bool, DecodeError> {
    contains_id(blob, id_of(name))
}

/// Same as [`contains`], but by id instead of name.
pub fn contains_id(blob: &str, id: u64) -> Result<bool, DecodeError> {
    Ok(decode(blob)?.contains_id(id))
}

pub(crate) fn decode(blob: &str) -> Result<Filter, DecodeError> {
    let bytes = URL_SAFE_NO_PAD.decode(blob).map_err(DecodeError::Base64)?;
    if bytes.len() < 5 {
        return Err(DecodeError::Truncated);
    }
    let m = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let k = bytes[4];
    let bits = bytes[5..].to_vec();
    let expected = m.div_ceil(8) as usize;
    if bits.len() != expected {
        return Err(DecodeError::LengthMismatch {
            expected,
            actual: bits.len(),
        });
    }
    Ok(Filter { m, k, bits })
}
