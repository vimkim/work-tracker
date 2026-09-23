use std::{path::Path, str::FromStr, time::Duration as StdDuration};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local, LocalResult, NaiveTime, TimeZone, Utc};
use rusqlite::{
    Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params, types::Type,
};
use serde_json::{Map, Value, json};

use crate::{
    domain::{HistoryEntry, Status, WorkItem, normalized_optional, normalized_required},
    ledger::{Ledger, ListFilter},
};

const SCHEMA_VERSION: i64 = 3;

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
        let schema_version: i64 =
            connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        match schema_version {
            0 => create_schema(&connection)?,
            1 => {
                migrate_deleted_items_to_archived(&connection)?;
                migrate_github_creation_recovery(&connection)?;
            }
            2 => migrate_github_creation_recovery(&connection)?,
            SCHEMA_VERSION => {}
            version => bail!("unsupported database schema version {version}"),
        }
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;

        Ok(Self { connection })
    }

    #[cfg(test)]
    fn open_in_memory() -> Result<Self> {
        Self::open(Path::new(":memory:"))
    }

    pub(crate) fn pending_github_creation(
        &self,
        request_json: &str,
    ) -> Result<Option<PendingGithubCreation>> {
        self.connection
            .query_row(
                "SELECT event_id, issue_number FROM pending_github_creations
                 WHERE request_json = ?1",
                params![request_json],
                |row| {
                    Ok(PendingGithubCreation {
                        event_id: row.get(0)?,
                        issue_number: row.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub(crate) fn begin_github_creation(&self, request_json: &str, event_id: &str) -> Result<()> {
        self.connection.execute(
            "INSERT INTO pending_github_creations (request_json, event_id)
             VALUES (?1, ?2)",
            params![request_json, event_id],
        )?;
        Ok(())
    }

    pub(crate) fn remember_github_issue(
        &self,
        request_json: &str,
        issue_number: i64,
    ) -> Result<()> {
        self.connection.execute(
            "UPDATE pending_github_creations SET issue_number = ?1
             WHERE request_json = ?2",
            params![issue_number, request_json],
        )?;
        Ok(())
    }

    pub(crate) fn github_cache_is_initialized(&self, repository: &str) -> Result<bool> {
        self.connection
            .query_row(
                "SELECT 1 FROM github_cache_state WHERE repository = ?1",
                params![repository],
                |_| Ok(()),
            )
            .optional()
            .map(|row| row.is_some())
            .map_err(Into::into)
    }

    pub(crate) fn mark_github_cache_initialized(&self, repository: &str) -> Result<()> {
        self.connection.execute(
            "INSERT OR IGNORE INTO github_cache_state (repository) VALUES (?1)",
            params![repository],
        )?;
        Ok(())
    }

    pub(crate) fn finish_github_creation(
        &mut self,
        request_json: &str,
        item: &WorkItem,
        history: &HistoryEntry,
    ) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO work_items
             (id, title, description, status, created_at, updated_at, archived_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                item.id,
                item.title,
                item.description,
                item.status.as_str(),
                timestamp(item.created_at),
                timestamp(item.updated_at),
                item.archived_at.map(timestamp),
            ],
        )?;
        transaction.execute(
            "INSERT INTO history_entries
             (id, work_item_id, kind, actor, note, occurred_at, changes_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                history.id,
                history.work_item_id,
                history.kind,
                history.actor,
                history.note,
                timestamp(history.occurred_at),
                serde_json::to_string(&history.changes)?,
            ],
        )?;
        transaction.execute(
            "DELETE FROM pending_github_creations WHERE request_json = ?1",
            params![request_json],
        )?;
        transaction.commit()?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PendingGithubCreation {
    pub event_id: String,
    pub issue_number: Option<i64>,
}

fn create_schema(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "
            CREATE TABLE IF NOT EXISTS work_items (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                title         TEXT NOT NULL CHECK (length(trim(title)) > 0),
                description   TEXT,
                status        TEXT NOT NULL CHECK (
                    status IN ('pending', 'active', 'waiting', 'blocked', 'done', 'cancelled', 'archived')
                ),
                created_at    TEXT NOT NULL,
                updated_at    TEXT NOT NULL,
                archived_at   TEXT,
                CHECK (
                    (status = 'archived' AND archived_at IS NOT NULL)
                    OR
                    (status != 'archived' AND archived_at IS NULL)
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
            CREATE INDEX IF NOT EXISTS idx_history_work_item
                ON history_entries(work_item_id, id);

            CREATE TABLE IF NOT EXISTS pending_github_creations (
                request_json  TEXT PRIMARY KEY,
                event_id      TEXT NOT NULL UNIQUE,
                issue_number  INTEGER
            );

            CREATE TABLE IF NOT EXISTS github_cache_state (
                repository TEXT PRIMARY KEY
            );

            PRAGMA user_version = 3;
            ",
    )?;
    Ok(())
}

fn migrate_deleted_items_to_archived(connection: &Connection) -> Result<()> {
    connection.execute_batch("BEGIN IMMEDIATE;")?;
    let locked_version: i64 =
        connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if locked_version >= 2 {
        connection.execute_batch("COMMIT;")?;
        return Ok(());
    }
    if locked_version != 1 {
        connection.execute_batch("ROLLBACK;")?;
        bail!("cannot migrate database schema version {locked_version}");
    }

    // Keep this v1-to-v2 schema snapshot self-contained. Future schema versions
    // must add a new migration instead of changing the historical v2 target.
    connection.execute_batch(
        "
        CREATE TABLE work_items_v2 (
            id            INTEGER PRIMARY KEY AUTOINCREMENT,
            title         TEXT NOT NULL CHECK (length(trim(title)) > 0),
            description   TEXT,
            status        TEXT NOT NULL CHECK (
                status IN ('pending', 'active', 'waiting', 'blocked', 'done', 'cancelled', 'archived')
            ),
            created_at    TEXT NOT NULL,
            updated_at    TEXT NOT NULL,
            archived_at   TEXT,
            CHECK (
                (status = 'archived' AND archived_at IS NOT NULL)
                OR
                (status != 'archived' AND archived_at IS NULL)
            )
        );
        CREATE TABLE history_entries_v2 (
            id            INTEGER PRIMARY KEY AUTOINCREMENT,
            work_item_id  INTEGER NOT NULL REFERENCES work_items_v2(id) ON DELETE CASCADE,
            kind          TEXT NOT NULL,
            actor         TEXT NOT NULL CHECK (length(trim(actor)) > 0),
            note          TEXT,
            occurred_at   TEXT NOT NULL,
            changes_json  TEXT NOT NULL
        );
        INSERT INTO work_items_v2
            (id, title, description, status, created_at, updated_at, archived_at)
        SELECT id, title, description,
               CASE status WHEN 'deleted' THEN 'archived' ELSE status END,
               created_at, updated_at,
               CASE status WHEN 'deleted' THEN deleted_at ELSE NULL END
        FROM work_items;
        INSERT INTO history_entries_v2
            (id, work_item_id, kind, actor, note, occurred_at, changes_json)
        SELECT id, work_item_id, kind, actor, note, occurred_at, changes_json
        FROM history_entries;
        DROP TABLE history_entries;
        DROP TABLE work_items;
        ALTER TABLE work_items_v2 RENAME TO work_items;
        ALTER TABLE history_entries_v2 RENAME TO history_entries;
        CREATE INDEX idx_work_items_status_updated
            ON work_items(status, updated_at DESC);
        CREATE INDEX IF NOT EXISTS idx_history_work_item
            ON history_entries(work_item_id, id);
        PRAGMA user_version = 2;
        COMMIT;
        ",
    )?;
    Ok(())
}

fn migrate_github_creation_recovery(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "BEGIN IMMEDIATE;
         CREATE TABLE IF NOT EXISTS pending_github_creations (
             request_json  TEXT PRIMARY KEY,
             event_id      TEXT NOT NULL UNIQUE,
             issue_number  INTEGER
         );
         CREATE TABLE IF NOT EXISTS github_cache_state (
             repository TEXT PRIMARY KEY
         );
         PRAGMA user_version = 3;
         COMMIT;",
    )?;
    Ok(())
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
        if status == Status::Archived {
            bail!("a work item cannot be created with archived status");
        }
        let description = normalized_optional(description);
        let now = Utc::now();
        let timestamp = timestamp(now);
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO work_items
             (title, description, status, created_at, updated_at, archived_at)
             VALUES (?1, ?2, ?3, ?4, ?4, NULL)",
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
        include_archived: bool,
        limit: usize,
    ) -> Result<Vec<WorkItem>> {
        let (status, actionable_only) = match filter {
            ListFilter::Actionable => (None, true),
            ListFilter::All => (None, false),
            ListFilter::Status(status) => (Some(status.as_str()), false),
        };
        let include_archived = include_archived || filter == ListFilter::Status(Status::Archived);
        let mut statement = self.connection.prepare(&format!(
            "SELECT id, title, description, status, created_at, updated_at, archived_at
             FROM work_items
             WHERE (?1 IS NULL OR status = ?1)
               AND (NOT ?2 OR status IN {ACTIONABLE_STATUSES})
               AND (?3 OR status != 'archived')
             ORDER BY {STATUS_PRIORITY_ORDER}
             LIMIT ?4"
        ))?;
        let rows = statement.query_map(
            params![status, actionable_only, include_archived, limit as i64],
            row_to_item,
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    fn daily_view(&self, include_archived: bool) -> Result<Vec<WorkItem>> {
        let (start, end) = local_day_bounds(Utc::now())?;
        let mut statement = self.connection.prepare(&format!(
            "SELECT id, title, description, status, created_at, updated_at, archived_at
             FROM work_items
             WHERE (updated_at >= ?1 AND updated_at < ?2
                    OR status IN {ACTIONABLE_STATUSES})
               AND (?3 OR status != 'archived')
             ORDER BY {STATUS_PRIORITY_ORDER}"
        ))?;
        let rows = statement.query_map(
            params![timestamp(start), timestamp(end), include_archived],
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
        let (archived_at, kind) = if status == Status::Archived {
            (Some(timestamp(now)), "archived")
        } else {
            (None, "status_changed")
        };
        transaction.execute(
            "UPDATE work_items
             SET status = ?1, updated_at = ?2, archived_at = ?3
             WHERE id = ?4",
            params![status.as_str(), timestamp(now), archived_at, id],
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
    if item.status == Status::Archived {
        bail!("work item {} is archived and cannot be modified", item.id);
    }
    Ok(())
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
            "SELECT id, title, description, status, created_at, updated_at, archived_at
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
            "SELECT id, title, description, status, created_at, updated_at, archived_at
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
        archived_at: optional_datetime_column(row, 6)?,
        deleted_at: optional_datetime_column(row, 6)?,
        purge_after: None,
    })
}

fn row_to_history(row: &Row<'_>) -> rusqlite::Result<HistoryEntry> {
    let changes_json: String = row.get(6)?;
    let mut kind: String = row.get(2)?;
    let mut changes: Value =
        serde_json::from_str(&changes_json).map_err(|error| conversion_error(6, error))?;
    canonicalize_archival_history(&mut kind, &mut changes);
    Ok(HistoryEntry {
        id: row.get(0)?,
        work_item_id: row.get(1)?,
        kind,
        actor: row.get(3)?,
        note: row.get(4)?,
        occurred_at: datetime_column(row, 5)?,
        changes,
    })
}

fn canonicalize_archival_history(kind: &mut String, changes: &mut Value) {
    if kind == "deleted" {
        *kind = "archived".to_owned();
    }
    let Some(status_change) = changes.get_mut("status").and_then(Value::as_object_mut) else {
        return;
    };
    for side in ["from", "to"] {
        if status_change.get(side).and_then(Value::as_str) == Some("deleted") {
            status_change.insert(side.to_owned(), Value::String("archived".to_owned()));
        }
    }
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
    fn lifecycle_records_history_and_keeps_archived_item() -> Result<()> {
        let mut tracker = SqliteLedger::open_in_memory()?;
        let item = tracker.create(
            "Watch CI",
            Some("Wait for the queued suite"),
            Status::Pending,
            "agent-a",
            None,
        )?;
        tracker.set_status(item.id, Status::Waiting, "agent-a", Some("CI queued"))?;
        let archived = tracker.set_status(item.id, Status::Archived, "human", Some("obsolete"))?;

        assert_eq!(archived.status, Status::Archived);
        assert_eq!(tracker.history(item.id)?.len(), 3);
        assert!(archived.archived_at.is_some());
        assert_eq!(archived.deleted_at, archived.archived_at);
        assert!(archived.purge_after.is_none());
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
    fn archived_item_and_history_survive_reopening_the_database() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("tracker.db");
        let mut tracker = SqliteLedger::open(&path)?;
        let item = tracker.create("Old work", None, Status::Pending, "human", None)?;
        tracker.set_status(item.id, Status::Archived, "human", None)?;
        tracker.connection.execute(
            "UPDATE work_items SET archived_at = '2020-01-01T00:00:00.000Z' WHERE id = ?1",
            params![item.id],
        )?;
        drop(tracker);

        let tracker = SqliteLedger::open(&path)?;
        assert_eq!(tracker.get(item.id)?.status, Status::Archived);
        assert_eq!(tracker.history(item.id)?.len(), 2);
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
        let archived_item =
            tracker.create("Archived work", None, Status::Pending, "agent", None)?;
        tracker.set_status(archived_item.id, Status::Archived, "agent", None)?;

        let actionable = tracker.list(ListFilter::Actionable, false, 100)?;
        assert_eq!(actionable.len(), 4);
        assert!(actionable.iter().all(|item| item.status.is_actionable()));

        assert_eq!(tracker.list(ListFilter::All, false, 100)?.len(), 6);
        assert_eq!(tracker.list(ListFilter::All, true, 100)?.len(), 7);

        let done = tracker.list(ListFilter::Status(Status::Done), false, 100)?;
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].status, Status::Done);

        let archived = tracker.list(ListFilter::Status(Status::Archived), false, 100)?;
        assert_eq!(archived.len(), 1);
        assert_eq!(archived[0].id, archived_item.id);
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
    fn archiving_twice_is_idempotent() -> Result<()> {
        let mut tracker = SqliteLedger::open_in_memory()?;
        let item = tracker.create("Disposable", None, Status::Pending, "agent", None)?;
        tracker.set_status(item.id, Status::Archived, "agent", None)?;
        tracker.set_status(item.id, Status::Archived, "agent", None)?;
        assert_eq!(tracker.history(item.id)?.len(), 2);
        Ok(())
    }
}
