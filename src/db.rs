use std::{path::Path, str::FromStr, time::Duration as StdDuration};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Local, LocalResult, NaiveTime, TimeZone, Utc};
use rusqlite::{
    Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params, types::Type,
};
use serde_json::{Map, Value, json};

use crate::{
    domain::{HistoryEntry, Status, WorkItem},
    ledger::{Ledger, ListFilter},
};

const RETENTION_DAYS: i64 = 60;

/// SQL list of the statuses that make a Work Item actionable. Keep in sync with
/// `Status::is_actionable`.
const ACTIONABLE_STATUSES: &str = "('pending', 'active', 'waiting', 'blocked')";

/// Shared ordering for every multi-item view: work that needs attention first,
/// then finished work, most recently updated first within each group.
const STATUS_PRIORITY_ORDER: &str = "CASE status
                 WHEN 'blocked' THEN 0
                 WHEN 'active' THEN 1
                 WHEN 'waiting' THEN 2
                 WHEN 'pending' THEN 3
                 ELSE 4
               END,
               updated_at DESC,
               id DESC";

pub(crate) struct SqliteLedger {
    connection: Connection,
}

impl SqliteLedger {
    pub fn open(path: &Path) -> Result<Self> {
        let connection = Connection::open(path)
            .with_context(|| format!("failed to open database {}", path.display()))?;
        connection.busy_timeout(StdDuration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS work_items (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                title         TEXT NOT NULL CHECK (length(trim(title)) > 0),
                description   TEXT,
                status        TEXT NOT NULL CHECK (
                    status IN ('pending', 'active', 'waiting', 'blocked', 'done', 'cancelled', 'deleted')
                ),
                created_at    TEXT NOT NULL,
                updated_at    TEXT NOT NULL,
                deleted_at    TEXT,
                purge_after   TEXT,
                CHECK (
                    (status = 'deleted' AND deleted_at IS NOT NULL AND purge_after IS NOT NULL)
                    OR
                    (status != 'deleted' AND deleted_at IS NULL AND purge_after IS NULL)
                )
            );

            CREATE TABLE IF NOT EXISTS history_entries (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                work_item_id  INTEGER NOT NULL REFERENCES work_items(id) ON DELETE CASCADE,
                kind          TEXT NOT NULL,
                actor         TEXT NOT NULL CHECK (length(trim(actor)) > 0),
                note          TEXT,
                occurred_at   TEXT NOT NULL,
                changes_json  TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_work_items_status_updated
                ON work_items(status, updated_at DESC);
            CREATE INDEX IF NOT EXISTS idx_work_items_purge_after
                ON work_items(purge_after) WHERE status = 'deleted';
            CREATE INDEX IF NOT EXISTS idx_history_work_item
                ON history_entries(work_item_id, id);

            PRAGMA user_version = 1;
            ",
        )?;

        let tracker = Self { connection };
        tracker.purge_expired(Utc::now())?;
        Ok(tracker)
    }

    #[cfg(test)]
    fn open_in_memory() -> Result<Self> {
        Self::open(Path::new(":memory:"))
    }

    fn purge_expired(&self, now: DateTime<Utc>) -> Result<usize> {
        self.connection
            .execute(
                "DELETE FROM work_items
                 WHERE status = 'deleted' AND purge_after <= ?1",
                params![timestamp(now)],
            )
            .map_err(Into::into)
    }
}

impl Ledger for SqliteLedger {
    fn create(
        &mut self,
        title: &str,
        description: Option<&str>,
        status: Status,
        actor: &str,
        note: Option<&str>,
    ) -> Result<WorkItem> {
        let title = normalized_required(title, "title")?;
        let actor = normalized_required(actor, "actor")?;
        if status == Status::Deleted {
            bail!("a work item cannot be created with deleted status");
        }
        let description = normalized_optional(description);
        let now = Utc::now();
        let timestamp = timestamp(now);
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO work_items
             (title, description, status, created_at, updated_at, deleted_at, purge_after)
             VALUES (?1, ?2, ?3, ?4, ?4, NULL, NULL)",
            params![title, description, status.as_str(), timestamp],
        )?;
        let id = transaction.last_insert_rowid();
        insert_history(
            &transaction,
            id,
            "created",
            actor,
            normalized_optional(note),
            now,
            &json!({"title": title, "description": description, "status": status}),
        )?;
        transaction.commit()?;
        self.get(id)
    }

    fn get(&self, id: i64) -> Result<WorkItem> {
        get_item(&self.connection, id)?.with_context(|| format!("work item {id} not found"))
    }

