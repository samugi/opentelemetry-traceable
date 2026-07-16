//! Standalone helper to turn a list of function names or ids into a blob for
//! `stylus::config::set_enabled_encoded` (and friends) -- no Rust required on
//! the caller's side, just this binary.
//!
//! This tool only hashes/encodes what it's given; it has no access to any
//! particular application's registry. To find out which functions an
//! application actually knows about (and their ids), call
//! `stylus::catalog::catalog_json()` *from within that application* -- see the
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
    /// `stylus::catalog::catalog_json()` from within that application.
    Encode {
        /// Registry names to enable (mutually exclusive with --ids/--all).
        #[arg(long, num_args = 1.., conflicts_with_all = ["ids", "all"])]
        names: Option<Vec<String>>,

        /// Precomputed ids to enable, e.g. read from a catalog dump
        /// (mutually exclusive with --names/--all).
        #[arg(long, num_args = 1.., conflicts_with_all = ["names", "all"])]
        ids: Option<Vec<u64>>,

        /// Match every function, without listing any names/ids -- for
        /// "enable/disable everything" (mutually exclusive with
        /// --names/--ids).
        #[arg(long, conflicts_with_all = ["names", "ids"])]
        all: bool,

        /// Target false-positive rate for the Bloom filter. Ignored with
        /// --all (there's nothing to tune -- it's deterministically a
        /// match for everything).
        #[arg(long, default_value_t = 0.01)]
        fp_rate: f64,
    },
}

fn main() {
    let cli = Cli::parse();
    let Command::Encode {
        names,
        ids,
        all,
        fp_rate,
    } = cli.command;

    let blob = match (names, ids, all) {
        (_, _, true) => subset::encode_all(),
        (Some(names), None, false) => subset::encode(names.iter().map(String::as_str), fp_rate),
        (None, Some(ids), false) => subset::encode_ids(ids, fp_rate),
        (None, None, false) => {
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
        (Some(_), Some(_), false) => {
            unreachable!("clap enforces --names/--ids are mutually exclusive")
        }
    };
    println!("{blob}");
}
