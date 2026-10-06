use std::{
    env,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};

use chrono::NaiveDate;

use crate::domain::{Priority, Schedule, ScheduleUpdate, Status, parse_date};

#[derive(Debug, Parser)]
#[command(
    name = "work-tracker",
    version,
    about = "Track long-running work across human and agent sessions"
)]
pub struct Cli {
    /// SQLite database path. Defaults to the platform user data directory.
    #[arg(long, global = true, env = "WORK_TRACKER_DB")]
    pub database: Option<PathBuf>,

    /// Emit machine-readable JSON.
    #[arg(long, global = true)]
    pub json: bool,

    /// Color human-readable output; JSON is always uncolored.
    #[arg(long, global = true, value_enum, default_value = "auto")]
    pub color: crate::output::ColorMode,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create a work item.
    Add(AddArgs),
    /// Show one work item, including a soft-deleted item.
    Show(IdArgs),
    /// List actionable work items, or every work item with --all.
    List(ListArgs),
    /// Show items updated today plus every actionable item.
    Today(TodayArgs),
    /// Show scheduled commitments over consecutive calendar days in Asia/Seoul.
    Todo(TodoArgs),
    /// Update a work item's title, description, or schedule.
    Update(UpdateArgs),
    /// Transition a work item's status.
    Status(StatusArgs),
    /// Add a context note without changing status.
    Note(NoteArgs),
    /// Soft-delete a work item for the 60-day retention window.
    Delete(DeleteArgs),
    /// Show the immutable history of a work item.
    History(IdArgs),
    /// Print the database path in use.
    Path,
    /// Host the read-only HTML dashboard.
    Serve(ServeArgs),
}

#[derive(Debug, Args)]
pub struct ActorArgs {
    /// Human or agent identity recorded in history.
    #[arg(long, env = "WORK_TRACKER_ACTOR")]
    pub actor: Option<String>,
}

impl ActorArgs {
    pub fn resolved(&self) -> String {
        self.actor
            .clone()
            .or_else(|| env::var("USER").ok())
            .filter(|actor| !actor.trim().is_empty())
            .unwrap_or_else(|| "unknown".to_owned())
    }
}

#[derive(Debug, Args)]
pub struct AddArgs {
    #[command(flatten)]
    pub schedule: ScheduleArgs,

    /// Short work item title.
    pub title: String,

    /// Longer context or acceptance detail.
    #[arg(short, long)]
    pub description: Option<String>,

    /// Initial status.
    #[arg(long, value_enum, default_value_t = Status::Pending)]
    pub status: Status,

    /// Optional creation context stored in history.
    #[arg(long)]
    pub note: Option<String>,

    #[command(flatten)]
    pub actor: ActorArgs,
}

#[derive(Debug, Args)]
pub struct ScheduleArgs {
    /// Date from which to keep this work visible (YYYY-MM-DD).
    #[arg(long, value_parser = parse_date)]
    pub planned: Option<NaiveDate>,
    /// Completion deadline in Asia/Seoul (YYYY-MM-DD).
    #[arg(long, value_parser = parse_date)]
    pub due: Option<NaiveDate>,
    #[arg(long, value_enum)]
    pub priority: Option<Priority>,
}

impl ScheduleArgs {
    pub fn for_creation(&self) -> Schedule {
        Schedule {
            planned_date: self.planned,
            due_date: self.due,
            priority: self.priority.unwrap_or_default(),
        }
    }
}

#[derive(Debug, Args)]
pub struct TodoArgs {
    /// Include done and cancelled work items scheduled within the horizon.
    #[arg(long, overrides_with = "all")]
    pub all: bool,
    /// Explicit single-day horizon; defaults to today when omitted.
    #[arg(value_parser = ["today"], conflicts_with = "days")]
    pub horizon: Option<String>,
    /// Number of consecutive calendar days, including today.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
    pub days: Option<u32>,
}

#[derive(Debug, Args)]
pub struct IdArgs {
    pub id: i64,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Show every status, including done and cancelled work items.
    #[arg(long, conflicts_with = "status")]
    pub all: bool,

