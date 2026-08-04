//! Integration tests for the `stylus-cli` binary.

use std::io::Write;
use std::process::{Command, Stdio};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_stylus-cli")
}

fn run(args: &[&str], stdin: Option<&str>) -> (String, String, i32) {
    let mut cmd = Command::new(bin());
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("failed to spawn stylus-cli");
    if let Some(input) = stdin {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    let output = child
        .wait_with_output()
        .expect("failed to wait on stylus-cli");
    (
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
        output.status.code().unwrap_or(-1),
    )
}

#[test]
fn encode_ids_prints_a_non_empty_string() {
    let (stdout, _stderr, code) = run(&["encode", "--ids", "1", "5", "9"], None);
    assert_eq!(code, 0);
    assert!(!stdout.trim().is_empty());
}

#[test]
fn encode_ids_round_trips_and_is_order_independent() {
    let (ascending, _, code1) = run(&["encode", "--ids", "1", "5", "9"], None);
    let (shuffled, _, code2) = run(&["encode", "--ids", "9", "1", "5"], None);

    assert_eq!(code1, 0);
    assert_eq!(code2, 0);
    // Encoding sorts, so order in doesn't change the output.
    assert_eq!(ascending.trim(), shuffled.trim());
    assert_eq!(
        stylus::codec::decode(ascending.trim()).unwrap(),
        vec![1, 5, 9]
    );
}

#[test]
fn encode_reads_ids_from_stdin_when_flag_omitted() {
    let (from_args, _, _) = run(&["encode", "--ids", "1", "2", "3"], None);
    let (from_stdin, _, code) = run(&["encode"], Some("1\n2\n3\n"));

    assert_eq!(code, 0);
    assert_eq!(from_args.trim(), from_stdin.trim());
}

#[test]
fn unknown_subcommand_fails_with_usage() {
    let (_stdout, stderr, code) = run(&["nope"], None);
    assert_eq!(code, 2);
    assert!(stderr.contains("Usage: stylus-cli"));
}

#[test]
fn encode_rejects_non_numeric_ids() {
    let (_stdout, _stderr, code) = run(&["encode", "--ids", "not-a-number"], None);
    // clap rejects a non-u64 value for --ids before the command even runs.
    assert_eq!(code, 2);
}
