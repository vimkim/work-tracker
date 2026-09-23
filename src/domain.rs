use std::{fmt, str::FromStr};

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(crate) fn normalized_required(value: &str, field: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        bail!("{field} cannot be empty");
    }
    Ok(value.to_owned())
}

pub(crate) fn normalized_optional(value: Option<&str>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_owned())
    })
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "snake_case")]
pub enum Status {
    Pending,
    Active,
    Waiting,
    Blocked,
    Done,
    Cancelled,
    #[serde(alias = "deleted")]
    #[value(alias = "deleted")]
    Archived,
}

impl Status {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Waiting => "waiting",
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Cancelled => "cancelled",
            Self::Archived => "archived",
        }
    }

    pub const fn is_actionable(self) -> bool {
        matches!(
            self,
            Self::Pending | Self::Active | Self::Waiting | Self::Blocked
        )
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.as_str())
    }
}

impl FromStr for Status {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "active" => Ok(Self::Active),
            "waiting" => Ok(Self::Waiting),
            "blocked" => Ok(Self::Blocked),
            "done" => Ok(Self::Done),
            "cancelled" => Ok(Self::Cancelled),
            "archived" | "deleted" => Ok(Self::Archived),
            _ => bail!("invalid status: {value}"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkItem {
    pub id: i64,
    pub title: String,
    pub description: Option<String>,
    pub status: Status,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
    /// Deprecated compatibility alias for `archived_at`.
    pub deleted_at: Option<DateTime<Utc>>,
    /// Deprecated compatibility field. Archival is retained indefinitely.
    pub purge_after: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub ledger_integrity_error: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceTrust {
    Trusted,
    Untrusted,
}

impl EvidenceTrust {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Trusted => "trusted",
            Self::Untrusted => "untrusted",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub id: i64,
    pub work_item_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    pub kind: String,
    pub actor: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub github_actor: Option<String>,
    pub note: Option<String>,
    pub occurred_at: DateTime<Utc>,
    pub changes: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_history_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_revision: Option<u64>,
    pub trust: EvidenceTrust,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrityBreakKind {
    EditedEvent,
    DeletedEvent,
    BrokenHashContinuity,
    HeadMismatch,
    UnknownSchemaVersion,
    UnreadableEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrityHealth {
    Healthy,
    LedgerIntegrityError,
}

impl IntegrityHealth {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::LedgerIntegrityError => "ledger_integrity_error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "kebab-case")]
pub enum RepairMode {
    RestoreExactCopy,
    Rebaseline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryOutcome {
    ExactRestoration,
    Rebaseline,
}

impl RecoveryOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExactRestoration => "exact_restoration",
            Self::Rebaseline => "rebaseline",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryReport {
    pub work_item_id: i64,
    pub outcome: RecoveryOutcome,
    pub archived: bool,
    pub full_history_revalidated: bool,
    pub projection_rebuilt: bool,
    pub trusted_event_count: usize,
    pub untrusted_event_count: usize,
}

impl RepairMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RestoreExactCopy => "restore_exact_copy",
            Self::Rebaseline => "rebaseline",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrityBreak {
    pub kind: IntegrityBreakKind,
    pub github_comment_id: Option<i64>,
    pub event_id: Option<String>,
    pub github_actor: Option<String>,
    pub expected_hash: Option<String>,
    pub observed_hash: Option<String>,
    pub cached_exact_copy: Option<String>,
    pub observed_copy: Option<String>,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrityDoctorReport {
    pub work_item_id: i64,
    pub integrity_health: IntegrityHealth,
    pub archived: bool,
    pub first_break: Option<IntegrityBreak>,
    pub trusted_event_count: usize,
    pub untrusted_event_count: usize,
    pub timeline_evidence: Vec<Value>,
    pub eligible_repair_modes: Vec<RepairMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RejectedMutation {
    pub id: i64,
    pub work_item_id: i64,
    pub event_id: String,
    pub actor: String,
    pub github_actor: String,
    pub note: Option<String>,
    pub occurred_at: DateTime<Utc>,
    pub expected_state_revision: u64,
    pub current_state_revision: u64,
    pub changes: Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_display_honours_column_width() {
        assert_eq!(format!("{:<10}|", Status::Done), "done      |");
        assert_eq!(format!("{}", Status::Cancelled), "cancelled");
    }
}