    fn list(
        &self,
        filter: ListFilter,
        include_deleted: bool,
        limit: usize,
    ) -> Result<Vec<WorkItem>> {
        let (status, actionable_only) = match filter {
            ListFilter::Actionable => (None, true),
            ListFilter::All => (None, false),
            ListFilter::Status(status) => (Some(status.as_str()), false),
        };
        let include_deleted = include_deleted || filter == ListFilter::Status(Status::Deleted);
        let mut statement = self.connection.prepare(&format!(
            "SELECT id, title, description, status, created_at, updated_at, deleted_at, purge_after
             FROM work_items
             WHERE (?1 IS NULL OR status = ?1)
               AND (NOT ?2 OR status IN {ACTIONABLE_STATUSES})
               AND (?3 OR status != 'deleted')
             ORDER BY {STATUS_PRIORITY_ORDER}
             LIMIT ?4"
        ))?;
        let rows = statement.query_map(
            params![status, actionable_only, include_deleted, limit as i64],
            row_to_item,
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    fn daily_view(&self, include_deleted: bool) -> Result<Vec<WorkItem>> {
        let (start, end) = local_day_bounds(Utc::now())?;
        let mut statement = self.connection.prepare(&format!(
            "SELECT id, title, description, status, created_at, updated_at, deleted_at, purge_after
             FROM work_items
             WHERE (updated_at >= ?1 AND updated_at < ?2
                    OR status IN {ACTIONABLE_STATUSES})
               AND (?3 OR status != 'deleted')
             ORDER BY {STATUS_PRIORITY_ORDER}"
        ))?;
        let rows = statement.query_map(
            params![timestamp(start), timestamp(end), include_deleted],
            row_to_item,
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    fn update(
        &mut self,
        id: i64,
        title: Option<&str>,
        description: Option<Option<&str>>,
        actor: &str,
        note: Option<&str>,
    ) -> Result<WorkItem> {
        let actor = normalized_required(actor, "actor")?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = get_item_from_transaction(&transaction, id)?
            .with_context(|| format!("work item {id} not found"))?;
        ensure_mutable(&current)?;

        let new_title = match title {
            Some(value) => normalized_required(value, "title")?,
            None => current.title.clone(),
        };
        let new_description = match description {
            Some(value) => normalized_optional(value),
            None => current.description.clone(),
        };

        let mut changes = Map::new();
        if new_title != current.title {
            changes.insert(
                "title".into(),
                json!({"from": current.title, "to": new_title}),
            );
        }
        if new_description != current.description {
            changes.insert(
                "description".into(),
                json!({"from": current.description, "to": new_description}),
            );
        }
        if changes.is_empty() {
            transaction.commit()?;
            return Ok(current);
        }

        let now = Utc::now();
        transaction.execute(
            "UPDATE work_items
             SET title = ?1, description = ?2, updated_at = ?3
             WHERE id = ?4",
            params![new_title, new_description, timestamp(now), id],
        )?;
        insert_history(
            &transaction,
            id,
            "updated",
            actor,
            normalized_optional(note),
            now,
            &Value::Object(changes),
        )?;
        transaction.commit()?;
        self.get(id)
    }

    fn set_status(
        &mut self,
        id: i64,
        status: Status,
        actor: &str,
        note: Option<&str>,
    ) -> Result<WorkItem> {
        let actor = normalized_required(actor, "actor")?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = get_item_from_transaction(&transaction, id)?
            .with_context(|| format!("work item {id} not found"))?;
        if current.status == status {
            transaction.commit()?;
            return Ok(current);
        }
        ensure_mutable(&current)?;

        let now = Utc::now();
        let (deleted_at, purge_after, kind) = if status == Status::Deleted {
            let purge_after = now + Duration::days(RETENTION_DAYS);
            (
                Some(timestamp(now)),
                Some(timestamp(purge_after)),
                "deleted",
            )
        } else {
            (None, None, "status_changed")
        };
        transaction.execute(
            "UPDATE work_items
             SET status = ?1, updated_at = ?2, deleted_at = ?3, purge_after = ?4
             WHERE id = ?5",
            params![status.as_str(), timestamp(now), deleted_at, purge_after, id],
        )?;
        insert_history(
            &transaction,
            id,
            kind,
            actor,
            normalized_optional(note),
            now,
            &json!({"status": {"from": current.status, "to": status}}),
        )?;
        transaction.commit()?;
        self.get(id)
    }

    fn add_note(&mut self, id: i64, message: &str, actor: &str) -> Result<HistoryEntry> {
        let actor = normalized_required(actor, "actor")?;
        let message = normalized_required(message, "message")?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = get_item_from_transaction(&transaction, id)?
            .with_context(|| format!("work item {id} not found"))?;
        ensure_mutable(&current)?;
        let now = Utc::now();
        insert_history(
            &transaction,
            id,
            "noted",
            actor,
            Some(message),
            now,
            &json!({}),
        )?;
        transaction.execute(
            "UPDATE work_items SET updated_at = ?1 WHERE id = ?2",
            params![timestamp(now), id],
        )?;
        let history_id = transaction.last_insert_rowid();
        transaction.commit()?;
        self.history(id)?
            .into_iter()
            .find(|entry| entry.id == history_id)
            .context("new history entry not found")
    }

    fn history(&self, id: i64) -> Result<Vec<HistoryEntry>> {
        self.get(id)?;
        let mut statement = self.connection.prepare(
            "SELECT id, work_item_id, kind, actor, note, occurred_at, changes_json
             FROM history_entries
             WHERE work_item_id = ?1
             ORDER BY id",
        )?;
        let rows = statement.query_map(params![id], row_to_history)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }
}

fn ensure_mutable(item: &WorkItem) -> Result<()> {
    if item.status == Status::Deleted {
        bail!(
            "work item {} is deleted and cannot be modified during retention",
            item.id
        );
    }
    Ok(())
}

fn normalized_required(value: &str, field: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        bail!("{field} cannot be empty");
    }
    Ok(value.to_owned())
}

fn normalized_optional(value: Option<&str>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_owned())
    })
}

fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn local_day_bounds(now: DateTime<Utc>) -> Result<(DateTime<Utc>, DateTime<Utc>)> {
    let local_date = now.with_timezone(&Local).date_naive();
    let start_naive = local_date.and_time(NaiveTime::MIN);
    let end_naive = local_date
        .succ_opt()
        .context("failed to determine next local day")?
        .and_time(NaiveTime::MIN);
    let start = local_datetime(start_naive)?;
    let end = local_datetime(end_naive)?;
    Ok((start.with_timezone(&Utc), end.with_timezone(&Utc)))
}

fn local_datetime(value: chrono::NaiveDateTime) -> Result<DateTime<Local>> {
    match Local.from_local_datetime(&value) {
        LocalResult::Single(value) | LocalResult::Ambiguous(value, _) => Ok(value),
        LocalResult::None => bail!("local time {value} does not exist"),
    }
}

fn insert_history(
    transaction: &Transaction<'_>,
    work_item_id: i64,
    kind: &str,
    actor: String,
    note: Option<String>,
    occurred_at: DateTime<Utc>,
    changes: &Value,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO history_entries
         (work_item_id, kind, actor, note, occurred_at, changes_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            work_item_id,
            kind,
            actor,
            note,
            timestamp(occurred_at),
            serde_json::to_string(changes)?
        ],
    )?;
    Ok(())
}

fn get_item(connection: &Connection, id: i64) -> Result<Option<WorkItem>> {
    connection
        .query_row(
            "SELECT id, title, description, status, created_at, updated_at, deleted_at, purge_after
             FROM work_items WHERE id = ?1",
            params![id],
            row_to_item,
        )
        .optional()
        .map_err(Into::into)
}

fn get_item_from_transaction(transaction: &Transaction<'_>, id: i64) -> Result<Option<WorkItem>> {
    transaction
        .query_row(
            "SELECT id, title, description, status, created_at, updated_at, deleted_at, purge_after
             FROM work_items WHERE id = ?1",
            params![id],
            row_to_item,
        )
        .optional()
        .map_err(Into::into)
}

fn row_to_item(row: &Row<'_>) -> rusqlite::Result<WorkItem> {
    let status: String = row.get(3)?;
    Ok(WorkItem {
        id: row.get(0)?,
        title: row.get(1)?,
        description: row.get(2)?,
        status: Status::from_str(&status).map_err(|error| {
            conversion_error(
                3,
                std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()),
            )
        })?,
        created_at: datetime_column(row, 4)?,
        updated_at: datetime_column(row, 5)?,
        deleted_at: optional_datetime_column(row, 6)?,
        purge_after: optional_datetime_column(row, 7)?,
    })
}

fn row_to_history(row: &Row<'_>) -> rusqlite::Result<HistoryEntry> {
    let changes_json: String = row.get(6)?;
    Ok(HistoryEntry {
        id: row.get(0)?,
        work_item_id: row.get(1)?,
        kind: row.get(2)?,
        actor: row.get(3)?,
        note: row.get(4)?,
        occurred_at: datetime_column(row, 5)?,
        changes: serde_json::from_str(&changes_json).map_err(|error| conversion_error(6, error))?,
    })
}

