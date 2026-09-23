use std::{fmt, path::PathBuf};

use anyhow::Result;
use chrono::{DateTime, Utc};

use crate::{
    db::SqliteLedger,
    domain::{HistoryEntry, Status, WorkItem},
    github::{GitHubLedger, RepositoryName},
};

/// Which Work Items a Ledger list operation returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListFilter {
    /// Only Actionable Work Items: pending, active, waiting, or blocked.
    Actionable,
    /// Every status, including done and cancelled.
    All,
    /// Exactly one status.
    Status(Status),
}

/// How a read may use the per-machine cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadPolicy {
    /// Synchronize first, falling back only for GitHub availability failures.
    PreferFresh,
    /// Synchronize successfully or fail without reading cached data.
    Fresh,
    /// Do not contact GitHub and read only an already synchronized cache.
    Offline,
}

/// The freshness and integrity state observed before a read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadHealth {
    Local,
    Fresh {
        synchronized_at: DateTime<Utc>,
    },
    Stale {
        last_successful_sync_at: DateTime<Utc>,
        reason: String,
        offline: bool,
    },
    Unavailable {
        reason: String,
    },
    Integrity {
        last_successful_sync_at: Option<DateTime<Utc>>,
        reason: String,
        error_kind: ReadHealthErrorKind,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadHealthKind {
    Local,
    Fresh,
    Stale,
    Unavailable,
    Integrity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadHealthErrorKind {
    CacheUnavailable,
    MetadataCollision,
    IncompatibleMetadata,
}

#[derive(Debug)]
pub struct ReadHealthError {
    kind: ReadHealthErrorKind,
    message: String,
}

impl ReadHealthError {
    pub fn kind(&self) -> ReadHealthErrorKind {
        self.kind
    }
}

impl fmt::Display for ReadHealthError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ReadHealthError {}

impl ReadHealth {
    pub fn kind(&self) -> ReadHealthKind {
        match self {
            Self::Local => ReadHealthKind::Local,
            Self::Fresh { .. } => ReadHealthKind::Fresh,
            Self::Stale { .. } => ReadHealthKind::Stale,
            Self::Unavailable { .. } => ReadHealthKind::Unavailable,
            Self::Integrity { .. } => ReadHealthKind::Integrity,
        }
    }

    pub fn last_successful_sync_at(&self) -> Option<&DateTime<Utc>> {
        match self {
            Self::Fresh { synchronized_at } => Some(synchronized_at),
            Self::Stale {
                last_successful_sync_at,
                ..
            } => Some(last_successful_sync_at),
            Self::Integrity {
                last_successful_sync_at,
                ..
            } => last_successful_sync_at.as_ref(),
            Self::Local | Self::Unavailable { .. } => None,
        }
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Stale { reason, .. }
            | Self::Unavailable { reason }
            | Self::Integrity { reason, .. } => Some(reason),
            Self::Local | Self::Fresh { .. } => None,
        }
    }

    pub fn is_offline(&self) -> bool {
        matches!(self, Self::Stale { offline: true, .. })
    }

    pub fn is_unavailable(&self) -> bool {
        matches!(self, Self::Unavailable { .. })
    }

    pub fn unreadable_error(&self) -> Option<ReadHealthError> {
        match self {
            Self::Unavailable { reason } => Some(ReadHealthError {
                kind: ReadHealthErrorKind::CacheUnavailable,
                message: format!(
                    "GitHub data is unavailable and no synchronized cache can be used: {reason}"
                ),
            }),
            Self::Integrity {
                last_successful_sync_at: None,
                reason,
                error_kind,
            } => Some(ReadHealthError {
                kind: *error_kind,
                message: reason.clone(),
            }),
            _ => None,
        }
    }
}

/// The application-facing seam for Work Item persistence and history.
///
/// Callers use the same interface regardless of where the authoritative ledger
/// lives. An adapter owns validation, atomic mutations, ordering, and storage.
pub trait Ledger: Send {
    fn prepare_read(&mut self, policy: ReadPolicy) -> Result<ReadHealth>;

    fn create(
        &mut self,
        title: &str,
        description: Option<&str>,
        status: Status,
        actor: &str,
        note: Option<&str>,
    ) -> Result<WorkItem>;

    fn get(&self, id: i64) -> Result<WorkItem>;

    fn list(
        &mut self,
        filter: ListFilter,
        include_archived: bool,
        limit: usize,
    ) -> Result<Vec<WorkItem>>;

    fn daily_view(&mut self, include_archived: bool) -> Result<Vec<WorkItem>>;

    fn update(
        &mut self,
        id: i64,
        title: Option<&str>,
        description: Option<Option<&str>>,
        actor: &str,
        note: Option<&str>,
    ) -> Result<WorkItem>;

    fn set_status(
        &mut self,
        id: i64,
        status: Status,
        actor: &str,
        note: Option<&str>,
    ) -> Result<WorkItem>;

    fn add_note(&mut self, id: i64, message: &str, actor: &str) -> Result<HistoryEntry>;

    fn history(&self, id: i64) -> Result<Vec<HistoryEntry>>;
}

/// Backend configuration shared by CLI dispatch and dashboard handlers.
///
/// Backend selection remains here so callers never construct an adapter or
/// open a persistence connection directly.
#[derive(Debug, Clone)]
pub struct LedgerConfig {
    backend: BackendConfig,
}

#[derive(Debug, Clone)]
enum BackendConfig {
    Sqlite(PathBuf),
    Github {
        repository: RepositoryName,
        cache: PathBuf,
        executable: PathBuf,
    },
}

impl LedgerConfig {
    pub fn sqlite(path: impl Into<PathBuf>) -> Self {
        Self {
            backend: BackendConfig::Sqlite(path.into()),
        }
    }

    pub fn github(repository: RepositoryName, cache: impl Into<PathBuf>) -> Self {
        Self {
            backend: BackendConfig::Github {
                repository,
                cache: cache.into(),
                executable: PathBuf::from("gh"),
            },
        }
    }

    #[cfg(test)]
    pub(crate) fn github_with_executable(
        repository: RepositoryName,
        cache: impl Into<PathBuf>,
        executable: impl Into<PathBuf>,
    ) -> Self {
        Self {
            backend: BackendConfig::Github {
                repository,
                cache: cache.into(),
                executable: executable.into(),
            },
        }
    }

    pub fn open(&self) -> Result<Box<dyn Ledger>> {
        match &self.backend {
            BackendConfig::Sqlite(path) => Ok(Box::new(SqliteLedger::open(path)?)),
            BackendConfig::Github {
                repository,
                cache,
                executable,
            } => Ok(Box::new(GitHubLedger::open(
                repository.clone(),
                cache,
                executable,
            )?)),
        }
    }

    pub fn prepare_github_cache(&self, repository_is_new: bool) -> Result<()> {
        let BackendConfig::Github {
            repository, cache, ..
        } = &self.backend
        else {
            anyhow::bail!("only a GitHub ledger has a GitHub cache");
        };
        let cache = SqliteLedger::open(cache)?;
        if repository_is_new {
            cache.mark_github_cache_initialized(&repository.to_string())?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn seed_github_sync_success(&self, synchronized_at: DateTime<Utc>) -> Result<()> {
        let BackendConfig::Github {
            repository, cache, ..
        } = &self.backend
        else {
            anyhow::bail!("only a GitHub ledger has GitHub synchronization state");
        };
        SqliteLedger::open(cache)?
            .mark_github_sync_success(&repository.to_string(), synchronized_at)
    }
}
