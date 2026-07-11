//! Standalone helper to turn a list of function names or ids into a blob for
//! `stylus::config::set_enabled_encoded` (and friends) -- no Rust required on
//! the caller's side, just this binary.
//!
//! This tool only hashes/encodes what it's given; it has no access to any
//! particular application's registry. To find out which functions an
//! application actually knows about (and their ids), call
//! `stylus::schema::schema_json()` *from within that application* -- see the
//! repo README for the full workflow.

use std::io::{self, Read};

use clap::{Parser, Subcommand};
use stylus::subset;

#[derive(Parser)]
#[command(name = "stylus-cli", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Encode function names or ids into a compact blob for
    /// `stylus::config::set_enabled_encoded` (and friends).
    ///
    /// Reads names from stdin (one per line) if neither --names nor --ids is
    /// given. This tool is standalone: it only hashes/encodes what it's
    /// given. To get the list of functions an application actually knows
    /// about (and their ids) for a human or LLM to pick from, call
    /// `stylus::schema::schema_json()` from within that application.
    Encode {
        /// Registry names to enable (mutually exclusive with --ids).
        #[arg(long, num_args = 1.., conflicts_with = "ids")]
        names: Option<Vec<String>>,

        /// Precomputed ids to enable, e.g. read from a schema dump
        /// (mutually exclusive with --names).
        #[arg(long, num_args = 1.., conflicts_with = "names")]
        ids: Option<Vec<u64>>,

        /// Target false-positive rate for the Bloom filter.
        #[arg(long, default_value_t = 0.01)]
        fp_rate: f64,
    },
}

fn main() {
    let cli = Cli::parse();
    let Command::Encode {
        names,
        ids,
        fp_rate,
    } = cli.command;

    let blob = match (names, ids) {
        (Some(names), None) => subset::encode(names.iter().map(String::as_str), fp_rate),
        (None, Some(ids)) => subset::encode_ids(ids, fp_rate),
        (None, None) => {
            let mut input = String::new();
            io::stdin()
                .read_to_string(&mut input)
                .unwrap_or_else(|e| panic!("failed to read stdin: {e}"));
            let names: Vec<&str> = input
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .collect();
            subset::encode(names, fp_rate)
        }
        (Some(_), Some(_)) => unreachable!("clap enforces --names/--ids are mutually exclusive"),
    };
    println!("{blob}");
}
