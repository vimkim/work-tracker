use std::{
    path::Path,
    str::FromStr,
    time::{Duration as StdDuration, Instant},
};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Local, LocalResult, NaiveTime, TimeZone, Utc};
use rusqlite::{
    Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params, types::Type,
};
use serde_json::{Map, Value, json};

use crate::domain::{
    HistoryEntry, Priority, Schedule, ScheduleUpdate, Status, WorkItem, parse_date,
};
use crate::todo::{TodoView, TodoWindow};

const RETENTION_DAYS: i64 = 60;
const BUSY_TIMEOUT: StdDuration = StdDuration::from_secs(5);

/// SQL list of the statuses that make a Work Item actionable. Keep in sync with
/// `Status::is_actionable`.
const ACTIONABLE_STATUSES: &str = "('pending', 'active', 'waiting', 'blocked')";

/// Shared ordering for the legacy list and Daily View: work that needs attention first,
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

/// Which Work Items `Tracker::list` returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListFilter {
    /// Only Actionable Work Items: pending, active, waiting, or blocked.
    Actionable,
    /// Every status, including done and cancelled.
    All,
    /// Exactly one status.
    Status(Status),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkDirectoryFilter<'a> {
    All,
    Exact(&'a str),
    Unknown,
}

/// Fields supplied when creating a Work Item. Directory data is explicit;
/// the persistence layer does not inspect the process working directory.
pub struct NewWorkItem<'a> {
    pub title: &'a str,
    pub description: Option<&'a str>,
    pub status: Status,
    pub schedule: Schedule,
    pub workdir: Option<&'a str>,
}

/// None preserves a field; Some(None) explicitly clears an optional field.
#[derive(Debug, Default)]
pub struct WorkItemUpdate<'a> {
    pub title: Option<&'a str>,
    pub description: Option<Option<&'a str>>,
    pub schedule: ScheduleUpdate,
    pub workdir: Option<Option<&'a str>>,
}

pub struct Tracker {
    connection: Connection,
}

impl Tracker {
    pub fn open(path: &Path) -> Result<Self> {
        let mut connection = Connection::open(path)
            .with_context(|| format!("failed to open database {}", path.display()))?;
        connection.busy_timeout(BUSY_TIMEOUT)?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        enable_wal(&connection)?;
        // Guard the supported version and inspect columns under the same write
        // lock so concurrent migration cannot change the schema between them.
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: i64 =
            transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version > 3 {
            bail!("database schema version {version} is newer than supported version 3");
        }
        transaction.execute_batch(
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
            ",
        )?;

        // Legacy binaries reset user_version to 1. Inspect actual columns under
        // a write lock so repeated and concurrent opens never add them twice.
        let columns = {
            let mut statement = transaction.prepare("PRAGMA table_info(work_items)")?;
            statement
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (name, declaration) in [
            ("planned_date", "TEXT"),
            ("due_date", "TEXT"),
            ("workdir", "TEXT"),
            (
                "priority",
                "TEXT NOT NULL DEFAULT 'normal' CHECK (priority IN ('high', 'normal', 'low'))",
            ),
        ] {
            if !columns.iter().any(|column| column == name) {
                transaction.execute_batch(&format!(
                    "ALTER TABLE work_items ADD COLUMN {name} {declaration}"
                ))?;
            }
        }
        transaction.pragma_update(None, "user_version", 3)?;
        transaction.commit()?;

        let tracker = Self { connection };
        tracker.purge_expired(Utc::now())?;
        Ok(tracker)
    }

    #[cfg(test)]
    fn open_in_memory() -> Result<Self> {
        Self::open(Path::new(":memory:"))
    }

    pub fn create(
        &mut self,
        title: &str,
        description: Option<&str>,
        status: Status,
        actor: &str,
        note: Option<&str>,
    ) -> Result<WorkItem> {
        self.create_scheduled(title, description, status, actor, note, Schedule::default())
    }

    pub fn create_scheduled(
        &mut self,
        title: &str,
        description: Option<&str>,
        status: Status,
        actor: &str,
        note: Option<&str>,
        schedule: Schedule,
    ) -> Result<WorkItem> {
        self.create_item(
            NewWorkItem {
                title,
                description,
                status,
                schedule,
                workdir: None,
            },
            actor,
            note,
        )
    }

    pub fn create_item(
        &mut self,
        fields: NewWorkItem<'_>,
        actor: &str,
        note: Option<&str>,
    ) -> Result<WorkItem> {
        let NewWorkItem {
            title,
            description,
            status,
            schedule,
            workdir,
        } = fields;
        schedule.validate()?;
        validate_work_directory(workdir)?;
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
             (title, description, status, created_at, updated_at, deleted_at, purge_after, planned_date, due_date, priority, workdir)
             VALUES (?1, ?2, ?3, ?4, ?4, NULL, NULL, ?5, ?6, ?7, ?8)",
            params![title, description, status.as_str(), timestamp,
                schedule.planned_date.map(|d| d.to_string()), schedule.due_date.map(|d| d.to_string()), schedule.priority.as_str(), workdir],
        )?;
        let id = transaction.last_insert_rowid();
        insert_history(
            &transaction,
            id,
            "created",
            actor,
            normalized_optional(note),
            now,
            &json!({"title": title, "description": description, "status": status,
                "planned_date": schedule.planned_date, "due_date": schedule.due_date, "priority": schedule.priority,
                "workdir": workdir}),
        )?;
        transaction.commit()?;
        self.get(id)
    }

