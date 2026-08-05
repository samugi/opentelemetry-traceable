//! Standalone helper to turn a list of trace-site ids into a compact encoded
//! string for `Instrumentation::set_enabled_encoded` (and friends) -- no Rust
//! required on the caller's side, just this binary.
//!
//! This tool only encodes the ids it's given; it has no access to any
//! particular application's registry. Ids are registry indices, so both the
//! id list and the encoded string are only meaningful for the exact binary
//! that produced them. To find out which functions an application knows about
//! (and their ids), call `stylus::catalog::catalog_json()` *from within that
//! application* -- see the repo README for the full workflow.

mod graph;

use std::io::{self, Read};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use stylus::codec;

#[derive(Parser)]
#[command(name = "stylus-cli", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Encode a list of trace-site ids into a compact, lossless string for
    /// `Instrumentation::set_enabled_encoded` (and friends).
    ///
    /// Reads ids from stdin (whitespace/newline separated) if --ids is not
    /// given. This tool is standalone: it only encodes the ids it's given, and
    /// ids are registry indices, so the output is valid only for the same
    /// binary the ids came from. To get the list of functions an application
    /// knows about (and their ids) for a human or LLM to pick from, call
    /// `stylus::catalog::catalog_json()` from within that application.
    Encode {
        /// Ids to enable, e.g. read from a catalog dump. If omitted, ids are
        /// read from stdin (whitespace or newline separated).
        #[arg(long, num_args = 1..)]
        ids: Option<Vec<u64>>,
    },

    /// Augment a catalog node dump with call-graph edges by statically
    /// parsing the application's source (no compile, no run).
    ///
    /// Reads the `{name, id}` node list produced by the app's own
    /// `stylus::catalog::catalog_json()`, resolves direct calls between
    /// traceable functions from source, and prints the catalog with
    /// `callers`/`callees` per function plus an `unresolved_calls` worklist
    /// for calls static analysis can't pin down (dynamic dispatch, macros).
    Graph {
        /// Path to the catalog node dump (from `... catalog > catalog.json`).
        #[arg(long)]
        catalog: PathBuf,

        /// Root of the application's source tree to scan (e.g. `./src`).
        #[arg(long)]
        src: PathBuf,
    },
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Encode { ids } => {
            println!("{}", encode(ids));
            ExitCode::SUCCESS
        }
        Command::Graph { catalog, src } => match graph::run(&catalog, &src) {
            Ok(json) => {
                println!("{json}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("stylus-cli graph: {e}");
                ExitCode::FAILURE
            }
        },
    }
}

fn encode(ids: Option<Vec<u64>>) -> String {
    let mut ids = ids.unwrap_or_else(|| {
        let mut input = String::new();
        io::stdin()
            .read_to_string(&mut input)
            .unwrap_or_else(|e| panic!("failed to read stdin: {e}"));
        input
            .split_whitespace()
            .map(|tok| {
                tok.parse::<u64>()
                    .unwrap_or_else(|e| panic!("invalid id `{tok}`: {e}"))
            })
            .collect()
    });
    codec::encode(&mut ids)
}
