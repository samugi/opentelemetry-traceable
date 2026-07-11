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
fn encode_names_prints_a_non_empty_blob() {
    let (stdout, _stderr, code) = run(&["encode", "--names", "foo", "bar", "baz"], None);
    assert_eq!(code, 0);
    assert!(!stdout.trim().is_empty());
}

#[test]
fn encode_names_and_encode_ids_agree() {
    let (by_name, _, code1) = run(&["encode", "--names", "foo", "bar", "baz"], None);
    let id_foo = stylus::subset::id_of("foo").to_string();
    let id_bar = stylus::subset::id_of("bar").to_string();
    let id_baz = stylus::subset::id_of("baz").to_string();
    let (by_id, _, code2) = run(&["encode", "--ids", &id_foo, &id_bar, &id_baz], None);

    assert_eq!(code1, 0);
    assert_eq!(code2, 0);
    assert_eq!(by_name.trim(), by_id.trim());
}

#[test]
fn encode_reads_names_from_stdin_when_neither_flag_given() {
    let (from_args, _, _) = run(&["encode", "--names", "a", "b", "c"], None);
    let (from_stdin, _, code) = run(&["encode"], Some("a\nb\nc\n"));

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
fn names_and_ids_are_mutually_exclusive() {
    let (_stdout, stderr, code) = run(&["encode", "--names", "a", "--ids", "1"], None);
    assert_eq!(code, 2);
    assert!(stderr.contains("cannot be used with"));
}
