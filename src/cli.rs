use std::{
    env,
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};

use crate::{
    domain::{DomainValidationError, RepairMode, Status},
    github::RepositoryName,
    ledger::ReadPolicy,
};

#[derive(Debug, Parser)]
#[command(
    name = "work-tracker",
    version,
    about = "Track long-running work across human and agent sessions"
)]
pub struct Cli {
    /// SQLite database path. Defaults to the platform user data directory.
    #[arg(
        long,
        global = true,
        env = "WORK_TRACKER_DB",
        conflicts_with = "repository"
    )]
    pub database: Option<PathBuf>,

    /// Emit machine-readable JSON.
    #[arg(long, global = true)]
    pub json: bool,

    /// Require a successful GitHub synchronization before reading.
    #[arg(long, global = true, conflicts_with = "offline")]
    pub fresh: bool,

    /// Read only from the local cache without contacting GitHub.
    #[arg(long, global = true, conflicts_with = "fresh")]
    pub offline: bool,

    /// Use this GitHub ledger for the current command without changing the default.
    #[arg(
        long,
        global = true,
        value_name = "OWNER/REPO",
        conflicts_with = "database"
    )]
    pub repository: Option<String>,

    #[command(subcommand)]
    pub command: Command,
}

pub struct ParseFailure {
    error: clap::Error,
    json_output: bool,
}

impl ParseFailure {
    pub fn error(&self) -> &clap::Error {
        &self.error
    }

    pub fn json_output(&self) -> bool {
        self.json_output
    }

    pub fn exit_code(&self) -> u8 {
        u8::try_from(self.error.exit_code()).unwrap_or(1)
    }
}

pub fn parse() -> std::result::Result<Cli, ParseFailure> {
    parse_from(env::args_os())
}

