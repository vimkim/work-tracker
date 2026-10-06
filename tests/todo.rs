use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use tempfile::TempDir;
use work_tracker::{
    db::{ListFilter, Tracker},
    domain::{Priority, Schedule, ScheduleUpdate, Status, parse_date},
    output::format_todo,
    todo::TodoWindow,
};

fn at(value: &str, days: u32) -> TodoWindow {
    TodoWindow::new(value.parse::<DateTime<Utc>>().unwrap(), days).unwrap()
}

fn schedule(planned: Option<&str>, due: Option<&str>, priority: Priority) -> Schedule {
    Schedule {
        planned_date: planned.map(|s| parse_date(s).unwrap()),
        due_date: due.map(|s| parse_date(s).unwrap()),
        priority,
    }
}

#[test]
fn all_includes_finished_dates_in_window_without_carrying_finished_work() -> Result<()> {
    let mut db = Tracker::open(std::path::Path::new(":memory:"))?;
    let mut expected = Vec::new();
    for (title, status, planned, due, first_days) in [
        ("Done today", Status::Done, Some("2026-10-06"), None, 1),
        (
            "Cancelled today",
            Status::Cancelled,
            None,
            Some("2026-10-06"),
            1,
        ),
        (
            "Both dates",
            Status::Done,
            Some("2026-10-06"),
            Some("2026-10-06"),
            1,
        ),
        (
            "Old due, current plan",
            Status::Done,
            Some("2026-10-06"),
            Some("2026-10-01"),
            1,
        ),
        ("Three-day edge", Status::Done, None, Some("2026-10-08"), 3),
        ("Five-day edge", Status::Done, Some("2026-10-10"), None, 5),
        (
            "Old done",
            Status::Done,
            Some("2026-10-05"),
            Some("2026-10-05"),
            0,
        ),
        ("Future done", Status::Done, Some("2026-10-11"), None, 0),
        ("Undated done", Status::Done, None, None, 0),
    ] {
        let item = db.create_scheduled(
            title,
            None,
            status,
            "test",
            None,
            schedule(planned, due, Priority::Normal),
        )?;
        if first_days != 0 {
            expected.push((item.id, first_days));
        }
    }
    let deleted = db.create_scheduled(
        "Deleted",
        None,
        Status::Pending,
        "test",
        None,
        schedule(Some("2026-10-06"), None, Priority::Normal),
    )?;
    db.set_status(deleted.id, Status::Deleted, "test", None)?;
    let carried = db.create_scheduled(
        "Unfinished old plan",
        None,
        Status::Pending,
        "test",
        None,
        schedule(Some("2026-10-01"), None, Priority::Normal),
    )?;
    for days in [1, 3, 5] {
        let view = db.todo_view(at("2026-10-06T00:00:00Z", days), true)?;
        let mut actual: Vec<_> = view.finished.iter().map(|r| r.item.id).collect();
        actual.sort();
        assert_eq!(
            actual,
            expected
                .iter()
                .filter(|(_, first)| *first <= days)
                .map(|(id, _)| *id)
                .collect::<Vec<_>>()
        );
        assert!(view.finished.iter().all(|r| !r.overdue && !r.carried_over));
        assert_eq!(view.actions.len(), 1);
        assert_eq!(view.actions[0].item.id, carried.id);
        assert!(view.actions[0].carried_over);
        assert!(
            db.todo_view(at("2026-10-06T00:00:00Z", days), false)?
                .finished
                .is_empty()
        );
    }
    Ok(())
}

#[test]
fn calendar_windows_count_weekends_and_cross_seoul_midnight() {
    let before = at("2026-10-06T14:59:59Z", 1);
    let after = at("2026-10-06T15:00:00Z", 1);
    assert_eq!(before.start.to_string(), "2026-10-06");
    assert_eq!(after.start.to_string(), "2026-10-07");
    for (now, days, expected) in [
        ("2026-10-09T00:00:00Z", 3, "2026-10-11"),
        ("2026-10-10T00:00:00Z", 5, "2026-10-14"),
        ("2026-12-31T00:00:00Z", 3, "2027-01-02"),
        ("2028-02-28T00:00:00Z", 3, "2028-03-01"),
        ("2027-02-28T00:00:00Z", 3, "2027-03-02"),
    ] {
        assert_eq!(at(now, days).end.to_string(), expected);
    }
    assert!(TodoWindow::new(Utc::now(), 0).is_err());
    assert!(TodoWindow::new(Utc::now(), u32::MAX).is_err());
}

