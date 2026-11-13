//! The strict JSON-lines reader the Codex tests use for the fake's answer log:
//! it never skips a line, so "nothing was answered" assertions stay sound.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::Write;
use std::time::Duration;

use common::{parse_jsonl, read_jsonl};
use serde_json::json;

#[test]
fn complete_lines_parse_and_empty_text_is_no_evidence() {
    assert_eq!(parse_jsonl("").unwrap(), Some(vec![]));
    assert_eq!(parse_jsonl("{\"a\":1}\n{\"b\":2}\n").unwrap(), Some(vec![json!({"a":1}), json!({"b":2})]));
}

#[test]
fn an_unterminated_last_line_is_a_write_in_progress() {
    assert_eq!(parse_jsonl("{\"a\":1}\n{\"b\":").unwrap(), None);
    // Even if the partial text happens to be valid JSON, it is not complete yet.
    assert_eq!(parse_jsonl("{\"a\":1}").unwrap(), None);
}

#[test]
fn a_complete_malformed_line_is_an_error_not_skipped() {
    let err = parse_jsonl("{\"a\":1}\nnot json\n{\"b\":2}\n").unwrap_err();
    assert!(err.starts_with("line 2:"), "{err}");
}

#[test]
fn a_missing_file_is_no_evidence() {
    let dir = tempfile::tempdir().unwrap();
    assert!(read_jsonl(&dir.path().join("absent.jsonl")).is_empty());
}

#[test]
fn the_reader_waits_for_a_line_being_written() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("answers.jsonl");
    std::fs::write(&path, "{\"answer\":1}\n{\"answer\":").unwrap();
    let writer = {
        let path = path.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
            writeln!(f, "2}}").unwrap();
        })
    };
    assert_eq!(read_jsonl(&path), vec![json!({"answer":1}), json!({"answer":2})]);
    writer.join().unwrap();
}

#[test]
#[should_panic(expected = "malformed evidence")]
fn the_reader_fails_on_a_malformed_complete_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("answers.jsonl");
    std::fs::write(&path, "{\"answer\":1}\ngarbage\n").unwrap();
    let _ = read_jsonl(&path);
}

#[test]
#[should_panic(expected = "never completed")]
fn the_reader_fails_on_a_line_that_never_completes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("answers.jsonl");
    std::fs::write(&path, "{\"answer\":").unwrap();
    let _ = read_jsonl(&path);
}
