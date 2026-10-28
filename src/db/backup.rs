use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, NaiveDateTime, Utc};
use rusqlite::{Connection, OpenFlags, backup::Backup, backup::StepResult};
use serde::Serialize;
use tempfile::{Builder, NamedTempFile};

#[derive(Debug, Serialize)]
pub struct Snapshot {
    pub path: PathBuf,
    pub created_at: DateTime<Utc>,
    pub size_bytes: u64,
    #[serde(flatten)]
    pub contents: DatabaseContents,
}

#[derive(Debug, Serialize)]
pub struct DatabaseContents {
    pub work_items: u64,
    pub history_entries: u64,
    pub schema_version: i64,
}

#[derive(Debug, Serialize)]
pub struct Recovery {
    pub database: PathBuf,
    pub snapshot: PathBuf,
    #[serde(flatten)]
    pub contents: DatabaseContents,
}

pub fn directory(database: &Path, selected: Option<PathBuf>) -> PathBuf {
    selected.unwrap_or_else(|| database.parent().unwrap_or(Path::new(".")).join("backups"))
}

pub fn create(database: &Path, directory: &Path) -> Result<Snapshot> {
    let source = open_source(database)?;
    let directory = prepare_directory(directory)?;
    let created_at = Utc::now();
    let prefix = format!("work-tracker-{}-", created_at.format("%Y%m%dT%H%M%S%.9fZ"));
    let temporary = Builder::new()
        .prefix(&prefix)
        .suffix(".partial")
        .tempfile_in(&directory)?;
    let contents = copy_and_verify(&source, &temporary)?;
    let path = temporary.path().with_extension("db");
    publish(temporary, &path)?;
    Ok(Snapshot {
        size_bytes: fs::metadata(&path)?.len(),
        path,
        created_at,
        contents,
    })
}

pub fn list(directory: &Path) -> Result<Vec<Snapshot>> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).context("failed to read snapshot directory"),
    };
    let mut snapshots = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        let Some(created_at) = snapshot_time(&path) else {
            continue;
        };
        let source = open_source(&path)?;
        let contents =
            verify(&source).with_context(|| format!("invalid snapshot {}", path.display()))?;
        snapshots.push(Snapshot {
            path: path.canonicalize()?,
            created_at,
            size_bytes: entry.metadata()?.len(),
            contents,
        });
    }
    snapshots.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.path.cmp(&a.path)));
    Ok(snapshots)
}

pub fn restore(snapshot: &Path, target: &Path) -> Result<Recovery> {
    let source = open_source(snapshot)?;
    verify(&source).context("cannot restore an invalid Work Tracker snapshot")?;
    if fs::symlink_metadata(target).is_ok() {
        bail!(
            "destination {} already exists; choose a new path with --to",
            target.display()
        );
    }
    // A stale WAL at the destination could replace data on the next open.
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut sidecar = target.as_os_str().to_owned();
        sidecar.push(suffix);
        if fs::symlink_metadata(Path::new(&sidecar)).is_ok() {
            bail!("destination has a SQLite sidecar; choose a new path with --to");
        }
    }
    let parent = prepare_directory(target.parent().unwrap_or(Path::new(".")))?;
    let target = parent.join(
        target
            .file_name()
            .context("destination needs a file name")?,
    );
    let temporary = Builder::new()
        .prefix(".work-tracker-restore-")
        .suffix(".partial")
        .tempfile_in(&parent)?;
    let contents = copy_and_verify(&source, &temporary)?;
    publish(temporary, &target)?;
    Ok(Recovery {
        database: target,
        snapshot: snapshot.canonicalize()?,
        contents,
    })
}

fn open_source(path: &Path) -> Result<Connection> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("failed to open existing database {}", path.display()))?;
    connection.busy_timeout(Duration::from_secs(5))?;
    Ok(connection)
}

fn copy_and_verify(source: &Connection, temporary: &NamedTempFile) -> Result<DatabaseContents> {
    let mut destination = Connection::open(temporary.path())?;
    destination.busy_timeout(Duration::from_secs(5))?;
    {
        let backup = Backup::new(source, &mut destination)?;
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if backup.step(256)? == StepResult::Done {
                break;
            }
            if Instant::now() >= deadline {
                bail!("snapshot timed out after 60 seconds; no completed snapshot was published");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let contents = verify(&destination)?;
    destination.pragma_update(None, "journal_mode", "DELETE")?;
    destination.close().map_err(|(_, error)| error)?;
    temporary.as_file().sync_all()?;
    Ok(contents)
}

fn verify(connection: &Connection) -> Result<DatabaseContents> {
    let integrity: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        bail!("SQLite integrity check failed: {integrity}");
    }
    if connection.prepare("PRAGMA foreign_key_check")?.exists([])? {
        bail!("SQLite foreign key check failed");
    }
    let schema_version = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if !(0..=super::SCHEMA_VERSION).contains(&schema_version) {
        bail!("database schema version {schema_version} is not supported by this binary");
    }
    connection.prepare(
        "SELECT id, title, description, status, created_at, updated_at, deleted_at, purge_after
         FROM work_items LIMIT 0",
    )?;
    connection.prepare(
        "SELECT id, work_item_id, kind, actor, note, occurred_at, changes_json
         FROM history_entries LIMIT 0",
    )?;
    Ok(DatabaseContents {
        work_items: connection
            .query_row("SELECT count(*) FROM work_items", [], |row| row.get(0))?,
        history_entries: connection.query_row(
            "SELECT count(*) FROM history_entries",
            [],
            |row| row.get(0),
        )?,
        schema_version,
    })
}

fn prepare_directory(path: &Path) -> Result<PathBuf> {
    let path = if path.as_os_str().is_empty() {
        Path::new(".")
    } else {
        path
    };
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .with_context(|| format!("failed to create directory {}", path.display()))?;
    path.canonicalize()
        .context("failed to resolve snapshot directory")
}

fn publish(temporary: NamedTempFile, path: &Path) -> Result<()> {
    temporary.persist_noclobber(path).with_context(|| {
        format!(
            "failed to publish {} without overwriting an existing file",
            path.display()
        )
    })?;
    #[cfg(unix)]
    File::open(
        path.parent()
            .context("destination needs a parent directory")?,
    )?
    .sync_all()?;
    Ok(())
}

fn snapshot_time(path: &Path) -> Option<DateTime<Utc>> {
    let name = path.file_name()?.to_str()?.strip_prefix("work-tracker-")?;
    if !name.ends_with(".db") {
        return None;
    }
    let (timestamp, _) = name.split_once('-')?;
    NaiveDateTime::parse_from_str(timestamp, "%Y%m%dT%H%M%S%.fZ")
        .ok()
        .map(|time| time.and_utc())
}