fn parse_from(
    arguments: impl IntoIterator<Item = OsString>,
) -> std::result::Result<Cli, ParseFailure> {
    let arguments = arguments.into_iter().collect::<Vec<_>>();
    let json_output = arguments
        .iter()
        .skip(1)
        .take_while(|argument| argument.as_os_str() != "--")
        .any(|argument| argument == "--json");
    Cli::try_parse_from(arguments).map_err(|error| ParseFailure { error, json_output })
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Initialize an authoritative ledger backend.
    Init(InitArgs),
    /// Create a work item.
    Add(AddArgs),
    /// Show one work item, including an Archived Work Item.
    Show(IdArgs),
    /// List actionable work items, or every work item with --all.
    List(ListArgs),
    /// Show items updated today plus every actionable item.
    Today(TodayArgs),
    /// Update a work item's title or description.
    Update(UpdateArgs),
    /// Transition a work item's status.
    Status(StatusArgs),
    /// Add a context note without changing status.
    Note(NoteArgs),
    /// Archive a Work Item permanently while retaining its history.
    #[command(visible_alias = "delete")]
    Archive(ArchiveArgs),
    /// Show the immutable history of a work item.
    History(IdArgs),
    /// Show field or Status proposals rejected by State Revision checks.
    Rejected(IdArgs),
    /// Diagnose structured ledger integrity without modifying GitHub.
    Doctor(IdArgs),
    /// Recover a Work Item from a diagnosed Ledger Integrity Error.
    Recover(RecoverArgs),
    /// Print the database path in use.
    Path,
    /// Host the read-only HTML dashboard.
    Serve(ServeArgs),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandAccess {
    Control,
    Read,
    Write,
    Dashboard,
}

impl Command {
    pub fn access(&self) -> CommandAccess {
        match self {
            Self::Init(_) | Self::Path => CommandAccess::Control,
            Self::Show(_)
            | Self::List(_)
            | Self::Today(_)
            | Self::History(_)
            | Self::Rejected(_)
            | Self::Doctor(_) => CommandAccess::Read,
            Self::Add(_)
            | Self::Update(_)
            | Self::Status(_)
            | Self::Note(_)
            | Self::Archive(_)
            | Self::Recover(_) => CommandAccess::Write,
            Self::Serve(_) => CommandAccess::Dashboard,
        }
    }
}

#[derive(Debug, Args)]
pub struct InitArgs {
    #[command(subcommand)]
    pub backend: InitBackend,
}

#[derive(Debug, Subcommand)]
pub enum InitBackend {
    /// Create or validate a private GitHub Issues ledger.
    Github(InitGithubArgs),
}

#[derive(Debug, Args)]
pub struct InitGithubArgs {
    /// Repository to initialize. Defaults to <authenticated-user>/work-tracker-data.
    #[arg(value_name = "OWNER/REPO", conflicts_with = "repository")]
    pub target: Option<String>,
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

impl Cli {
    pub fn read_policy(&self) -> Result<ReadPolicy> {
        match (self.fresh, self.offline) {
            (true, true) => Err(DomainValidationError::new(
                "--fresh and --offline cannot be used together",
            )
            .into()),
            (true, false) => Ok(ReadPolicy::Fresh),
            (false, true) => Ok(ReadPolicy::Offline),
            (false, false) => Ok(ReadPolicy::PreferFresh),
        }
    }
}

#[derive(Debug, Args)]
pub struct AddArgs {
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

    /// Stable creation identity for safely retrying after cache loss.
    #[arg(long)]
    pub event_id: Option<String>,

    #[command(flatten)]
    pub actor: ActorArgs,
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

    /// Include Archived Work Items.
    #[arg(long, visible_alias = "include-deleted")]
    pub include_archived: bool,

    /// Maximum number of rows.
    #[arg(long, default_value_t = 100)]
    pub limit: usize,
}

#[derive(Debug, Args)]
pub struct TodayArgs {
    /// Include Work Items archived today.
    #[arg(long, visible_alias = "include-deleted")]
    pub include_archived: bool,
}

#[derive(Debug, Args)]
pub struct UpdateArgs {
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

    /// Stable mutation identity for safely retrying an uncertain publication.
    #[arg(long)]
    pub event_id: Option<String>,

    #[command(flatten)]
    pub actor: ActorArgs,
}

#[derive(Debug, Args)]
pub struct StatusArgs {
    pub id: i64,

    #[arg(value_enum)]
    pub status: Status,

    /// Optional transition reason stored in history.
    #[arg(long)]
    pub note: Option<String>,

    /// Stable mutation identity for safely retrying an uncertain publication.
    #[arg(long)]
    pub event_id: Option<String>,

    #[command(flatten)]
    pub actor: ActorArgs,
}

#[derive(Debug, Args)]
pub struct NoteArgs {
    pub id: i64,
    pub message: String,

    /// Stable mutation identity for safely retrying an uncertain publication.
    #[arg(long)]
    pub event_id: Option<String>,

    #[command(flatten)]
    pub actor: ActorArgs,
}

#[derive(Debug, Args)]
pub struct ArchiveArgs {
    pub id: i64,

    /// Optional archival reason stored in history.
    #[arg(long)]
    pub note: Option<String>,

    /// Stable mutation identity for safely retrying an uncertain publication.
    #[arg(long)]
    pub event_id: Option<String>,

    #[command(flatten)]
    pub actor: ActorArgs,
}

#[derive(Debug, Args)]
pub struct RecoverArgs {
    pub id: i64,

    /// Explicit recovery strategy; recovery never falls back automatically.
    #[arg(long, value_enum)]
    pub mode: RepairMode,

    /// Required explanation when establishing a Rebaseline.
    #[arg(long)]
    pub reason: Option<String>,

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

pub fn github_cache_path(repository: &RepositoryName) -> Result<PathBuf> {
    let base = if let Some(base) = env::var_os("XDG_DATA_HOME") {
        PathBuf::from(base)
    } else if let Some(home) = env::var_os("HOME") {
        PathBuf::from(home).join(".local/share")
    } else {
        env::current_dir().context("failed to determine current directory")?
    };
    Ok(base
        .join("work-tracker/github")
        .join(repository.owner())
        .join(format!("{}.db", repository.name())))
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
        assert!(!args.include_archived);
        Ok(())
    }

    #[test]
    fn list_all_conflicts_with_a_status_filter() {
        let error = Cli::try_parse_from(["work-tracker", "list", "--all", "--status", "done"])
            .expect_err("--all and --status must not combine");
        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn fresh_and_offline_are_global_flags() -> Result<()> {
        let fresh = Cli::try_parse_from(["work-tracker", "show", "--fresh", "7"])?;
        assert!(fresh.fresh);
        assert!(!fresh.offline);

        let offline = Cli::try_parse_from(["work-tracker", "--offline", "history", "7"])?;
        assert!(offline.offline);
        assert!(!offline.fresh);

        let conflicting = Cli::try_parse_from(["work-tracker", "--fresh", "list", "--offline"])?;
        assert!(conflicting.read_policy().is_err());

        let clap_conflict = Cli::try_parse_from(["work-tracker", "--fresh", "--offline", "list"])
            .expect_err("same-scope freshness flags must conflict in clap");
        assert_eq!(
            clap_conflict.kind(),
            clap::error::ErrorKind::ArgumentConflict
        );

        Ok(())
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

    #[test]
    fn recover_requires_an_explicit_mode_and_rebaseline_attribution() -> Result<()> {
        let missing_mode = Cli::try_parse_from(["work-tracker", "recover", "7"])
            .expect_err("recovery mode must be explicit");
        assert_eq!(
            missing_mode.kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );

        let restore = Cli::try_parse_from([
            "work-tracker",
            "recover",
            "7",
            "--mode",
            "restore-exact-copy",
        ])?;
        let Command::Recover(restore) = restore.command else {
            anyhow::bail!("expected recover command");
        };
        assert_eq!(restore.mode, RepairMode::RestoreExactCopy);

        let rebaseline = Cli::try_parse_from([
            "work-tracker",
            "recover",
            "7",
            "--mode",
            "rebaseline",
            "--actor",
            "reviewer",
            "--reason",
            "reviewed current state",
        ])?;
        let Command::Recover(rebaseline) = rebaseline.command else {
            anyhow::bail!("expected recover command");
        };
        assert_eq!(rebaseline.mode, RepairMode::Rebaseline);
        assert_eq!(rebaseline.actor.actor.as_deref(), Some("reviewer"));
        assert_eq!(rebaseline.reason.as_deref(), Some("reviewed current state"));
        Ok(())
    }

    #[test]
    fn parse_failure_retains_json_mode_and_clap_exit_code() {
        let failure = parse_from([
            OsString::from("work-tracker"),
            OsString::from("--json"),
            OsString::from("list"),
            OsString::from("--all"),
            OsString::from("--status"),
            OsString::from("done"),
        ])
        .expect_err("conflicting arguments must fail");
        assert!(failure.json_output());
        assert_eq!(failure.exit_code(), 2);
        assert!(failure.error().use_stderr());
    }

    #[test]
    fn json_text_after_the_argument_delimiter_does_not_select_json_diagnostics() {
        let failure = parse_from([
            OsString::from("work-tracker"),
            OsString::from("add"),
            OsString::from("--"),
            OsString::from("--json"),
            OsString::from("unexpected"),
        ])
        .expect_err("extra positional input must fail");
        assert!(!failure.json_output());
    }
}
