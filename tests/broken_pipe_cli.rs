use std::{
    io::pipe,
    path::Path,
    process::{Command, Output},
};

use serde_json::Value;
use tempfile::TempDir;

fn command(database: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_work-tracker"));
    command
        .arg("--database")
        .arg(database)
        .args(args)
        .env("NO_COLOR", "1");
    command
}

fn run(database: &Path, args: &[&str]) -> Output {
    command(database, args).output().unwrap()
}

fn run_with_closed_stdout(database: &Path, args: &[&str]) -> Output {
    let (reader, writer) = pipe().unwrap();
    drop(reader);
    command(database, args).stdout(writer).output().unwrap()
}

fn seeded_database(directory: &TempDir) -> std::path::PathBuf {
    let database = directory.path().join("work-tracker.db");
    for title in ["first", "second", "third"] {
        let output = run(&database, &["add", title]);
        assert!(output.status.success(), "{output:?}");
    }
    database
}

fn assert_silent_success(output: &Output) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(stderr, "");
}

#[test]
fn closed_stdout_is_a_silent_success_for_human_output() {
    let directory = TempDir::new().unwrap();
    let database = seeded_database(&directory);

    assert_silent_success(&run_with_closed_stdout(&database, &["list", "--all"]));
}

#[test]
fn closed_stdout_is_a_silent_success_for_json_output() {
    let directory = TempDir::new().unwrap();
    let database = seeded_database(&directory);

    assert_silent_success(&run_with_closed_stdout(
        &database,
        &["list", "--all", "--json"],
    ));
}

#[test]
fn closed_stdout_after_a_mutation_does_not_report_failure() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("work-tracker.db");

    assert_silent_success(&run_with_closed_stdout(
        &database,
        &["add", "created despite closed stdout", "--json"],
    ));

    let stored = run(&database, &["show", "1", "--json"]);
    assert!(stored.status.success(), "{stored:?}");
    let item: Value = serde_json::from_slice(&stored.stdout).unwrap();
    assert_eq!(item["title"], "created despite closed stdout");
}

#[test]
fn closed_stdout_does_not_hide_real_errors() {
    let directory = TempDir::new().unwrap();
    let database = seeded_database(&directory);

    let output = run_with_closed_stdout(&database, &["show", "999", "--json"]);

    assert!(!output.status.success(), "{output:?}");
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert!(error["error"].is_string(), "{error}");
}
