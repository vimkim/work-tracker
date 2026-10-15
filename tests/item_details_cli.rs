use std::{
    collections::BTreeSet,
    path::Path,
    process::{Command, Output},
};

use chrono::{DateTime, Local};
use rusqlite::{Connection, types::ValueRef};
use serde_json::{Map, Value, json};
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

fn json_output(database: &Path, args: &[&str]) -> Value {
    serde_json::from_str(&text(database, &[args, &["--json"]].concat())).unwrap()
}

fn add(database: &Path, title: &str, status: &str) -> Value {
    json_output(
        database,
        &[
            "add",
            title,
            "--description",
            "First line of context\nSecond line of context",
            "--status",
            status,
            "--planned",
            "2026-10-10",
            "--due",
            "2026-10-20",
            "--priority",
            "high",
            "--actor",
            "details-test",
        ],
    )
}

fn stored_item(database: &Path, id: i64) -> Value {
    let connection = Connection::open(database).unwrap();
    let mut statement = connection
        .prepare("SELECT * FROM work_items WHERE id = ?1")
        .unwrap();
    let names: Vec<_> = statement
        .column_names()
        .into_iter()
        .map(str::to_owned)
        .collect();
    statement
        .query_row([id], |row| {
            let fields: Map<String, Value> = names
                .iter()
                .enumerate()
                .map(|(index, name)| {
                    let value = match row.get_ref(index).unwrap() {
                        ValueRef::Null => Value::Null,
                        ValueRef::Integer(value) => json!(value),
                        ValueRef::Text(value) => json!(std::str::from_utf8(value).unwrap()),
                        other => panic!("unexpected Work Item value: {other:?}"),
                    };
                    (name.clone(), value)
                })
                .collect();
            Ok(Value::Object(fields))
        })
        .unwrap()
}

#[test]
fn details_match_show_and_keep_list_selection_order_and_footer() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("tracker.db");
    let active = add(&database, "Active work", "active");
    let blocked = add(&database, "Blocked work", "blocked");
    add(&database, "Finished work", "done");

    let shown_blocked = text(&database, &["show", &blocked["id"].to_string()]);
    let shown_active = text(&database, &["show", &active["id"].to_string()]);
    assert_eq!(
        text(&database, &["list", "--details"]),
        format!(
            "{shown_blocked}\n{shown_active}Showing 2 actionable work items. Use --all to include done and cancelled.\n"
        )
    );
    assert_eq!(
        text(&database, &["list", "--details", "--limit", "1"]),
        format!(
            "{shown_blocked}Showing 1 actionable work item. Use --all to include done and cancelled.\n"
        )
    );
    assert_eq!(
        text(&database, &["list", "--details", "--status", "active"]),
        shown_active
    );
    let all = json_output(&database, &["list", "--all"]);
    let expected = all
        .as_array()
        .unwrap()
        .iter()
        .map(|item| text(&database, &["show", &item["id"].to_string()]))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(text(&database, &["list", "--all", "--details"]), expected);

    let compact = text(&database, &["list"]);
    assert!(compact.contains("TITLE") && compact.contains("Active work"));
    assert!(!compact.contains("First line of context") && !compact.contains("Description:"));
    assert_eq!(
        text(&database, &["list", "--details", "--status", "waiting"]),
        "No work items.\n"
    );
    assert_eq!(
        text(&database, &["list", "--details", "--limit", "0"]),
        "No actionable work items. Use --all to include done and cancelled.\n"
    );
}

#[test]
fn json_values_match_every_sqlite_column_and_human_details_include_deletion() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("tracker.db");
    let item = add(&database, "All persisted fields", "active");
    let id = item["id"].as_i64().unwrap();
    let id_text = id.to_string();
    json_output(&database, &["note", &id_text, "History stays separate"]);
    json_output(&database, &["delete", &id_text]);

    let stored = stored_item(&database, id);
    assert_eq!(json_output(&database, &["show", &id_text]), stored);
    let list_args = ["list", "--status", "deleted"];
    assert_eq!(json_output(&database, &list_args), json!([stored]));
    assert_eq!(
        text(&database, &["list", "--status", "deleted", "--json"]),
        text(
            &database,
            &["list", "--status", "deleted", "--details", "--json"]
        )
    );
    let shown = text(&database, &["show", &id_text]);
    assert_eq!(
        text(&database, &["list", "--details", "--status", "deleted"]),
        shown
    );
    for (name, label) in [
        ("id", "ID:"),
        ("status", "Status:"),
        ("title", "Title:"),
        ("workdir", "Work dir:"),
        ("priority", "Priority:"),
        ("planned_date", "Planned:"),
        ("due_date", "Due:"),
        ("description", "Description:"),
        ("created_at", "Created:"),
        ("updated_at", "Updated:"),
        ("deleted_at", "Deleted:"),
        ("purge_after", "Purge after:"),
    ] {
        let value = if name.ends_with("_at") || name == "purge_after" {
            DateTime::parse_from_rfc3339(stored[name].as_str().unwrap())
                .unwrap()
                .with_timezone(&Local)
                .format("%Y-%m-%d %H:%M:%S%.f %:z")
                .to_string()
        } else {
            stored[name]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| stored[name].to_string())
        };
        assert!(
            shown.contains(&format!("{label:<12} {value}\n")),
            "missing {name}: {shown}"
        );
    }
    assert!(!shown.contains("History stays separate"));
    let history = json_output(&database, &["history", &id_text]);
    assert_eq!(history[1]["note"], "History stays separate");
}

