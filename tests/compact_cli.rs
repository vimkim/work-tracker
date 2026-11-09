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

fn text(database: &Path, args: &[&str]) -> String {
    let output = run(database, args);
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

fn json(database: &Path, args: &[&str]) -> Value {
    serde_json::from_str(&text(database, &[args, &["--json"]].concat())).unwrap()
}

fn keys(value: &Value) -> Vec<&str> {
    let mut keys: Vec<_> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    keys
}

const COMPACT_KEYS: [&str; 8] = [
    "due_date",
    "id",
    "planned_date",
    "priority",
    "status",
    "title",
    "updated_at",
    "workdir",
];

fn seeded() -> (TempDir, std::path::PathBuf) {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("tracker.db");
    json(
        &database,
        &[
            "add",
            "Watch CI",
            "--description",
            "long context that agents do not need in an index",
            "--status",
            "waiting",
            "--due",
            "2026-12-01",
            "--actor",
            "compact-test",
        ],
    );
    (directory, database)
}

#[test]
fn list_and_today_compact_json_keep_only_index_fields() {
    let (_directory, database) = seeded();
    for command in [&["list", "--compact"][..], &["today", "--compact"][..]] {
        let items = json(&database, command);
        let item = &items.as_array().unwrap()[0];
        assert_eq!(keys(item), COMPACT_KEYS, "{command:?}");
        assert_eq!(item["title"], "Watch CI");
        assert_eq!(item["status"], "waiting");
        assert_eq!(item["due_date"], "2026-12-01");
    }
}

#[test]
fn json_without_compact_keeps_full_items() {
    let (_directory, database) = seeded();
    let items = json(&database, &["today"]);
    assert_eq!(
        items[0]["description"],
        "long context that agents do not need in an index"
    );
}

#[test]
fn compact_conflicts_with_details() {
    let (_directory, database) = seeded();
    let output = run(&database, &["list", "--compact", "--details"]);
    assert!(!output.status.success());
}

#[test]
fn history_limit_keeps_newest_entries_in_order() {
    let (_directory, database) = seeded();
    for note in ["first", "second", "third"] {
        text(&database, &["note", "1", note, "--actor", "compact-test"]);
    }

    let entries = json(&database, &["history", "1", "--limit", "2"]);
    let notes: Vec<_> = entries
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["note"].as_str().unwrap())
        .collect();
    assert_eq!(notes, ["second", "third"]);

    let human = text(&database, &["history", "1", "--limit", "2"]);
    assert!(
        human.starts_with("2 earlier entries omitted. Remove --limit to show all.\n"),
        "{human}"
    );
    assert!(!text(&database, &["history", "1"]).contains("omitted"));
    assert_eq!(
        json(&database, &["history", "1"]).as_array().unwrap().len(),
        4
    );
}

#[test]
fn history_limit_zero_omits_everything_with_a_marker() {
    let (_directory, database) = seeded();
    assert_eq!(
        json(&database, &["history", "1", "--limit", "0"]),
        Value::Array(vec![])
    );
    let human = text(&database, &["history", "1", "--limit", "0"]);
    assert_eq!(
        human,
        "1 earlier entry omitted. Remove --limit to show all.\n"
    );
}