#[test]
fn selection_priority_sections_and_carryover_are_independent() -> Result<()> {
    let mut db = Tracker::open(std::path::Path::new(":memory:"))?;
    let mut add = |title, status, planned, due, priority| {
        db.create_scheduled(
            title,
            None,
            status,
            "test",
            None,
            schedule(planned, due, priority),
        )
    };
    let low = add(
        "Old low deadline",
        Status::Pending,
        None,
        Some("2026-10-01"),
        Priority::Low,
    )?;
    let high = add(
        "Today's priority",
        Status::Active,
        None,
        Some("2026-10-06"),
        Priority::High,
    )?;
    let carried = add(
        "Carry plan forward",
        Status::Pending,
        Some("2026-10-05"),
        None,
        Priority::Normal,
    )?;
    let replanned = add(
        "Late plan, overdue deadline",
        Status::Pending,
        Some("2026-10-10"),
        Some("2026-10-05"),
        Priority::Normal,
    )?;
    let both = add(
        "Both selectors",
        Status::Pending,
        Some("2026-10-06"),
        Some("2026-10-06"),
        Priority::Normal,
    )?;
    let blocked = add(
        "Blocked deadline",
        Status::Blocked,
        None,
        Some("2026-10-06"),
        Priority::High,
    )?;
    let waiting = add(
        "Waiting plan",
        Status::Waiting,
        Some("2026-10-06"),
        None,
        Priority::Normal,
    )?;
    let edge = add(
        "Inclusive end",
        Status::Pending,
        None,
        Some("2026-10-08"),
        Priority::Normal,
    )?;
    add(
        "Beyond window",
        Status::Pending,
        Some("2026-10-09"),
        Some("2026-10-09"),
        Priority::High,
    )?;
    add("Undated high", Status::Active, None, None, Priority::High)?;
    add(
        "Done",
        Status::Done,
        None,
        Some("2026-10-01"),
        Priority::High,
    )?;
    add(
        "Cancelled",
        Status::Cancelled,
        None,
        Some("2026-10-01"),
        Priority::High,
    )?;
    let deleted = add(
        "Deleted",
        Status::Pending,
        None,
        Some("2026-10-01"),
        Priority::High,
    )?;
    db.set_status(deleted.id, Status::Deleted, "test", None)?;
    let before_history = db.history(carried.id)?.len();
    let view = db.todo_view(at("2026-10-06T00:00:00Z", 1), false)?;
    let ids: Vec<_> = view.actions.iter().map(|r| r.item.id).collect();
    assert_eq!(
        ids,
        vec![high.id, replanned.id, both.id, carried.id, low.id]
    );
    assert_eq!(
        view.blocked_waiting
            .iter()
            .map(|r| r.item.id)
            .collect::<Vec<_>>(),
        vec![blocked.id, waiting.id]
    );
    assert!(!view.actions[0].overdue);
    assert!(view.actions[1].overdue);
    assert!(!view.actions[1].carried_over);
    assert!(view.actions[3].carried_over);
    assert!(!view.actions[3].overdue);
    assert!(format_todo(&view).contains("Blocked / waiting"));
    assert!(format_todo(&view).contains("[overdue]"));
    assert_eq!(db.history(carried.id)?.len(), before_history);
    let longer = db.todo_view(at("2026-10-06T00:00:00Z", 3), false)?;
    assert!(longer.actions.iter().any(|r| r.item.id == edge.id));
    // Existing actionable list still includes undated work and its own ordering.
    assert!(
        db.list(ListFilter::Actionable, false, 100)?
            .iter()
            .any(|i| i.title == "Undated high")
    );
    Ok(())
}

