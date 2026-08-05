//! A machine-readable dump of every `#[traceable]` function currently linked
//! into this binary, alongside the id [`crate::codec`] uses to encode it.
//!
//! This is what lets a consumer *without* running Rust -- an LLM with source
//! access, a script, an operator -- independently pick a subset and build the
//! exact encoded string
//! [`Instrumentation::set_enabled_encoded`](crate::instrumentation::Instrumentation::set_enabled_encoded)
//! expects: read this catalog, pick functions by name or id, and either call
//! [`crate::codec::encode`], or hand the chosen ids to `stylus-cli encode`.
//!
//! An `id` is the function's index in the sorted set of registry keys (see
//! [`crate::registry::names_by_id`]) -- dense, and stable across rebuilds and
//! build profiles. Regenerate this catalog when you add or remove a
//! `#[traceable]` function, which renumbers the ids after it.
//!
//! `stylus` has no way to know how a given application wants to expose this
//! (an admin endpoint, a debug CLI flag, a one-off script) -- it only
//! provides the data.

use serde::Serialize;

use crate::registry;

/// A single `#[traceable]` function's registry key and id, as reported by
/// [`catalog`].
///
/// This is the *node* list only. Call-graph edges (who calls whom) aren't
/// known to the running binary -- they're added by static source analysis
/// (`stylus-cli graph`), which augments this dump with `callers`/`callees`.
#[derive(Debug, Serialize)]
pub struct FunctionEntry {
    /// The registry key -- `module_path!() + "::" + fn_name`, or the
    /// `#[traceable]` macro's `name` argument.
    pub name: &'static str,
    /// The function's stable id -- its index in
    /// [`crate::registry::names_by_id`], and the value
    /// [`crate::codec::encode`] delta-compresses to build an enabled set.
    pub id: u64,
}

/// Every `#[traceable]` function linked into the current binary, with its id.
/// See [`catalog`].
#[derive(Debug, Serialize)]
pub struct Catalog {
    /// Every `#[traceable]` function linked into the current binary.
    pub functions: Vec<FunctionEntry>,
}

/// Every `#[traceable]` function linked into the current binary, with its id.
pub fn catalog() -> Catalog {
    Catalog {
        functions: registry::names_by_id()
            .iter()
            .enumerate()
            .map(|(id, name)| FunctionEntry {
                name,
                id: id as u64,
            })
            .collect(),
    }
}

/// [`catalog`], serialized as pretty-printed JSON.
pub fn catalog_json() -> String {
    serde_json::to_string_pretty(&catalog()).expect("Catalog serialization is infallible")
}

/// Every registry key known so far -- one per `#[traceable]` function linked
/// into the current binary. Just the names; see [`catalog`] for the ids that go
/// with them.
pub fn all_names() -> impl Iterator<Item = &'static str> {
    registry::names_by_id().iter().copied()
}
