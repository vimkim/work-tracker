use std::{fs, path::Path, process::Command};

use rusqlite::Connection;
use serde_json::Value;
use tempfile::TempDir;
use work_tracker::{
    db::{Tracker, backup},
    domain::Status,
};

fn json(database: &Path, args: &[&str]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_work-tracker"))
        .arg("--database")
        .arg(database)
        .args(args)
        .arg("--json")
        .env_remove("WORK_TRACKER_BACKUP_DIR")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn snapshot_recovers_all_statuses_fields_and_history_from_committed_wal() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("ledger.db");
    let mut tracker = Tracker::open(&database).unwrap();
    let item = tracker
        .create(
            "Keep my archive",
            Some("context"),
            Status::Active,
            "tester",
            None,
        )
        .unwrap();
    tracker
        .add_note(item.id, "recovery context", "tester")
        .unwrap();
    let deleted = tracker
        .create(
            "Keep deleted history",
            None,
            Status::Pending,
            "tester",
            None,
        )
        .unwrap();
    tracker
        .set_status(deleted.id, Status::Deleted, "tester", None)
        .unwrap();
    // An expired item must be captured before normal open-time housekeeping.
    let writer = Connection::open(&database).unwrap();
    writer
        .execute(
            "UPDATE work_items SET purge_after = '2000-01-01T00:00:00Z' WHERE id = ?1",
            [deleted.id],
        )
        .unwrap();
    writer
        .execute_batch("BEGIN IMMEDIATE; UPDATE work_items SET title = 'uncommitted'")
        .unwrap();
    assert!(database.with_extension("db-wal").exists());

    let snapshot = backup::create(&database, &temp.path().join("backups")).unwrap();
    assert_eq!(snapshot.contents.work_items, 2);
    assert_eq!(snapshot.contents.history_entries, 4);
    writer.execute_batch("ROLLBACK").unwrap();
    tracker
        .set_status(item.id, Status::Done, "tester", Some("after snapshot"))
        .unwrap();
    let restored = temp.path().join("recovery/ledger.db");
    backup::restore(&snapshot.path, &restored).unwrap();
    let recovered = Connection::open(&restored).unwrap();
    assert_eq!(
        recovered
            .query_row(
                "SELECT title FROM work_items WHERE id = ?1",
                [item.id],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "Keep my archive"
    );
    assert_eq!(
        recovered
            .query_row(
                "SELECT status FROM work_items WHERE id = ?1",
                [item.id],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "active"
    );
    assert_eq!(
        recovered
            .query_row("SELECT count(*) FROM work_items", [], |r| r
                .get::<_, u64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        recovered
            .query_row("SELECT count(*) FROM history_entries", [], |r| r
                .get::<_, u64>(0))
            .unwrap(),
        4
    );
    assert_eq!(tracker.get(item.id).unwrap().status, Status::Done);
    assert!(tracker.get(deleted.id).is_ok());
    for entry in fs::read_dir(snapshot.path.parent().unwrap()).unwrap() {
        assert_eq!(entry.unwrap().path().extension().unwrap(), "db");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&snapshot.path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(snapshot.path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
}

#[test]
fn restore_never_overwrites_existing_files_aliases_or_sidecars() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("ledger.db");
    Tracker::open(&database).unwrap();
    let snapshot = backup::create(&database, &temp.path().join("backups")).unwrap();
    let before = fs::read(&snapshot.path).unwrap();
    assert!(backup::restore(&snapshot.path, &snapshot.path).is_err());
    assert_eq!(fs::read(&snapshot.path).unwrap(), before);
    let target = temp.path().join("existing.db");
    fs::write(&target, b"precious file").unwrap();
    assert!(backup::restore(&snapshot.path, &target).is_err());
    assert_eq!(fs::read(&target).unwrap(), b"precious file");
    let missing = temp.path().join("missing.db");
    fs::write(temp.path().join("missing.db-wal"), b"old WAL").unwrap();
    assert!(backup::restore(&snapshot.path, &missing).is_err());
    assert!(!missing.exists());
    #[cfg(unix)]
    {
        let alias = temp.path().join("alias.db");
        std::os::unix::fs::symlink(&missing, &alias).unwrap();
        assert!(backup::restore(&snapshot.path, &alias).is_err());
        assert!(!missing.exists());
    }
}

#[test]
fn missing_invalid_and_future_databases_fail_without_publishing_snapshots() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source.db");
    let directory = temp.path().join("backups");
    assert!(backup::create(&source, &directory).is_err());
    assert!(!source.exists());
    assert!(!directory.exists());
    fs::write(&source, b"not SQLite").unwrap();
    let target = temp.path().join("restored.db");
    assert!(backup::create(&source, &directory).is_err());
    assert!(backup::restore(&source, &target).is_err());
    assert!(!target.exists());
    assert!(backup::list(&directory).unwrap().is_empty());
    fs::remove_file(&source).unwrap();
    let connection = Connection::open(&source).unwrap();
    assert!(backup::create(&source, &directory).is_err());
    connection.pragma_update(None, "user_version", 99).unwrap();
    assert!(backup::create(&source, &directory).is_err());
    assert!(backup::restore(&source, &target).is_err());
    assert!(backup::list(&directory).unwrap().is_empty());
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
}

#[test]
fn concurrent_backups_keep_every_snapshot_and_list_newest_first() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("ledger.db");
    Tracker::open(&source).unwrap();
    let directory = temp.path().join("backups");
    let workers = (0..4)
        .map(|_| {
            let source = source.clone();
            let directory = directory.clone();
            std::thread::spawn(move || backup::create(&source, &directory).unwrap())
        })
        .collect::<Vec<_>>();
    for worker in workers {
        assert!(worker.join().unwrap().path.exists());
    }
    fs::write(
        directory.join("work-tracker-interrupted.partial"),
        b"incomplete",
    )
    .unwrap();
    fs::write(directory.join("other.db"), b"unrelated").unwrap();
    let snapshots = backup::list(&directory).unwrap();
    assert_eq!(snapshots.len(), 4);
    assert!(
        snapshots
            .windows(2)
            .all(|pair| pair[0].created_at >= pair[1].created_at)
    );
    fs::write(&snapshots[0].path, b"corrupt snapshot").unwrap();
    assert!(backup::list(&directory).is_err());
}

#[test]
fn cli_supports_human_json_custom_directory_and_snapshot_recovery() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("ledger.db");
    let item = json(
        &database,
        &[
            "add",
            "Initial status",
            "--planned",
            "2026-10-29",
            "--priority",
            "high",
        ],
    );
    let id = item["id"].to_string();
    let snapshot = json(&database, &["backup"]);
    assert_eq!(snapshot["work_items"], 1);
    assert_eq!(snapshot["history_entries"], 1);
    let snapshots = json(&database, &["backups"]);
    assert_eq!(snapshots[0], snapshot);
    json(&database, &["status", &id, "done"]);
    let recovered = temp.path().join("restored.db");
    let recovery = json(
        &database,
        &[
            "restore",
            snapshot["path"].as_str().unwrap(),
            "--to",
            recovered.to_str().unwrap(),
        ],
    );
    assert_eq!(recovery["work_items"], 1);
    assert_eq!(json(&recovered, &["show", &id]), item);
    assert_eq!(
        json(&recovered, &["history", &id])
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(json(&database, &["show", &id])["status"], "done");

    let custom = temp.path().join("custom directory");
    let custom_snapshot = json(
        &database,
        &["backup", "--directory", custom.to_str().unwrap()],
    );
    let listed = Command::new(env!("CARGO_BIN_EXE_work-tracker"))
        .args(["backups", "--json"])
        .arg("--database")
        .arg(&database)
        .env("WORK_TRACKER_BACKUP_DIR", &custom)
        .output()
        .unwrap();
    assert!(listed.status.success(), "{listed:?}");
    assert_eq!(
        serde_json::from_slice::<Value>(&listed.stdout).unwrap()[0],
        custom_snapshot
    );
    for args in [
        vec!["backup"],
        vec!["backups"],
        vec![
            "restore",
            snapshot["path"].as_str().unwrap(),
            "--to",
            "human-recovery.db",
        ],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_work-tracker"))
            .arg("--database")
            .arg(&database)
            .args(args)
            .current_dir(temp.path())
            .env_remove("WORK_TRACKER_BACKUP_DIR")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(
            text.contains("work-tracker-") || text.contains("Snapshot:"),
            "{text}"
        );
    }
    let error = Command::new(env!("CARGO_BIN_EXE_work-tracker"))
        .args(["restore", snapshot["path"].as_str().unwrap(), "--to"])
        .arg(&recovered)
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(error.status.code(), Some(1));
    assert!(
        serde_json::from_slice::<Value>(&error.stderr).unwrap()["error"]
            .as_str()
            .unwrap()
            .contains("already exists")
    );
}

#[test]
fn cli_backup_commands_do_not_initialize_a_missing_selected_ledger() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("missing-parent/ledger.db");
    let output = Command::new(env!("CARGO_BIN_EXE_work-tracker"))
        .arg("--database")
        .arg(&database)
        .args(["backup", "--json"])
        .env_remove("WORK_TRACKER_BACKUP_DIR")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(serde_json::from_slice::<Value>(&output.stderr).unwrap()["error"].is_string());
    assert_eq!(json(&database, &["backups"]), serde_json::json!([]));
    assert!(!database.parent().unwrap().exists());
}