    /// Show only this exact status instead of the actionable default.
    #[arg(long, value_enum)]
    pub status: Option<Status>,

    /// Include soft-deleted work items.
    #[arg(long)]
    pub include_deleted: bool,

    /// Maximum number of rows.
    #[arg(long, default_value_t = 100)]
    pub limit: usize,
}

#[derive(Debug, Args)]
pub struct TodayArgs {
    /// Include work items deleted today.
    #[arg(long)]
    pub include_deleted: bool,
}

#[derive(Debug, Args)]
pub struct UpdateArgs {
    #[command(flatten)]
    pub schedule: ScheduleArgs,

    #[arg(long, conflicts_with = "planned")]
    pub clear_planned: bool,

    #[arg(long, conflicts_with = "due")]
    pub clear_due: bool,

    pub id: i64,

    #[arg(long)]
    pub title: Option<String>,

    #[arg(long, conflicts_with = "clear_description")]
    pub description: Option<String>,

    #[arg(long)]
    pub clear_description: bool,

    /// Optional reason stored in history when a field changes.
    #[arg(long)]
    pub note: Option<String>,

    #[command(flatten)]
    pub actor: ActorArgs,
}

impl UpdateArgs {
    pub fn schedule_update(&self) -> ScheduleUpdate {
        ScheduleUpdate {
            planned_date: if self.clear_planned {
                Some(None)
            } else {
                self.schedule.planned.map(Some)
            },
            due_date: if self.clear_due {
                Some(None)
            } else {
                self.schedule.due.map(Some)
            },
            priority: self.schedule.priority,
        }
    }
}

#[derive(Debug, Args)]
pub struct StatusArgs {
    pub id: i64,

    #[arg(value_enum)]
    pub status: Status,

    /// Optional transition reason stored in history.
    #[arg(long)]
    pub note: Option<String>,

    #[command(flatten)]
    pub actor: ActorArgs,
}

#[derive(Debug, Args)]
pub struct NoteArgs {
    pub id: i64,
    pub message: String,

    #[command(flatten)]
    pub actor: ActorArgs,
}

#[derive(Debug, Args)]
pub struct DeleteArgs {
    pub id: i64,

    /// Optional deletion reason stored in history.
    #[arg(long)]
    pub note: Option<String>,

    #[command(flatten)]
    pub actor: ActorArgs,
}

#[derive(Debug, Args)]
pub struct ServeArgs {
    /// Socket address. Keep the localhost default and use an SSH tunnel.
    #[arg(long, default_value = "127.0.0.1:8787")]
    pub bind: String,
}

pub fn database_path(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path);
    }
    if let Some(base) = env::var_os("XDG_DATA_HOME") {
        return Ok(PathBuf::from(base).join("work-tracker/work-tracker.db"));
    }
    if let Some(home) = env::var_os("HOME") {
        return Ok(PathBuf::from(home).join(".local/share/work-tracker/work-tracker.db"));
    }
    Ok(env::current_dir()
        .context("failed to determine current directory")?
        .join("work-tracker.db"))
}

pub fn prepare_database_path(path: &Path) -> Result<()> {
    if path.as_os_str() == ":memory:" {
        return Ok(());
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create database directory {}", parent.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_defaults_to_the_actionable_scope() -> Result<()> {
        let cli = Cli::try_parse_from(["work-tracker", "list"])?;
        let Command::List(args) = cli.command else {
            anyhow::bail!("expected the list command");
        };
        assert!(!args.all);
        assert!(args.status.is_none());
        assert!(!args.include_deleted);
        Ok(())
    }

    #[test]
    fn list_all_conflicts_with_a_status_filter() {
        let error = Cli::try_parse_from(["work-tracker", "list", "--all", "--status", "done"])
            .expect_err("--all and --status must not combine");
        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn relative_database_filename_needs_no_parent_directory() -> Result<()> {
        prepare_database_path(Path::new("tracker.db"))
    }

    #[test]
    fn nested_database_parent_is_created() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let database = directory.path().join("nested/path/tracker.db");
        prepare_database_path(&database)?;
        assert!(database.parent().context("missing parent")?.is_dir());
        Ok(())
    }
}
