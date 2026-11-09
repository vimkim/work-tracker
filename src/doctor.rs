use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;

use crate::db::{SCHEMA_VERSION, backup};

/// Twice the daily backup interval, so one late run is not a warning.
const BACKUP_MAX_AGE_HOURS: i64 = 48;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Serialize)]
pub struct Check {
    pub name: &'static str,
    pub status: CheckStatus,
    pub detail: String,
    pub required: bool,
}

impl Check {
    fn new(name: &'static str, required: bool, result: (CheckStatus, String)) -> Self {
        Self {
            name,
            status: result.0,
            detail: result.1,
            required,
        }
    }

    pub fn failed_required(&self) -> bool {
        self.required && self.status == CheckStatus::Fail
    }
}

/// Inspects the ledger without writing to it: no migration, no WAL switch.
pub fn run(database: &Path, backups: &Path, actor: Option<&str>, now: DateTime<Utc>) -> Vec<Check> {
    let mut checks = Vec::new();
    let connection = if database.exists() {
        Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|error| format!("cannot open {}: {error}", database.display()))
    } else {
        Err(format!("{} does not exist", database.display()))
    };
    match &connection {
        Ok(_) => checks.push(Check::new(
            "database",
            true,
            (CheckStatus::Ok, database.display().to_string()),
        )),
        Err(detail) => checks.push(Check::new(
            "database",
            true,
            (CheckStatus::Fail, detail.clone()),
        )),
    }
    if let Ok(connection) = &connection {
        checks.push(Check::new("schema", true, schema(connection)));
        checks.push(Check::new("integrity", true, integrity(connection)));
        checks.push(Check::new("journal", false, journal(connection)));
    }
    checks.push(Check::new("backup", false, backup_freshness(backups, now)));
    checks.push(Check::new(
        "actor",
        false,
        match actor.filter(|a| !a.trim().is_empty()) {
            Some(actor) => (CheckStatus::Ok, format!("WORK_TRACKER_ACTOR={actor}")),
            None => (
                CheckStatus::Warn,
                "WORK_TRACKER_ACTOR is unset; history falls back to $USER".into(),
            ),
        },
    ));
    checks
}

fn schema(connection: &Connection) -> (CheckStatus, String) {
    match connection.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0)) {
        Ok(version) if version == SCHEMA_VERSION => (CheckStatus::Ok, format!("version {version}")),
        Ok(version) if version > SCHEMA_VERSION => (
            CheckStatus::Fail,
            format!(
                "version {version} is newer than supported {SCHEMA_VERSION}; upgrade work-tracker"
            ),
        ),
        Ok(version) => (
            CheckStatus::Warn,
            format!("version {version}; the next write command migrates it to {SCHEMA_VERSION}"),
        ),
        Err(error) => (
            CheckStatus::Fail,
            format!("cannot read schema version: {error}"),
        ),
    }
}

fn integrity(connection: &Connection) -> (CheckStatus, String) {
    match connection.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0)) {
        Ok(result) if result == "ok" => (CheckStatus::Ok, "quick_check ok".into()),
        Ok(result) => (CheckStatus::Fail, format!("quick_check: {result}")),
        Err(error) => (CheckStatus::Fail, format!("quick_check failed: {error}")),
    }
}

fn journal(connection: &Connection) -> (CheckStatus, String) {
    match connection.pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0)) {
        Ok(mode) if mode.eq_ignore_ascii_case("wal") => (CheckStatus::Ok, "wal".into()),
        Ok(mode) => (
            CheckStatus::Warn,
            format!("{mode}; concurrent agents expect wal, which the next write command enables"),
        ),
        Err(error) => (
            CheckStatus::Warn,
            format!("cannot read journal mode: {error}"),
        ),
    }
}

fn backup_freshness(directory: &Path, now: DateTime<Utc>) -> (CheckStatus, String) {
    let snapshots = match backup::list(directory) {
        Ok(snapshots) => snapshots,
        Err(error) => return (CheckStatus::Warn, format!("{error:#}")),
    };
    let Some(newest) = snapshots.first() else {
        return (
            CheckStatus::Warn,
            format!(
                "no snapshots in {}; is the backup timer enabled?",
                directory.display()
            ),
        );
    };
    let age = now - newest.created_at;
    let hours = age.num_hours();
    if age > Duration::hours(BACKUP_MAX_AGE_HOURS) {
        (
            CheckStatus::Warn,
            format!(
                "newest snapshot is {hours} hours old (limit {BACKUP_MAX_AGE_HOURS}); is the backup timer running?"
            ),
        )
    } else {
        (
            CheckStatus::Ok,
            format!("{} snapshot(s), newest {hours} hours old", snapshots.len()),
        )
    }
}
