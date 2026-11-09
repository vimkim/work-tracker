use std::{
    path::Path,
    process::{Command, Output},
};

use serde_json::Value;
use tempfile::TempDir;

fn run(database: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_work-tracker"))
        .arg("--database")
        .arg(database)
        .args(args)
        .env("NO_COLOR", "1")
        .output()
        .unwrap()
}

fn ok(database: &Path, args: &[&str]) -> String {
    let output = run(database, args);
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

fn search(database: &Path, terms: &[&str]) -> Vec<Value> {
    let output = ok(database, &[&["search"], terms, &["--json"]].concat());
    serde_json::from_str::<Value>(&output)
        .unwrap()
        .as_array()
        .unwrap()
        .clone()
}

fn ids(hits: &[Value]) -> Vec<i64> {
    hits.iter().map(|hit| hit["id"].as_i64().unwrap()).collect()
}

fn seeded() -> (TempDir, std::path::PathBuf) {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("tracker.db");
    let actor = ["--actor", "search"];
    ok(
        &database,
        &[&["add", "Watch company CI"][..], &actor].concat(),
    );
    ok(
        &database,
        &[
            &[
                "add",
                "OOS 트래커 정리",
                "--description",
                "100% done_flag 경로",
            ][..],
            &actor,
        ]
        .concat(),
    );
    ok(
        &database,
        &[&["add", "Finished release", "--status", "done"][..], &actor].concat(),
    );
    ok(
        &database,
        &[&["note", "1", "Runner timeout on medium suite"][..], &actor].concat(),
    );
    ok(
        &database,
        &[&["note", "3", "배포 대기 중이었음"][..], &actor].concat(),
    );
    (directory, database)
}

#[test]
fn matches_titles_descriptions_and_notes_case_insensitively() {
    let (_directory, database) = seeded();
    let hits = search(&database, &["ci"]);
    assert_eq!(ids(&hits), [1]);
    assert_eq!(hits[0]["matched_in"], "title");

    let hits = search(&database, &["TIMEOUT"]);
    assert_eq!(ids(&hits), [1]);
    assert_eq!(hits[0]["matched_in"], "note");
    assert_eq!(hits[0]["snippet"], "Runner timeout on medium suite");
    assert!(hits[0].get("description").is_none());
}

#[test]
fn matches_korean_substrings_and_finished_work() {
    let (_directory, database) = seeded();
    assert_eq!(ids(&search(&database, &["트래커"])), [2]);
    let hits = search(&database, &["대기"]);
    assert_eq!(ids(&hits), [3]);
    assert_eq!(hits[0]["status"], "done");
}

#[test]
fn every_term_must_match_somewhere_on_the_item() {
    let (_directory, database) = seeded();
    assert_eq!(ids(&search(&database, &["company", "medium"])), [1]);
    assert_eq!(ids(&search(&database, &["company medium"])), [1]);
    assert!(search(&database, &["company", "배포"]).is_empty());
}

#[test]
fn like_wildcards_are_literal() {
    let (_directory, database) = seeded();
    assert_eq!(ids(&search(&database, &["100%"])), [2]);
    assert_eq!(ids(&search(&database, &["done_flag"])), [2]);
    assert!(search(&database, &["%"]).len() == 1);
    assert!(search(&database, &["_"]).len() == 1);
}

#[test]
fn deleted_items_are_hidden_unless_requested() {
    let (_directory, database) = seeded();
    ok(&database, &["delete", "1", "--actor", "search"]);
    assert!(search(&database, &["timeout"]).is_empty());
    let output = ok(
        &database,
        &["search", "timeout", "--include-deleted", "--json"],
    );
    assert!(output.contains("\"id\": 1"));
}

#[test]
fn human_output_shows_where_it_matched() {
    let (_directory, database) = seeded();
    assert_eq!(
        ok(&database, &["search", "timeout"]),
        "1       pending    Watch company CI\n        note: Runner timeout on medium suite\n"
    );
    assert_eq!(
        ok(&database, &["search", "nothing-here"]),
        "No matching work items.\n"
    );
}

#[test]
fn an_empty_query_is_rejected() {
    let (_directory, database) = seeded();
    assert!(!run(&database, &["search"]).status.success());
    assert!(!run(&database, &["search", "  "]).status.success());
}
