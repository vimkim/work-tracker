use anyhow::Result;
use rusqlite::Connection;
use serde_json::json;
use work_tracker::{
    db::{NewWorkItem, Tracker, WorkItemUpdate},
    domain::{Schedule, Status},
};

#[test]
fn directory_corrections_and_clears_are_atomic_idempotent_and_preserve_other_fields() -> Result<()>
{
    let mut tracker = Tracker::open(std::path::Path::new(":memory:"))?;
    let original = tracker.create_item(
        NewWorkItem {
            title: "Original",
            description: Some("Context"),
            status: Status::Active,
            schedule: Schedule::default(),
            workdir: Some("/work/first"),
        },
        "creator",
        Some("Creation"),
    )?;
    assert_eq!(
        tracker.history(original.id)?[0].changes["workdir"],
        "/work/first"
    );
    let moved = tracker.update_item(
        original.id,
        WorkItemUpdate {
            title: Some("Moved"),
            workdir: Some(Some("/work/second")),
            ..Default::default()
        },
        "agent",
        Some("New worktree"),
    )?;
    assert_eq!(moved.workdir.as_deref(), Some("/work/second"));
    assert_eq!(moved.description, original.description);
    assert_eq!(moved.status, original.status);
    let history = tracker.history(original.id)?;
    assert_eq!(history.len(), 2);
    assert_eq!(history[1].actor, "agent");
    assert_eq!(history[1].note.as_deref(), Some("New worktree"));
    assert_eq!(
        history[1].changes["title"],
        json!({"from": "Original", "to": "Moved"})
    );
    assert_eq!(
        history[1].changes["workdir"],
        json!({"from": "/work/first", "to": "/work/second"})
    );
    let unchanged = tracker.update_item(
        original.id,
        WorkItemUpdate {
            workdir: Some(Some("/work/second")),
            ..Default::default()
        },
        "agent",
        None,
    )?;
    assert_eq!(unchanged.updated_at, moved.updated_at);
    assert_eq!(tracker.history(original.id)?.len(), 2);
    for invalid in ["", "relative"] {
        assert!(
            tracker
                .update_item(
                    original.id,
                    WorkItemUpdate {
                        title: Some("Must not change"),
                        workdir: Some(Some(invalid)),
                        ..Default::default()
                    },
                    "agent",
                    None
                )
                .is_err()
        );
        assert_eq!(tracker.get(original.id)?.title, "Moved");
        assert_eq!(tracker.get(original.id)?.workdir, moved.workdir);
        assert_eq!(tracker.history(original.id)?.len(), 2);
    }
    // A legacy field-specific API must preserve the association.
    tracker.update(
        original.id,
        None,
        Some(Some("Edited context")),
        "agent",
        None,
    )?;
    assert_eq!(tracker.get(original.id)?.workdir, moved.workdir);
    tracker.update_item(
        original.id,
        WorkItemUpdate {
            workdir: Some(None),
            ..Default::default()
        },
        "agent",
        None,
    )?;
    assert!(tracker.get(original.id)?.workdir.is_none());
    assert_eq!(
        tracker.history(original.id)?[3].changes["workdir"],
        json!({"from": "/work/second", "to": null})
    );
    tracker.update_item(
        original.id,
        WorkItemUpdate {
            workdir: Some(None),
            ..Default::default()
        },
        "agent",
        None,
    )?;
    assert_eq!(tracker.history(original.id)?.len(), 4);
    tracker.set_status(original.id, Status::Deleted, "agent", None)?;
    assert!(
        tracker
            .update_item(
                original.id,
                WorkItemUpdate {
                    workdir: Some(Some("/work/third")),
                    ..Default::default()
                },
                "agent",
                None
            )
            .is_err()
    );
    assert!(tracker.get(original.id)?.workdir.is_none());
    assert_eq!(tracker.history(original.id)?.len(), 5);
    Ok(())
}