fn datetime_column(row: &Row<'_>, index: usize) -> rusqlite::Result<DateTime<Utc>> {
    let value: String = row.get(index)?;
    DateTime::parse_from_rfc3339(&value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| conversion_error(index, error))
}

fn optional_datetime_column(
    row: &Row<'_>,
    index: usize,
) -> rusqlite::Result<Option<DateTime<Utc>>> {
    let value: Option<String> = row.get(index)?;
    value
        .map(|value| {
            DateTime::parse_from_rfc3339(&value)
                .map(|value| value.with_timezone(&Utc))
                .map_err(|error| conversion_error(index, error))
        })
        .transpose()
}

fn conversion_error(
    index: usize,
    error: impl std::error::Error + Send + Sync + 'static,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(index, Type::Text, Box::new(error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_records_history_and_keeps_deleted_item() -> Result<()> {
        let mut tracker = SqliteLedger::open_in_memory()?;
        let item = tracker.create(
            "Watch CI",
            Some("Wait for the queued suite"),
            Status::Pending,
            "agent-a",
            None,
        )?;
        tracker.set_status(item.id, Status::Waiting, "agent-a", Some("CI queued"))?;
        let deleted = tracker.set_status(item.id, Status::Deleted, "human", Some("obsolete"))?;

        assert_eq!(deleted.status, Status::Deleted);
        assert_eq!(tracker.history(item.id)?.len(), 3);
        assert!(deleted.purge_after.is_some());
        assert!(tracker.list(ListFilter::All, false, 100)?.is_empty());
        assert_eq!(tracker.list(ListFilter::All, true, 100)?.len(), 1);
        Ok(())
    }

    #[test]
    fn repeated_status_is_idempotent() -> Result<()> {
        let mut tracker = SqliteLedger::open_in_memory()?;
        let item = tracker.create("Compile", None, Status::Active, "agent-a", None)?;
        tracker.set_status(item.id, Status::Active, "agent-b", None)?;
        assert_eq!(tracker.history(item.id)?.len(), 1);
        Ok(())
    }

    #[test]
    fn purging_removes_item_and_history_after_retention() -> Result<()> {
        let mut tracker = SqliteLedger::open_in_memory()?;
        let item = tracker.create("Old work", None, Status::Pending, "human", None)?;
        let deleted = tracker.set_status(item.id, Status::Deleted, "human", None)?;
        let purge_time = deleted.purge_after.context("missing purge time")? + Duration::seconds(1);

        assert_eq!(tracker.purge_expired(purge_time - Duration::days(1))?, 0);
        assert_eq!(tracker.purge_expired(purge_time)?, 1);
        assert!(tracker.get(item.id).is_err());
        Ok(())
    }

    #[test]
    fn update_records_only_real_changes() -> Result<()> {
        let mut tracker = SqliteLedger::open_in_memory()?;
        let item = tracker.create("Original", None, Status::Pending, "human", None)?;
        tracker.update(
            item.id,
            Some("Changed"),
            Some(Some("details")),
            "agent-a",
            Some("clarified"),
        )?;
        tracker.update(
            item.id,
            Some("Changed"),
            Some(Some("details")),
            "agent-b",
            None,
        )?;

        let history = tracker.history(item.id)?;
        assert_eq!(history.len(), 2);
        assert_eq!(history[1].actor, "agent-a");
        assert_eq!(history[1].note.as_deref(), Some("clarified"));
        Ok(())
    }

    #[test]
    fn note_preserves_context_without_changing_status() -> Result<()> {
        let mut tracker = SqliteLedger::open_in_memory()?;
        let item = tracker.create("Wait for CI", None, Status::Waiting, "agent-a", None)?;
        tracker.add_note(item.id, "Queue position 12", "agent-b")?;

        let current = tracker.get(item.id)?;
        let history = tracker.history(item.id)?;
        assert_eq!(current.status, Status::Waiting);
        assert_eq!(history.len(), 2);
        assert_eq!(history[1].kind, "noted");
        assert_eq!(history[1].note.as_deref(), Some("Queue position 12"));
        Ok(())
    }

    #[test]
    fn daily_view_includes_stale_actionable_and_excludes_stale_done() -> Result<()> {
        let mut tracker = SqliteLedger::open_in_memory()?;
        let actionable = tracker.create("Still blocked", None, Status::Blocked, "agent", None)?;
        let done = tracker.create("Old result", None, Status::Done, "agent", None)?;
        let old = "2020-01-01T00:00:00.000Z";
        tracker.connection.execute(
            "UPDATE work_items SET updated_at = ?1 WHERE id IN (?2, ?3)",
            params![old, actionable.id, done.id],
        )?;

        let items = tracker.daily_view(false)?;
        assert!(items.iter().any(|item| item.id == actionable.id));
        assert!(!items.iter().any(|item| item.id == done.id));
        Ok(())
    }

    #[test]
    fn list_shows_actionable_items_by_default_and_everything_with_all() -> Result<()> {
        let mut tracker = SqliteLedger::open_in_memory()?;
        for status in [
            Status::Pending,
            Status::Active,
            Status::Waiting,
            Status::Blocked,
            Status::Done,
            Status::Cancelled,
        ] {
            tracker.create(&format!("{status} work"), None, status, "agent", None)?;
        }
        let removed = tracker.create("Removed work", None, Status::Pending, "agent", None)?;
        tracker.set_status(removed.id, Status::Deleted, "agent", None)?;

        let actionable = tracker.list(ListFilter::Actionable, false, 100)?;
        assert_eq!(actionable.len(), 4);
        assert!(actionable.iter().all(|item| item.status.is_actionable()));

        assert_eq!(tracker.list(ListFilter::All, false, 100)?.len(), 6);
        assert_eq!(tracker.list(ListFilter::All, true, 100)?.len(), 7);

        let done = tracker.list(ListFilter::Status(Status::Done), false, 100)?;
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].status, Status::Done);

        let deleted = tracker.list(ListFilter::Status(Status::Deleted), false, 100)?;
        assert_eq!(deleted.len(), 1);
        assert_eq!(deleted[0].id, removed.id);
        Ok(())
    }

    #[test]
    fn list_orders_by_status_priority_then_recency() -> Result<()> {
        let mut tracker = SqliteLedger::open_in_memory()?;
        let done = tracker.create("Finished", None, Status::Done, "agent", None)?;
        let older_pending =
            tracker.create("Older pending", None, Status::Pending, "agent", None)?;
        let newer_pending =
            tracker.create("Newer pending", None, Status::Pending, "agent", None)?;
        let waiting = tracker.create("Waiting", None, Status::Waiting, "agent", None)?;
        let active = tracker.create("Active", None, Status::Active, "agent", None)?;
        let blocked = tracker.create("Blocked", None, Status::Blocked, "agent", None)?;
        let cancelled = tracker.create("Abandoned", None, Status::Cancelled, "agent", None)?;
        for (item, day) in [
            (&blocked, 1),
            (&active, 2),
            (&waiting, 3),
            (&older_pending, 4),
            (&newer_pending, 5),
            (&cancelled, 7),
            (&done, 8),
        ] {
            tracker.connection.execute(
                "UPDATE work_items SET updated_at = ?1 WHERE id = ?2",
                params![format!("2026-01-0{day}T00:00:00.000Z"), item.id],
            )?;
        }

        let ids = |items: Vec<WorkItem>| items.into_iter().map(|item| item.id).collect::<Vec<_>>();
        assert_eq!(
            ids(tracker.list(ListFilter::All, false, 100)?),
            vec![
                blocked.id,
                active.id,
                waiting.id,
                newer_pending.id,
                older_pending.id,
                done.id,
                cancelled.id
            ]
        );
        assert_eq!(
            ids(tracker.list(ListFilter::Actionable, false, 100)?),
            vec![
                blocked.id,
                active.id,
                waiting.id,
                newer_pending.id,
                older_pending.id
            ]
        );
        Ok(())
    }

    #[test]
    fn concurrent_connections_do_not_lose_creations() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("tracker.db");
        SqliteLedger::open(&path)?;

        let handles = (0..16)
            .map(|index| {
                let path = path.clone();
                std::thread::spawn(move || -> Result<()> {
                    let mut tracker = SqliteLedger::open(&path)?;
                    tracker.create(
                        &format!("Parallel work {index}"),
                        None,
                        Status::Pending,
                        &format!("agent-{index}"),
                        None,
                    )?;
                    Ok(())
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().expect("writer thread panicked")?;
        }

        let tracker = SqliteLedger::open(&path)?;
        assert_eq!(tracker.list(ListFilter::All, false, 100)?.len(), 16);
        Ok(())
    }

    #[test]
    fn deleting_twice_is_idempotent() -> Result<()> {
        let mut tracker = SqliteLedger::open_in_memory()?;
        let item = tracker.create("Disposable", None, Status::Pending, "agent", None)?;
        tracker.set_status(item.id, Status::Deleted, "agent", None)?;
        tracker.set_status(item.id, Status::Deleted, "agent", None)?;
        assert_eq!(tracker.history(item.id)?.len(), 2);
        Ok(())
    }
}