#[test]
fn mutations_are_atomic_idempotent_and_clearable() -> Result<()> {
    let mut db = Tracker::open(std::path::Path::new(":memory:"))?;
    let initial = schedule(Some("2026-10-08"), Some("2026-10-06"), Priority::High);
    let item = db.create_scheduled("Original", None, Status::Pending, "test", None, initial)?;
    let created = db.history(item.id)?;
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].changes["due_date"], "2026-10-06");
    let unchanged = ScheduleUpdate {
        planned_date: Some(initial.planned_date),
        due_date: Some(initial.due_date),
        priority: Some(initial.priority),
    };
    db.update_scheduled(item.id, None, None, "test", None, unchanged)?;
    assert_eq!(db.history(item.id)?.len(), 1);
    let clear = ScheduleUpdate {
        planned_date: Some(None),
        due_date: Some(None),
        priority: Some(Priority::Normal),
    };
    db.update_scheduled(
        item.id,
        Some("Updated"),
        None,
        "test",
        Some("reschedule"),
        clear,
    )?;
    let updated = db.get(item.id)?;
    assert_eq!(updated.schedule, Schedule::default());
    let history = db.history(item.id)?;
    assert_eq!(history.len(), 2);
    assert_eq!(history[1].changes["planned_date"]["from"], "2026-10-08");
    assert!(history[1].changes["planned_date"]["to"].is_null());
    let invalid = ScheduleUpdate {
        due_date: Some(Some(chrono::NaiveDate::from_ymd_opt(10000, 1, 1).unwrap())),
        ..Default::default()
    };
    assert!(
        db.update_scheduled(item.id, Some("Must roll back"), None, "test", None, invalid)
            .is_err()
    );
    assert_eq!(db.get(item.id)?.title, "Updated");
    assert_eq!(db.history(item.id)?.len(), 2);
    db.set_status(item.id, Status::Deleted, "test", None)?;
    assert!(
        db.update_scheduled(item.id, None, None, "test", None, unchanged)
            .is_err()
    );
    Ok(())
}

#[test]
fn v1_migration_preserves_history_and_survives_legacy_opens() -> Result<()> {
    let temp = TempDir::new()?;
    let path = temp.path().join("v1.db");
    let legacy = Connection::open(&path)?;
    legacy.execute_batch(include_str!("fixtures/schema_v1.sql"))?;
    legacy.execute("INSERT INTO work_items(id,title,status,created_at,updated_at) VALUES(42,'Legacy','pending',?1,?1)", ["2026-10-01T00:00:00.000Z"])?;
    legacy.execute("INSERT INTO history_entries(work_item_id,kind,actor,occurred_at,changes_json) VALUES(42,'created','legacy',?1,'{}')", ["2026-10-01T00:00:00.000Z"])?;
    let mut db = Tracker::open(&path)?;
    assert_eq!(db.get(42)?.schedule, Schedule::default());
    let history = serde_json::to_value(db.history(42)?)?;
    assert_eq!(history[0]["actor"], "legacy");
    db.update_scheduled(
        42,
        None,
        None,
        "test",
        None,
        ScheduleUpdate {
            due_date: Some(Some(parse_date("2026-10-08").unwrap())),
            priority: Some(Priority::High),
            ..Default::default()
        },
    )?;
    drop(db);
    // Actual legacy initialization and field-specific writes leave the additive fields intact.
    legacy.execute_batch(include_str!("fixtures/schema_v1.sql"))?;
    legacy.execute(
        "UPDATE work_items SET title=?1 WHERE id=?2",
        params!["Legacy updated", 42],
    )?;
    legacy.execute("INSERT INTO work_items(title,status,created_at,updated_at) VALUES('Legacy new','pending',?1,?1)", ["2026-10-01T00:00:00.000Z"])?;
    for _ in 0..2 {
        let db = Tracker::open(&path)?;
        let item = db.get(42)?;
        assert_eq!(item.title, "Legacy updated");
        assert_eq!(item.schedule.priority, Priority::High);
        assert_eq!(item.schedule.due_date.unwrap().to_string(), "2026-10-08");
        assert_eq!(serde_json::to_value(db.history(42)?)?[0], history[0]);
        assert_eq!(db.get(43)?.schedule, Schedule::default());
    }
    legacy.pragma_update(None, "user_version", 3)?;
    assert!(Tracker::open(&path).is_err());
    Ok(())
}

#[test]
fn stable_ties_empty_view_and_no_default_limit() -> Result<()> {
    let mut db = Tracker::open(std::path::Path::new(":memory:"))?;
    let empty = db.todo_view(at("2026-10-06T00:00:00Z", 5), false)?;
    assert!(format_todo(&empty).contains("No scheduled work"));
    for n in 0..105 {
        db.create_scheduled(
            &format!("Item {n}"),
            None,
            Status::Pending,
            "test",
            None,
            schedule(None, Some("2026-10-06"), Priority::Normal),
        )?;
    }
    let view = db.todo_view(at("2026-10-06T00:00:00Z", 1), false)?;
    assert_eq!(view.actions.len(), 105);
    assert!(view.actions.windows(2).all(|w| w[0].item.id < w[1].item.id));
    Ok(())
}
