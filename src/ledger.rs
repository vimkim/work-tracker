use std::path::PathBuf;

use anyhow::Result;

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

/// The application-facing seam for Work Item persistence and history.
///
/// Callers use the same interface regardless of where the authoritative ledger
/// lives. An adapter owns validation, atomic mutations, ordering, and storage.
pub trait Ledger: Send {
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
            },
        }
    }

    pub fn open(&self) -> Result<Box<dyn Ledger>> {
        match &self.backend {
            BackendConfig::Sqlite(path) => Ok(Box::new(SqliteLedger::open(path)?)),
            BackendConfig::Github { repository, cache } => {
                Ok(Box::new(GitHubLedger::open(repository.clone(), cache)?))
            }
        }
    }

    pub fn prepare_github_cache(&self, repository_is_new: bool) -> Result<()> {
        let BackendConfig::Github { repository, cache } = &self.backend else {
            anyhow::bail!("only a GitHub ledger has a GitHub cache");
        };
        let cache = SqliteLedger::open(cache)?;
        if repository_is_new {
            cache.mark_github_cache_initialized(&repository.to_string())?;
        }
        Ok(())
    }
}