#[test]
fn unset_fields_remain_visible_and_json_keeps_nulls() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("tracker.db");
    let item = json_output(&database, &["add", "Minimal"]);
    let id = item["id"].to_string();
    json_output(&database, &["update", &id, "--clear-workdir"]);
    let shown = text(&database, &["show", &id]);
    for label in [
        "Planned:",
        "Due:",
        "Description:",
        "Deleted:",
        "Purge after:",
    ] {
        assert!(shown.contains(&format!("{label:<12} -\n")));
    }
    assert!(shown.contains("Work dir:    unknown\n"));
    let stored = stored_item(&database, item["id"].as_i64().unwrap());
    assert_eq!(json_output(&database, &["show", &id]), stored);
    assert_eq!(json_output(&database, &["list"]), json!([stored]));
}

fn assert_schema_error(database: &Path, args: &[&str], column: &str) {
    let output = run(database, args);
    assert_eq!(output.status.code(), Some(1), "{args:?}: {output:?}");
    assert!(output.stdout.is_empty(), "partial output: {output:?}");
    let error = if args.contains(&"--json") {
        serde_json::from_slice::<Value>(&output.stderr).unwrap()["error"]
            .as_str()
            .unwrap()
            .to_owned()
    } else {
        String::from_utf8(output.stderr).unwrap()
    };
    assert!(
        error.contains("SQLite work_items schema") && error.contains(column),
        "{error}"
    );
}

#[test]
fn runtime_schema_check_rejects_unmapped_columns_including_on_empty_lists() {
    for populated in [false, true] {
        for generated in [false, true] {
            let temp = TempDir::new().unwrap();
            let database = temp.path().join("tracker.db");
            json_output(&database, &["list"]);
            if populated {
                add(&database, "Existing work", "active");
            }
            let connection = Connection::open(&database).unwrap();
            let declaration = if generated {
                "TEXT GENERATED ALWAYS AS (title || ' extra') VIRTUAL"
            } else {
                "TEXT"
            };
            connection
                .execute_batch(&format!(
                    "ALTER TABLE work_items ADD COLUMN future_context {declaration}"
                ))
                .unwrap();
            drop(connection);
            for args in [
                vec!["show", "1"],
                vec!["show", "1", "--json"],
                vec!["list", "--json"],
                vec!["list", "--details"],
                vec!["list", "--details", "--json"],
            ] {
                assert_schema_error(&database, &args, "future_context");
            }
        }
    }
}

#[test]
fn runtime_schema_check_rejects_a_missing_persisted_field() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("tracker.db");
    add(&database, "Existing work", "active");
    let connection = Connection::open(&database).unwrap();
    connection
        .execute_batch("ALTER TABLE work_items DROP COLUMN description")
        .unwrap();
    drop(connection);
    for args in [
        vec!["show", "1"],
        vec!["list", "--json"],
        vec!["list", "--details"],
    ] {
        assert_schema_error(&database, &args, "description");
    }
}

#[test]
fn runtime_check_uses_the_current_schema_on_an_open_connection() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("tracker.db");
    let tracker = work_tracker::db::Tracker::open(&database).unwrap();
    tracker
        .check_work_item_output_fields(work_tracker::output::item_field_names())
        .unwrap();
    let connection = Connection::open(&database).unwrap();
    connection
        .execute_batch("ALTER TABLE work_items ADD COLUMN extra TEXT")
        .unwrap();
    assert!(
        tracker
            .check_work_item_output_fields(work_tracker::output::item_field_names())
            .is_err()
    );

    // No hard-coded test column set: SQLite defines which fields must be covered.
    let expected: BTreeSet<String> = connection
        .prepare("PRAGMA table_xinfo(work_items)")
        .unwrap()
        .query_map([], |row| row.get(1))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    tracker
        .check_work_item_output_fields(expected.iter().map(String::as_str))
        .unwrap();
}
