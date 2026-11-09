use std::{
    path::Path,
    process::{Command, Output},
};

use chrono::{Duration, Utc};
use serde_json::Value;
use tempfile::TempDir;
use work_tracker::doctor::{self, CheckStatus};

fn run(database: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_work-tracker"))
        .arg("--database")
        .arg(database)
        .args(args)
        .env("NO_COLOR", "1")
        .env("WORK_TRACKER_ACTOR", "doctor-test")
        .env_remove("WORK_TRACKER_BACKUP_DIR")
        .output()
        .unwrap()
}

fn report(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap()
}

fn status_of<'a>(report: &'a Value, name: &str) -> &'a str {
    report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["name"] == name)
        .unwrap_or_else(|| panic!("no {name} check in {report}"))["status"]
        .as_str()
        .unwrap()
}

fn healthy() -> (TempDir, std::path::PathBuf) {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("tracker.db");
    assert!(run(&database, &["add", "Seed"]).status.success());
    assert!(run(&database, &["backup"]).status.success());
    (directory, database)
}

#[test]
fn healthy_ledger_passes_every_check() {
    let (_directory, database) = healthy();
    let output = run(&database, &["doctor", "--json"]);
    assert!(output.status.success(), "{output:?}");
    let report = report(&output);
    assert_eq!(report["ok"], true);
    for name in [
        "database",
        "schema",
        "integrity",
        "journal",
        "backup",
        "actor",
    ] {
        assert_eq!(status_of(&report, name), "ok", "{report}");
    }

    let text = String::from_utf8(run(&database, &["doctor"]).stdout).unwrap();
    assert!(text.starts_with("ok    database   "), "{text}");
    assert!(
        text.contains("ok    actor      WORK_TRACKER_ACTOR=doctor-test\n"),
        "{text}"
    );
}

#[test]
fn missing_database_fails_without_creating_it() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("missing.db");
    let output = run(&database, &["doctor", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let report = report(&output);
    assert_eq!(report["ok"], false);
    assert_eq!(status_of(&report, "database"), "fail");
    assert_eq!(status_of(&report, "backup"), "warn");
    assert!(!database.exists());
}

#[test]
fn doctor_does_not_write_to_the_ledger() {
    let (_directory, database) = healthy();
    let before = std::fs::read(&database).unwrap();
    assert!(run(&database, &["doctor"]).status.success());
    assert_eq!(std::fs::read(&database).unwrap(), before);
}

#[test]
fn stale_or_missing_snapshots_warn_without_failing() {
    let (directory, database) = healthy();
    let backups = directory.path().join("backups");

    let fresh = doctor::run(&database, &backups, Some("a"), Utc::now());
    let backup = fresh.iter().find(|check| check.name == "backup").unwrap();
    assert_eq!(backup.status, CheckStatus::Ok);

    let later = doctor::run(
        &database,
        &backups,
        Some("a"),
        Utc::now() + Duration::hours(49),
    );
    let backup = later.iter().find(|check| check.name == "backup").unwrap();
    assert_eq!(backup.status, CheckStatus::Warn);
    assert!(
        backup.detail.contains("hours old (limit 48)"),
        "{}",
        backup.detail
    );
    assert!(!later.iter().any(|check| check.failed_required()));

    let empty = directory.path().join("empty");
    let none = doctor::run(&database, &empty, None, Utc::now());
    for name in ["backup", "actor"] {
        let check = none.iter().find(|check| check.name == name).unwrap();
        assert_eq!(check.status, CheckStatus::Warn, "{name}");
    }
}