    pub fn get(&self, id: i64) -> Result<WorkItem> {
        get_item(&self.connection, id)?.with_context(|| format!("work item {id} not found"))
    }

    pub fn list(
        &self,
        filter: ListFilter,
        include_deleted: bool,
        limit: usize,
    ) -> Result<Vec<WorkItem>> {
        self.list_in_directory(filter, WorkDirectoryFilter::All, include_deleted, limit)
    }

    pub fn list_in_directory(
        &self,
        filter: ListFilter,
        directory: WorkDirectoryFilter<'_>,
        include_deleted: bool,
        limit: usize,
    ) -> Result<Vec<WorkItem>> {
        let (workdir, unknown_only) = match directory {
            WorkDirectoryFilter::All => (None, false),
            WorkDirectoryFilter::Exact(path) => (Some(path), false),
            WorkDirectoryFilter::Unknown => (None, true),
        };
        let (status, actionable_only) = match filter {
            ListFilter::Actionable => (None, true),
            ListFilter::All => (None, false),
            ListFilter::Status(status) => (Some(status.as_str()), false),
        };
        let include_deleted = include_deleted || filter == ListFilter::Status(Status::Deleted);
        let mut statement = self.connection.prepare(&format!(
            "SELECT id, title, description, status, created_at, updated_at, deleted_at, purge_after, planned_date, due_date, priority, workdir
             FROM work_items
             WHERE (?1 IS NULL OR status = ?1)
               AND (NOT ?2 OR status IN {ACTIONABLE_STATUSES})
               AND (?3 OR status != 'deleted')
               AND (?5 IS NULL OR workdir = ?5)
               AND (NOT ?6 OR workdir IS NULL)
             ORDER BY {STATUS_PRIORITY_ORDER}
             LIMIT ?4"
        ))?;
        let rows = statement.query_map(
            params![
                status,
                actionable_only,
                include_deleted,
                limit as i64,
                workdir,
                unknown_only
            ],
            row_to_item,
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn daily_view(&self, include_deleted: bool) -> Result<Vec<WorkItem>> {
        let (start, end) = local_day_bounds(Utc::now())?;
        let mut statement = self.connection.prepare(&format!(
            "SELECT id, title, description, status, created_at, updated_at, deleted_at, purge_after, planned_date, due_date, priority, workdir
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

    pub fn todo_view(&self, window: TodoWindow, all: bool) -> Result<TodoView> {
        let mut statement = self.connection.prepare(&format!(
            "SELECT id, title, description, status, created_at, updated_at, deleted_at, purge_after,
                    planned_date, due_date, priority, workdir
             FROM work_items WHERE (status IN {ACTIONABLE_STATUSES}
                                   OR (?2 AND status IN ('done', 'cancelled')))
             AND (planned_date <= ?1 OR due_date <= ?1)"
        ))?;
        let items = statement
            .query_map(params![window.end.to_string(), all], row_to_item)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(TodoView::new(window, items, all))
    }

    pub fn update(
        &mut self,
        id: i64,
        title: Option<&str>,
        description: Option<Option<&str>>,
        actor: &str,
        note: Option<&str>,
    ) -> Result<WorkItem> {
        self.update_scheduled(
            id,
            title,
            description,
            actor,
            note,
            ScheduleUpdate::default(),
        )
    }

    pub fn update_scheduled(
        &mut self,
        id: i64,
        title: Option<&str>,
        description: Option<Option<&str>>,
        actor: &str,
        note: Option<&str>,
        schedule: ScheduleUpdate,
    ) -> Result<WorkItem> {
        self.update_item(
            id,
            WorkItemUpdate {
                title,
                description,
                schedule,
                workdir: None,
            },
            actor,
            note,
        )
    }

    pub fn update_item(
        &mut self,
        id: i64,
        fields: WorkItemUpdate<'_>,
        actor: &str,
        note: Option<&str>,
    ) -> Result<WorkItem> {
        let WorkItemUpdate {
            title,
            description,
            schedule,
            workdir,
        } = fields;
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

        let new_schedule = schedule.apply(current.schedule);
        new_schedule.validate()?;
        let new_workdir = workdir
            .map(|value| value.map(str::to_owned))
            .unwrap_or_else(|| current.workdir.clone());
        validate_work_directory(new_workdir.as_deref())?;
        let mut changes = Map::new();
        for (field, before, after) in [
            ("workdir", json!(current.workdir), json!(new_workdir)),
            (
                "planned_date",
                json!(current.schedule.planned_date),
                json!(new_schedule.planned_date),
            ),
            (
                "due_date",
                json!(current.schedule.due_date),
                json!(new_schedule.due_date),
            ),
            (
                "priority",
                json!(current.schedule.priority),
                json!(new_schedule.priority),
            ),
        ] {
            if before != after {
                changes.insert(field.into(), json!({"from": before, "to": after}));
            }
        }
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
             SET title = ?1, description = ?2, updated_at = ?3,
                 planned_date = ?5, due_date = ?6, priority = ?7, workdir = ?8
             WHERE id = ?4",
            params![
                new_title,
                new_description,
                timestamp(now),
                id,
                new_schedule.planned_date.map(|d| d.to_string()),
                new_schedule.due_date.map(|d| d.to_string()),
                new_schedule.priority.as_str(),
                new_workdir
            ],
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

    pub fn set_status(
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

    pub fn add_note(&mut self, id: i64, message: &str, actor: &str) -> Result<HistoryEntry> {
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

    pub fn history(&self, id: i64) -> Result<Vec<HistoryEntry>> {
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

    pub fn purge_expired(&self, now: DateTime<Utc>) -> Result<usize> {
        self.connection
            .execute(
                "DELETE FROM work_items
                 WHERE status = 'deleted' AND purge_after <= ?1",
                params![timestamp(now)],
            )
            .map_err(Into::into)
    }
}

fn enable_wal(connection: &Connection) -> Result<()> {
    let started = Instant::now();
    loop {
        let remaining = BUSY_TIMEOUT.saturating_sub(started.elapsed());
        connection.busy_timeout(remaining)?;
        match connection.pragma_update(None, "journal_mode", "WAL") {
            Ok(()) => {
                connection.busy_timeout(BUSY_TIMEOUT)?;
                return Ok(());
            }
            Err(rusqlite::Error::SqliteFailure(error, _))
                if error.code == rusqlite::ErrorCode::DatabaseBusy
                    && started.elapsed() < BUSY_TIMEOUT =>
            {
                // SQLite can bypass its busy handler to avoid a deadlock while
                // enabling WAL. Retry the statement after it releases its lock.
                // https://www.sqlite.org/c3ref/busy_handler.html
                std::thread::sleep(
                    StdDuration::from_millis(10)
                        .min(BUSY_TIMEOUT.saturating_sub(started.elapsed())),
                );
            }
            Err(error) => return Err(error).context("failed to enable SQLite WAL"),
        }
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

fn validate_work_directory(workdir: Option<&str>) -> Result<()> {
    if let Some(path) = workdir
        && (path.is_empty() || !Path::new(path).is_absolute())
    {
        bail!("work directory must be a nonempty absolute path");
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
            "SELECT id, title, description, status, created_at, updated_at, deleted_at, purge_after, planned_date, due_date, priority, workdir
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
            "SELECT id, title, description, status, created_at, updated_at, deleted_at, purge_after, planned_date, due_date, priority, workdir
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
        workdir: row.get(11)?,
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
        schedule: Schedule {
            planned_date: date_column(row, 8)?,
            due_date: date_column(row, 9)?,
            priority: Priority::from_str(&row.get::<_, String>(10)?).map_err(|error| {
                conversion_error(
                    10,
                    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()),
                )
            })?,
        },
    })
}

fn date_column(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<chrono::NaiveDate>> {
    let value: Option<String> = row.get(index)?;
    value
        .map(|value| {
            parse_date(&value).map_err(|error| {
                conversion_error(
                    index,
                    std::io::Error::new(std::io::ErrorKind::InvalidData, error),
                )
            })
        })
        .transpose()
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
        let mut tracker = Tracker::open_in_memory()?;
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
        let mut tracker = Tracker::open_in_memory()?;
        let item = tracker.create("Compile", None, Status::Active, "agent-a", None)?;
        tracker.set_status(item.id, Status::Active, "agent-b", None)?;
        assert_eq!(tracker.history(item.id)?.len(), 1);
        Ok(())
    }

    #[test]
    fn purging_removes_item_and_history_after_retention() -> Result<()> {
        let mut tracker = Tracker::open_in_memory()?;
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
        let mut tracker = Tracker::open_in_memory()?;
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
        let mut tracker = Tracker::open_in_memory()?;
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
        let mut tracker = Tracker::open_in_memory()?;
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
        let mut tracker = Tracker::open_in_memory()?;
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
        let mut tracker = Tracker::open_in_memory()?;
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
        Tracker::open(&path)?;

        let handles = (0..16)
            .map(|index| {
                let path = path.clone();
                std::thread::spawn(move || -> Result<()> {
                    let mut tracker = Tracker::open(&path)?;
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

        let tracker = Tracker::open(&path)?;
        assert_eq!(tracker.list(ListFilter::All, false, 100)?.len(), 16);
        Ok(())
    }

    #[test]
    fn deleting_twice_is_idempotent() -> Result<()> {
        let mut tracker = Tracker::open_in_memory()?;
        let item = tracker.create("Disposable", None, Status::Pending, "agent", None)?;
        tracker.set_status(item.id, Status::Deleted, "agent", None)?;
        tracker.set_status(item.id, Status::Deleted, "agent", None)?;
        assert_eq!(tracker.history(item.id)?.len(), 2);
        Ok(())
    }
}