fn legacy_ledger(path: &std::path::Path, version: i64) -> Result<()> {
    let connection = Connection::open(path)?;
    connection.execute_batch(include_str!("fixtures/schema_v1.sql"))?;
    connection.execute("INSERT INTO work_items(id,title,status,created_at,updated_at) VALUES(42,'Legacy','pending',?1,?1)", ["2026-10-01T00:00:00.000Z"])?;
    connection.execute("INSERT INTO history_entries(work_item_id,kind,actor,occurred_at,changes_json) VALUES(42,'created','legacy',?1,'{\"title\":\"Legacy\"}')", ["2026-10-01T00:00:00.000Z"])?;
    if version == 2 {
        connection.execute_batch(
            "ALTER TABLE work_items ADD COLUMN planned_date TEXT;
            ALTER TABLE work_items ADD COLUMN due_date TEXT;
            ALTER TABLE work_items ADD COLUMN priority TEXT NOT NULL DEFAULT 'normal';
            UPDATE work_items SET planned_date='2026-10-01', due_date='2026-10-08', priority='high';
            PRAGMA user_version=2;",
        )?;
    }
    Ok(())
}

#[test]
fn migration_preserves_both_earlier_schemas_and_legacy_field_writes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    for version in [1, 2] {
        let path = temp.path().join(format!("v{version}.db"));
        legacy_ledger(&path, version)?;
        let mut tracker = Tracker::open(&path)?;
        let original = tracker.get(42)?;
        assert!(original.workdir.is_none());
        assert_eq!(original.title, "Legacy");
        assert_eq!(original.status, Status::Pending);
        assert_eq!(
            original.created_at.to_rfc3339(),
            "2026-10-01T00:00:00+00:00"
        );
        if version == 2 {
            assert_eq!(
                original.schedule.priority,
                work_tracker::domain::Priority::High
            );
            assert_eq!(
                original.schedule.planned_date.unwrap().to_string(),
                "2026-10-01"
            );
            assert_eq!(
                original.schedule.due_date.unwrap().to_string(),
                "2026-10-08"
            );
        }
        let history = serde_json::to_value(tracker.history(42)?)?;
        assert_eq!(history.as_array().unwrap().len(), 1);
        assert_eq!(history[0]["changes"], json!({"title": "Legacy"}));
        tracker.update_item(
            42,
            WorkItemUpdate {
                workdir: Some(Some("/work/project")),
                ..Default::default()
            },
            "agent",
            None,
        )?;
        drop(tracker);
        let legacy = Connection::open(&path)?;
        legacy.execute_batch(include_str!("fixtures/schema_v1.sql"))?;
        legacy.execute(
            "UPDATE work_items SET title='Legacy edited' WHERE id=42",
            [],
        )?;
        for _ in 0..2 {
            let tracker = Tracker::open(&path)?;
            let item = tracker.get(42)?;
            assert_eq!(item.title, "Legacy edited");
            assert_eq!(item.workdir.as_deref(), Some("/work/project"));
            assert_eq!(item.schedule, original.schedule);
            let after = serde_json::to_value(tracker.history(42)?)?;
            assert_eq!(after[0], history[0]);
            assert_eq!(after.as_array().unwrap().len(), 2);
        }
        legacy.pragma_update(None, "user_version", 4)?;
        let Err(error) = Tracker::open(&path) else {
            panic!("future schema version was accepted");
        };
        assert!(error.to_string().contains("newer than supported version 3"));
    }
    Ok(())
}

#[test]
fn concurrent_migration_preserves_unknown_associations_and_creation_history() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("concurrent.db");
    legacy_ledger(&path, 2)?;
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || -> Result<()> {
                barrier.wait();
                let tracker = Tracker::open(&path)?;
                assert!(tracker.get(42)?.workdir.is_none());
                assert_eq!(tracker.history(42)?.len(), 1);
                Ok(())
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap()?;
    }
    Ok(())
}
