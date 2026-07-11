//! A machine-readable dump of every `#[traceable]` function currently linked
//! into this binary, alongside the id [`crate::subset`] uses to encode it.
//!
//! This is what lets a consumer *without* running Rust -- an LLM with source
//! access, a script, an operator -- independently pick a subset and build the
//! exact blob [`crate::config::set_enabled_encoded`] expects: read this
//! schema, pick functions by name or id, and either call
//! [`crate::subset::encode`]/[`crate::subset::encode_ids`], or hand the
//! chosen names/ids to `stylus-cli encode`.
//!
//! `stylus` has no way to know how a given application wants to expose this
//! (an admin endpoint, a debug CLI flag, a one-off script) -- it only
//! provides the data.

use serde::Serialize;

use crate::registry::REGISTRY;
use crate::subset::id_of;

/// A single `#[traceable]` function's registry key and id, as reported by
/// [`schema`].
#[derive(Debug, Serialize)]
pub struct FunctionEntry {
    /// The registry key -- what `stylus::config`'s exact-name functions
    /// match against, and what `id` is a hash of.
    pub name: &'static str,
    /// `subset::id_of(name)` -- the 64-bit FNV-1a digest fed into the Bloom
    /// filter's bit-position formula.
    pub id: u64,
}

/// Every `#[traceable]` function linked into the current binary, plus the
/// hash/index details needed to reconstruct a [`crate::subset`] blob without
/// this crate. See [`schema`].
#[derive(Debug, Serialize)]
pub struct Schema {
    /// Name of the hash function `id` is computed with.
    pub hash: &'static str,
    /// The formula used to turn an id into a Bloom filter bit index, spelled
    /// out so it's reproducible without linking this crate. See
    /// `subset` module docs for the full wire-format spec.
    pub index_formula: &'static str,
    /// Every `#[traceable]` function linked into the current binary.
    pub functions: Vec<FunctionEntry>,
}

/// Every `#[traceable]` function linked into the current binary, with its id.
pub fn schema() -> Schema {
    Schema {
        hash: "fnv1a64",
        index_formula: "let h1 = id as u32; let h2 = (id >> 32) as u32; idx_i = h1.wrapping_add(i * h2) % m, for i in 0..k",
        functions: REGISTRY
            .iter()
            .map(|site| FunctionEntry {
                name: site.name,
                id: id_of(site.name),
            })
            .collect(),
    }
}

/// [`schema`], serialized as pretty-printed JSON.
pub fn schema_json() -> String {
    serde_json::to_string_pretty(&schema()).expect("Schema serialization is infallible")
}
