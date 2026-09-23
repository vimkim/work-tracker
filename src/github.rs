use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    fmt,
    path::Path,
    process::Command,
    str::FromStr,
};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    db::{GithubCacheCleanup, GithubCacheItem, GithubEventEvidence, SqliteLedger},
    domain::{
        EvidenceTrust, HistoryEntry, IntegrityBreak, IntegrityBreakKind, IntegrityDoctorReport,
        IntegrityHealth, ObservedIntegrityEvidence, RecoveryOutcome, RecoveryReport,
        RejectedMutation, RepairMode, Status, WorkItem, normalized_optional, normalized_required,
    },
    ledger::{Ledger, ListFilter, ReadHealth, ReadHealthErrorKind, ReadPolicy},
};

const PROJECTION_MARKER: &str = "work-tracker:projection";
const EVENT_MARKER: &str = "work-tracker:event";

const LABELS: [(&str, &str, &str); 8] = [
    (
        "work-tracker:item",
        "0052cc",
        "Issue managed by Work Tracker",
    ),
    (
        "work-tracker:status:pending",
        "5319e7",
        "Work Tracker Status: pending",
    ),
    (
        "work-tracker:status:active",
        "0e8a16",
        "Work Tracker Status: active",
    ),
    (
        "work-tracker:status:waiting",
        "fbca04",
        "Work Tracker Status: waiting",
    ),
    (
        "work-tracker:status:blocked",
        "b60205",
        "Work Tracker Status: blocked",
    ),
    (
        "work-tracker:status:done",
        "1d76db",
        "Work Tracker Status: done",
    ),
    (
        "work-tracker:status:cancelled",
        "6a737d",
        "Work Tracker Status: cancelled",
    ),
    (
        "work-tracker:status:archived",
        "24292e",
        "Work Tracker Status: archived",
    ),
];

#[derive(Debug, Clone, Deserialize)]
struct User {
    login: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RepositoryName {
    owner: String,
    name: String,
}

impl RepositoryName {
    pub fn owner(&self) -> &str {
        &self.owner
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn eq_ignore_case(&self, other: &Self) -> bool {
        self.owner.eq_ignore_ascii_case(&other.owner) && self.name.eq_ignore_ascii_case(&other.name)
    }
}

impl fmt::Display for RepositoryName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/{}", self.owner, self.name)
    }
}

impl FromStr for RepositoryName {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        let Some((owner, name)) = value.split_once('/') else {
            bail!("repository must use a valid OWNER/REPO name");
        };
        if owner.is_empty()
            || name.is_empty()
            || name.contains('/')
            || !owner
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            bail!("repository must use a valid OWNER/REPO name");
        }
        Ok(Self {
            owner: owner.to_owned(),
            name: name.to_owned(),
        })
    }
}

impl Serialize for RepositoryName {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for RepositoryName {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(de::Error::custom)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Repository {
    pub full_name: RepositoryName,
    pub private: bool,
    pub has_issues: bool,
    #[serde(default)]
    permissions: Permissions,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Permissions {
    #[serde(default)]
    admin: bool,
    #[serde(default)]
    push: bool,
}

#[derive(Debug, Deserialize)]
struct Label {
    name: String,
    color: String,
    description: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct IssueLabel {
    name: String,
}

#[derive(Debug)]
struct GhFailure {
    stderr: String,
    kind: GhFailureKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GhFailureKind {
    MissingCli,
    NotFound,
    Unauthenticated,
    PermissionDenied,
    ValidationFailed,
    RateLimited,
    NetworkFailure,
    ServiceFailure,
    Other,
}

#[derive(Debug)]
pub struct GitHubError {
    kind: GitHubErrorKind,
    message: String,
    details: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitHubErrorKind {
    CliMissing,
    Unauthenticated,
    PermissionDenied,
    NotFound,
    ValidationFailed,
    RateLimited,
    NetworkFailure,
    ServiceFailure,
    ApiFailure,
    LedgerIntegrity,
    UnknownEventSchema,
    IncompatibleMetadata,
    MetadataCollision,
    InvalidVisibility,
    IncompatibleRepository,
    RejectedMutation,
    ProjectionPending,
    ArchivedImmutable,
    RecoveryValidationFailed,
    RecoveryStillBlocked,
}

impl GitHubError {
    pub fn kind(&self) -> GitHubErrorKind {
        self.kind
    }

    fn new(kind: GitHubErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            details: None,
        }
    }

    fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn details(&self) -> Option<&Value> {
        self.details.as_ref()
    }

    fn is_availability_failure(&self) -> bool {
        matches!(
            self.kind,
            GitHubErrorKind::ApiFailure
                | GitHubErrorKind::CliMissing
                | GitHubErrorKind::RateLimited
                | GitHubErrorKind::NetworkFailure
                | GitHubErrorKind::ServiceFailure
        )
    }
}

impl GitHubErrorKind {
    pub(crate) fn read_health_error_kind(self) -> Option<ReadHealthErrorKind> {
        match self {
            GitHubErrorKind::MetadataCollision => Some(ReadHealthErrorKind::MetadataCollision),
            GitHubErrorKind::IncompatibleMetadata => {
                Some(ReadHealthErrorKind::IncompatibleMetadata)
            }
            GitHubErrorKind::LedgerIntegrity => Some(ReadHealthErrorKind::LedgerIntegrity),
            GitHubErrorKind::UnknownEventSchema => Some(ReadHealthErrorKind::UnknownEventSchema),
            _ => None,
        }
    }
}

impl fmt::Display for GitHubError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for GitHubError {}

#[derive(Debug, Deserialize)]
struct IssueIdentity {
    number: u64,
    #[serde(default)]
    labels: Vec<IssueLabel>,
}

pub struct GitHub {
    executable: String,
    authenticated_login: RefCell<Option<String>>,
}

pub(crate) struct GitHubLedger {
    repository: RepositoryName,
    github: GitHub,
    cache: SqliteLedger,
    recover_pending_remotely: bool,
    repaired_work_item_ids: Vec<i64>,
}

#[derive(Debug, Serialize)]
struct CreationRequest<'a> {
    title: &'a str,
    description: Option<&'a str>,
    status: Status,
    actor: &'a str,
    note: Option<&'a str>,
    event_id: Option<&'a str>,
}

#[derive(Debug, Deserialize)]
struct RecoverableCreationRequest {
    title: String,
    description: Option<String>,
    status: Status,
    actor: String,
    note: Option<String>,
    #[serde(default)]
    event_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ProjectionMetadata {
    schema_version: u32,
    kind: String,
    event_id: String,
    creation_fingerprint: String,
    #[serde(default)]
    creation_event_id_supplied: bool,
    pending_genesis_event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending_genesis_event: Option<CanonicalEvent>,
    genesis_comment_id: Option<i64>,
    state_revision: u64,
    #[serde(default)]
    head_event_id: Option<String>,
    #[serde(default)]
    head_comment_id: Option<i64>,
    #[serde(default)]
    history_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CanonicalEvent {
    schema_version: u32,
    event_id: String,
    kind: EventKind,
    actor: String,
    github_actor: String,
    note: Option<String>,
    changes: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expected_state_revision: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum EventKind {
    Created,
    Rebaseline,
    Noted,
    Updated,
    StatusChanged,
    Archived,
}

impl EventKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Rebaseline => "rebaseline",
            Self::Noted => "noted",
            Self::Updated => "updated",
            Self::StatusChanged => "status_changed",
            Self::Archived => "archived",
        }
    }

    fn from_history_name(name: &str) -> Option<Self> {
        match name {
            "created" => Some(Self::Created),
            "rebaseline" => Some(Self::Rebaseline),
            "noted" => Some(Self::Noted),
            "updated" => Some(Self::Updated),
            "status_changed" => Some(Self::StatusChanged),
            "archived" => Some(Self::Archived),
            _ => None,
        }
    }

    const fn operation_name(self) -> Option<&'static str> {
        match self {
            Self::Updated => Some("update"),
            Self::StatusChanged => Some("Status transition"),
            Self::Archived => Some("archive"),
            Self::Created | Self::Rebaseline | Self::Noted => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FieldChange<T> {
    from: T,
    to: T,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FieldChanges {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title: Option<FieldChange<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<FieldChange<Option<String>>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusChanges {
    status: FieldChange<Status>,
}

impl StatusChanges {
    fn is_valid_for(&self, kind: EventKind, item: &WorkItem) -> bool {
        self.status.from != Status::Archived
            && self.status.from == item.status
            && self.status.to != self.status.from
            && match kind {
                EventKind::Archived => self.status.to == Status::Archived,
                EventKind::StatusChanged => self.status.to != Status::Archived,
                _ => false,
            }
    }
}

enum MutationChanges {
    Fields(FieldChanges),
    Status(StatusChanges),
}

impl MutationChanges {
    fn parse(kind: EventKind, value: Value) -> Result<Option<Self>> {
        match kind {
            EventKind::Updated => serde_json::from_value(value)
                .map(Self::Fields)
                .map(Some)
                .context("invalid updated changes"),
            EventKind::StatusChanged | EventKind::Archived => serde_json::from_value(value)
                .map(Self::Status)
                .map(Some)
                .context("invalid Status changes"),
            EventKind::Created | EventKind::Rebaseline | EventKind::Noted => Ok(None),
        }
    }

    fn is_valid_for(&self, kind: EventKind, item: &WorkItem) -> bool {
        match self {
            Self::Fields(changes) => changes.is_valid_for(item),
            Self::Status(changes) => changes.is_valid_for(kind, item),
        }
    }

    fn apply_to(&self, values: &mut GenesisValues) {
        match self {
            Self::Fields(changes) => changes.apply_to(values),
            Self::Status(changes) => values.status = changes.status.to,
        }
    }
}

impl FieldChanges {
    fn is_empty(&self) -> bool {
        self.title.is_none() && self.description.is_none()
    }

    fn is_valid_for(&self, item: &WorkItem) -> bool {
        if self.is_empty() {
            return false;
        }
        let title_is_valid = self.title.as_ref().is_none_or(|change| {
            change.from == item.title
                && change.to != change.from
                && !change.to.is_empty()
                && change.to.trim() == change.to
        });
        let description_is_valid = self.description.as_ref().is_none_or(|change| {
            change.from == item.description
                && change.to != change.from
                && change.to.as_deref().is_none_or(|description| {
                    !description.is_empty() && description.trim() == description
                })
        });
        title_is_valid && description_is_valid
    }

    fn apply_to(&self, values: &mut GenesisValues) {
        if let Some(change) = &self.title {
            values.title.clone_from(&change.to);
        }
        if let Some(change) = &self.description {
            values.description.clone_from(&change.to);
        }
    }
}

#[derive(Debug)]
struct ReplayedHistory {
    accepted: Vec<HistoryEntry>,
    untrusted: Vec<HistoryEntry>,
    rejected: Vec<RejectedMutation>,
}

impl ReplayedHistory {
    fn cache_history(&self) -> Vec<HistoryEntry> {
        self.untrusted
            .iter()
            .chain(&self.accepted)
            .cloned()
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RetainedRecoveryEvidence {
    evidence_id: i64,
    github_comment_id: i64,
    variant: String,
    event_id: Option<String>,
    github_actor: String,
    body: String,
    history_hash: Option<String>,
    occurred_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RebaselineChanges {
    title: String,
    description: Option<String>,
    status: Status,
    integrity_context: IntegrityDoctorReport,
    prior_evidence: Vec<RetainedRecoveryEvidence>,
}

struct PreparedMutation {
    issue: LedgerIssue,
    metadata: ProjectionMetadata,
    replayed: ReplayedHistory,
    current: WorkItem,
    projection_needs_update: bool,
    evidence: Vec<GithubEventEvidence>,
}

struct ProjectionRepair {
    issue: LedgerIssue,
    metadata: ProjectionMetadata,
    history: Vec<HistoryEntry>,
    report_to_caller: bool,
}

struct RebaselineValidationSnapshot {
    issue: LedgerIssue,
    metadata: ProjectionMetadata,
    comments: Vec<LedgerComment>,
    reviewed: WorkItem,
    evidence: Vec<ObservedIntegrityEvidence>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyGenesisEvent {
    schema_version: u32,
    event_id: String,
    kind: String,
    actor: String,
    github_actor: String,
    note: Option<String>,
    initial_values: GenesisValues,
    occurred_at_source: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct GenesisValues {
    title: String,
    description: Option<String>,
    status: Status,
}

#[derive(Debug, Clone, Deserialize)]
struct LedgerIssue {
    number: i64,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    body: String,
    #[serde(default)]
    labels: Vec<IssueLabel>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    state_reason: Option<String>,
    #[serde(default)]
    locked: bool,
    #[serde(default)]
    updated_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pull_request: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct LedgerComment {
    id: i64,
    created_at: DateTime<Utc>,
    user: User,
    #[serde(default)]
    body: String,
}

struct CorrelatedDeletion {
    comment_id: Option<i64>,
    event_id: Option<String>,
    github_actor: Option<String>,
}

enum ConditionalResult<T> {
    NotModified,
    Modified {
        values: Vec<T>,
        etag: Option<String>,
    },
}

struct IncludedResponse<'a> {
    status: u16,
    etag: Option<String>,
    has_next_page: bool,
    body: &'a [u8],
}

impl Default for GitHub {
    fn default() -> Self {
        Self::new()
    }
}

impl GitHubLedger {
    pub(crate) fn open(
        repository: RepositoryName,
        cache_path: &Path,
        executable: &Path,
    ) -> Result<Self> {
        let cache = SqliteLedger::open(cache_path)?;
        let recover_pending_remotely =
            !cache.github_cache_is_initialized(&repository.to_string())?;
        Ok(Self {
            repository,
            github: GitHub::with_executable(executable),
            cache,
            recover_pending_remotely,
            repaired_work_item_ids: Vec::new(),
        })
    }

    fn create_work_item(
        &mut self,
        title: &str,
        description: Option<&str>,
        status: Status,
        actor: &str,
        note: Option<&str>,
        requested_event_id: Option<&str>,
    ) -> Result<WorkItem> {
        let title = normalized_required(title, "title")?;
        let actor = normalized_required(actor, "actor")?;
        if status == Status::Archived {
            bail!("a work item cannot be created with archived status");
        }
        let description = normalized_optional(description);
        let note = normalized_optional(note);
        let requested_event_id = requested_event_id
            .map(|value| normalized_required(value, "event ID"))
            .transpose()?;
        let github_actor = self.github.authenticated_user()?;
        let request = CreationRequest {
            title: &title,
            description: description.as_deref(),
            status,
            actor: &actor,
            note: note.as_deref(),
            event_id: requested_event_id.as_deref(),
        };
        let request_json = serde_json::to_string(&request)?;
        let creation_fingerprint = creation_fingerprint(&request_json);
        let existing = self.cache.pending_github_creation(&request_json)?;
        let (event_id, known_issue, retrying, recovered_issue) = if let Some(pending) = existing {
            (pending.event_id, pending.issue_number, true, None)
        } else if self.recover_pending_remotely && requested_event_id.is_some() {
            if let Some((issue, metadata)) = self.find_creation_by_event_id(
                requested_event_id.as_deref().expect("checked above"),
                &creation_fingerprint,
                status,
            )? {
                self.cache
                    .begin_github_creation(&request_json, &metadata.event_id)?;
                (
                    metadata.event_id.clone(),
                    Some(issue.number),
                    true,
                    Some((issue, metadata)),
                )
            } else {
                let event_id = requested_event_id
                    .clone()
                    .unwrap_or_else(|| new_event_id("genesis"));
                self.cache.begin_github_creation(&request_json, &event_id)?;
                (event_id, None, false, None)
            }
        } else {
            let event_id = requested_event_id
                .clone()
                .unwrap_or_else(|| new_event_id("genesis"));
            self.cache.begin_github_creation(&request_json, &event_id)?;
            (event_id, None, false, None)
        };
        let event = CanonicalEvent {
            schema_version: 1,
            event_id: event_id.clone(),
            kind: EventKind::Created,
            actor: actor.clone(),
            github_actor,
            note: note.clone(),
            changes: json!({
                "title": title.clone(),
                "description": description.clone(),
                "status": status,
            }),
            expected_state_revision: None,
        };
        let pending_metadata = ProjectionMetadata {
            schema_version: 1,
            kind: "work_item".to_owned(),
            event_id: event_id.clone(),
            creation_fingerprint: creation_fingerprint.clone(),
            creation_event_id_supplied: requested_event_id.is_some(),
            pending_genesis_event_id: Some(event_id.clone()),
            pending_genesis_event: Some(event.clone()),
            genesis_comment_id: None,
            state_revision: 0,
            head_event_id: None,
            head_comment_id: None,
            history_hash: None,
        };
        let pending_body = projection_body(description.as_deref(), &pending_metadata)?;

        let (issue, issue_metadata) = if let Some(recovered) = recovered_issue {
            recovered
        } else if let Some(issue_number) = known_issue {
            self.load_creation_issue(issue_number, &event_id, &creation_fingerprint, status)?
        } else if retrying {
            match self.find_pending_issue(&event_id, &creation_fingerprint, status)? {
                Some(issue) => {
                    let metadata = parse_projection(&issue.body)
                        .map_err(|_| metadata_collision(issue.number))?;
                    (issue, metadata)
                }
                None => (
                    self.create_pending_issue(&title, &pending_body, status)?,
                    pending_metadata.clone(),
                ),
            }
        } else {
            (
                self.create_pending_issue(&title, &pending_body, status)?,
                pending_metadata.clone(),
            )
        };
        self.cache
            .remember_github_issue(&request_json, issue.number)?;

        if issue_metadata.pending_genesis_event_id.is_none() {
            return self.recover_completed_creation(&request_json, issue, issue_metadata, &event);
        }

        let comment = if retrying {
            self.find_genesis_comment(issue.number, &event)?
                .map(Ok)
                .unwrap_or_else(|| self.publish_genesis(issue.number, &event))?
        } else {
            self.publish_genesis(issue.number, &event)?
        };
        if retrying && issue.body.contains(PROJECTION_MARKER) {
            let metadata =
                parse_projection(&issue.body).map_err(|_| metadata_collision(issue.number))?;
            if metadata
                .genesis_comment_id
                .is_some_and(|comment_id| comment_id != comment.id)
            {
                return Err(metadata_collision(issue.number).into());
            }
        }

        let history_hash = history_hash(None, &event, comment.id)?;
        let completed_metadata = ProjectionMetadata {
            schema_version: 1,
            kind: "work_item".to_owned(),
            event_id: event_id.clone(),
            creation_fingerprint,
            creation_event_id_supplied: requested_event_id.is_some(),
            pending_genesis_event_id: None,
            pending_genesis_event: None,
            genesis_comment_id: Some(comment.id),
            state_revision: 1,
            head_event_id: Some(event_id.clone()),
            head_comment_id: Some(comment.id),
            history_hash: Some(history_hash.clone()),
        };
        let completed_body = projection_body(description.as_deref(), &completed_metadata)?;
        let status_label = format!("work-tracker:status:{}", status.as_str());
        let state = if status.is_actionable() {
            "open"
        } else {
            "closed"
        };
        let mut fields = vec![
            ("title", title.as_str()),
            ("body", completed_body.as_str()),
            ("labels[]", "work-tracker:item"),
            ("labels[]", status_label.as_str()),
            ("state", state),
        ];
        if !status.is_actionable() {
            fields.push((
                "state_reason",
                if status == Status::Done {
                    "completed"
                } else {
                    "not_planned"
                },
            ));
        }
        self.github.api_empty(
            "PATCH",
            &format!("repos/{}/issues/{}", self.repository, issue.number),
            &fields,
        )?;

        let item = WorkItem {
            id: issue.number,
            title: title.clone(),
            description: description.clone(),
            status,
            created_at: comment.created_at,
            updated_at: comment.created_at,
            archived_at: None,
            deleted_at: None,
            purge_after: None,
            ledger_integrity_error: false,
        };
        let evidence = GithubEventEvidence {
            comment_id: comment.id,
            event_id: Some(event_id.clone()),
            github_actor: event.github_actor.clone(),
            body: event_comment_body(&event)?,
            history_hash: Some(history_hash.clone()),
        };
        let history = HistoryEntry {
            id: comment.id,
            work_item_id: issue.number,
            event_id: Some(event_id),
            kind: EventKind::Created.as_str().to_owned(),
            actor,
            github_actor: Some(event.github_actor),
            note,
            occurred_at: comment.created_at,
            changes: event.changes,
            previous_history_hash: None,
            history_hash: Some(history_hash),
            state_revision: Some(1),
            trust: EvidenceTrust::Trusted,
        };
        self.cache
            .finish_github_creation(&request_json, &item, &history, &evidence)?;
        Ok(item)
    }

    fn audit_unchanged_cached_evidence(&mut self, changed_issue_ids: &HashSet<i64>) -> Result<()> {
        let mut first_error: Option<anyhow::Error> = None;
        for issue_number in self.cache.github_cached_evidence_work_item_ids()? {
            if changed_issue_ids.contains(&issue_number) {
                continue;
            }
            let comments = self.load_comments(issue_number)?;
            let item = self.cache.get(issue_number)?;
            let issue = LedgerIssue {
                number: issue_number,
                title: Some(item.title.clone()),
                body: String::new(),
                labels: vec![
                    IssueLabel {
                        name: "work-tracker:item".to_owned(),
                    },
                    IssueLabel {
                        name: format!("work-tracker:status:{}", item.status.as_str()),
                    },
                ],
                state: Some(if item.status.is_actionable() {
                    "open".to_owned()
                } else {
                    "closed".to_owned()
                }),
                state_reason: None,
                locked: item.status == Status::Archived,
                updated_at: Some(item.updated_at),
                pull_request: None,
            };
            if let Some(report) = self.cached_integrity_report(&issue, &comments, Vec::new())? {
                let detail = report
                    .first_break
                    .as_ref()
                    .map(|first_break| first_break.detail.as_str())
                    .unwrap_or("structured ledger evidence is inconsistent");
                self.cache.record_github_integrity(&report)?;
                first_error
                    .get_or_insert_with(|| ledger_integrity_error(issue_number, detail).into());
                continue;
            }

            let cached_history = self.cache.history(issue_number)?;
            let Some(genesis) = cached_history.first() else {
                continue;
            };
            let head = cached_history
                .last()
                .context("cached Work Tracker history has no head")?;
            let metadata = ProjectionMetadata {
                schema_version: 1,
                kind: "work_item".to_owned(),
                event_id: genesis
                    .event_id
                    .clone()
                    .context("cached genesis omitted event ID")?,
                creation_fingerprint: String::new(),
                creation_event_id_supplied: false,
                pending_genesis_event_id: None,
                pending_genesis_event: None,
                genesis_comment_id: Some(genesis.id),
                state_revision: head
                    .state_revision
                    .context("cached history head omitted State Revision")?,
                head_event_id: head.event_id.clone(),
                head_comment_id: Some(head.id),
                history_hash: head.history_hash.clone(),
            };
            if let Err(error) = replay_trusted_history(issue_number, &comments) {
                let report =
                    self.replay_failure_report(&issue, &metadata, &comments, &error, Vec::new());
                self.cache.record_github_integrity(&report)?;
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn synchronize(&mut self) -> Result<(DateTime<Utc>, Vec<i64>)> {
        let repository = self.repository.to_string();
        let previous_cursor = self.cache.github_sync_cursor(&repository)?;
        let previous_etag = self.cache.github_sync_etag(&repository)?;
        let since = previous_cursor.map(|cursor| {
            format!(
                "&since={}",
                cursor.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
            )
        });
        let endpoint = format!(
            "repos/{}/issues?state=all&sort=updated&direction=asc&per_page=100{}",
            self.repository,
            since.as_deref().unwrap_or_default()
        );
        let (issues, response_etag) = match self
            .github
            .api_conditional_paginated_json::<LedgerIssue>(&endpoint, previous_etag.as_deref())?
        {
            ConditionalResult::NotModified => {
                self.audit_unchanged_cached_evidence(&HashSet::new())?;
                let synchronized_at = Utc::now();
                self.cache
                    .mark_github_sync_success(&repository, synchronized_at)?;
                return Ok((synchronized_at, Vec::new()));
            }
            ConditionalResult::Modified { values, etag } => (values, etag),
        };
        let changed_issue_ids = issues.iter().map(|issue| issue.number).collect();
        self.audit_unchanged_cached_evidence(&changed_issue_ids)?;
        let mut synchronized = Vec::new();
        let mut projection_updates = Vec::new();
        let mut removed_item_ids = Vec::new();
        let mut completed_creation_requests = Vec::new();
        let mut cursor = None;
        for issue in issues {
            let issue_updated_at = issue.updated_at.ok_or_else(|| {
                GitHubError::new(
                    GitHubErrorKind::IncompatibleMetadata,
                    format!("GitHub issue #{} omitted updated_at", issue.number),
                )
            })?;
            cursor = Some(cursor.map_or(issue_updated_at, |current: DateTime<Utc>| {
                current.max(issue_updated_at)
            }));
            if issue.pull_request.is_some() {
                removed_item_ids.push(issue.number);
                continue;
            }
            let has_item_label = issue
                .labels
                .iter()
                .any(|label| label.name.eq_ignore_ascii_case("work-tracker:item"));
            let has_projection_metadata = issue.body.contains(PROJECTION_MARKER);
            if !has_item_label && !has_projection_metadata {
                removed_item_ids.push(issue.number);
                continue;
            }
            if has_item_label != has_projection_metadata {
                return Err(metadata_collision(issue.number).into());
            }
            let mut metadata =
                parse_projection(&issue.body).map_err(|_| metadata_collision(issue.number))?;
            if metadata.schema_version != 1
                || metadata.kind != "work_item"
                || !has_valid_projection_stage(&metadata)
            {
                return Err(metadata_collision(issue.number).into());
            }
            let mut comments: Vec<LedgerComment> = self.github.api_paginated_json(
                "GET",
                &format!(
                    "repos/{}/issues/{}/comments?per_page=100",
                    self.repository, issue.number
                ),
            )?;
            let pending_genesis = metadata.pending_genesis_event_id.is_some();
            if !pending_genesis
                && let Some(report) = self.cached_integrity_report(&issue, &comments, Vec::new())?
            {
                let detail = report
                    .first_break
                    .as_ref()
                    .map(|first_break| first_break.detail.as_str())
                    .unwrap_or("structured ledger evidence is inconsistent");
                self.preserve_integrity_snapshot(&issue, &comments, &report, None)?;
                return Err(ledger_integrity_error(issue.number, detail).into());
            }
            if pending_genesis
                && !comments
                    .iter()
                    .any(|comment| has_metadata(&comment.body, EVENT_MARKER))
            {
                let local_request_json = self
                    .cache
                    .pending_github_creation_request(&metadata.event_id)?;
                let mut event = if let Some(event) = metadata.pending_genesis_event.clone() {
                    validate_pending_genesis_intent(issue.number, &metadata, &event)?;
                    event
                } else if let Some(request_json) = local_request_json.as_deref() {
                    pending_event_from_request(&metadata, request_json)?
                } else {
                    return Err(GitHubError::new(
                        GitHubErrorKind::ProjectionPending,
                        format!(
                            "GitHub issue #{} has an incomplete pending creation whose original intent is unavailable; retry the original add command to complete it",
                            issue.number
                        ),
                    )
                    .with_details(json!({
                        "event_id": metadata.event_id,
                        "projection_pending": true,
                        "instruction": "retry the original add command with the same title, description, Status, Actor, and note",
                    }))
                    .into());
                };
                event.github_actor = self.github.authenticated_user()?;
                let values: GenesisValues = serde_json::from_value(event.changes.clone())
                    .context("invalid pending genesis changes")?;
                validate_status_label(&issue, values.status)?;
                let mut comment = self.publish_genesis(issue.number, &event)?;
                comment.body = event_comment_body(&event)?;
                comments.push(comment);
                if let Some(request_json) = local_request_json {
                    completed_creation_requests.push(request_json);
                }
            } else if pending_genesis
                && let Some(request_json) = self
                    .cache
                    .pending_github_creation_request(&metadata.event_id)?
            {
                completed_creation_requests.push(request_json);
            }
            let replayed = match replay_trusted_history(issue.number, &comments) {
                Ok(replayed) => replayed,
                Err(error) => {
                    let report = self.replay_failure_report(
                        &issue,
                        &metadata,
                        &comments,
                        &error,
                        Vec::new(),
                    );
                    self.preserve_integrity_snapshot(&issue, &comments, &report, None)?;
                    return Err(error);
                }
            };
            let history = &replayed.accepted;
            let projection_needs_update = if pending_genesis {
                let genesis = history.first().ok_or_else(|| {
                    ledger_integrity_error(issue.number, "pending creation has no genesis entry")
                })?;
                if genesis.kind != EventKind::Created.as_str()
                    || genesis.event_id.as_deref() != Some(metadata.event_id.as_str())
                {
                    return Err(ledger_integrity_error(
                        issue.number,
                        "published genesis does not match the pending creation",
                    )
                    .into());
                }
                metadata.pending_genesis_event_id = None;
                metadata.genesis_comment_id = Some(genesis.id);
                metadata.state_revision = 1;
                true
            } else {
                let legacy_genesis = self.cache.legacy_github_genesis_evidence(issue.number)?;
                match projection_head_needs_update(
                    issue.number,
                    &metadata,
                    history,
                    legacy_genesis.as_ref(),
                ) {
                    Ok(needs_update) => needs_update,
                    Err(error) => {
                        let report = self.head_failure_report(
                            &issue,
                            &metadata,
                            history,
                            &error,
                            Vec::new(),
                        );
                        self.preserve_integrity_snapshot(
                            &issue,
                            &comments,
                            &report,
                            Some(history),
                        )?;
                        return Err(error);
                    }
                }
            };
            let item = materialize_item(issue.number, history)?;
            if projection_differs(&issue, &item, projection_needs_update)? {
                let legacy_head_bootstrap = !pending_genesis
                    && metadata.head_event_id.is_none()
                    && metadata.head_comment_id.is_none()
                    && metadata.history_hash.is_none();
                let visible_or_status_drift = readable_projection_differs(&issue, &item)?
                    || status_projection_differs(&issue, item.status)
                    || lock_projection_differs(&issue, item.status);
                projection_updates.push(ProjectionRepair {
                    issue,
                    metadata,
                    history: history.clone(),
                    report_to_caller: pending_genesis
                        || !legacy_head_bootstrap
                        || visible_or_status_drift,
                });
            }
            let evidence = event_evidence(&comments, history);
            synchronized.push(GithubCacheItem {
                item,
                history: replayed.cache_history(),
                rejected: replayed.rejected,
                evidence,
            });
        }
        let repaired_work_item_ids = projection_updates
            .iter()
            .filter(|repair| repair.report_to_caller)
            .map(|repair| repair.issue.number)
            .collect::<Vec<_>>();
        for repair in projection_updates {
            self.project_history_head(&repair.issue, repair.metadata, &repair.history)?;
        }
        let advanced = cursor.is_some_and(|cursor| previous_cursor.is_none_or(|old| cursor > old));
        let synchronized_at = Utc::now();
        self.cache.replace_github_cache_batch(
            &repository,
            &synchronized,
            GithubCacheCleanup {
                removed_item_ids: &removed_item_ids,
                completed_creation_requests: &completed_creation_requests,
            },
            cursor,
            (!advanced).then_some(response_etag.as_deref()).flatten(),
            synchronized_at,
        )?;
        Ok((synchronized_at, repaired_work_item_ids))
    }

    fn load_creation_issue(
        &self,
        issue_number: i64,
        event_id: &str,
        creation_fingerprint: &str,
        status: Status,
    ) -> Result<(LedgerIssue, ProjectionMetadata)> {
        let issue: LedgerIssue = self.github.api_json(
            "GET",
            &format!("repos/{}/issues/{issue_number}", self.repository),
            &[],
        )?;
        let metadata = validate_creation_issue(&issue, event_id, creation_fingerprint, status)?;
        Ok((issue, metadata))
    }

    fn recover_completed_creation(
        &mut self,
        request_json: &str,
        issue: LedgerIssue,
        metadata: ProjectionMetadata,
        event: &CanonicalEvent,
    ) -> Result<WorkItem> {
        let comments = self.load_comments(issue.number)?;
        let replayed = replay_trusted_history(issue.number, &comments)?;
        let genesis = replayed
            .accepted
            .first()
            .context("completed creation has no genesis entry")?;
        let mut expected = event.clone();
        expected.github_actor = genesis
            .github_actor
            .clone()
            .context("completed genesis omitted GitHub Actor")?;
        validate_retry(genesis, &expected)?;
        let projection_needs_update =
            projection_head_needs_update(issue.number, &metadata, &replayed.accepted, None)?;
        let item = materialize_item(issue.number, &replayed.accepted)?;
        if projection_differs(&issue, &item, projection_needs_update)? {
            self.project_history_head(&issue, metadata, &replayed.accepted)?;
            self.repaired_work_item_ids.push(issue.number);
        }
        let evidence = event_evidence(&comments, &replayed.accepted);
        self.cache.finish_github_creation_recovery(
            request_json,
            &GithubCacheItem {
                item: item.clone(),
                history: replayed.cache_history(),
                rejected: replayed.rejected,
                evidence,
            },
        )?;
        Ok(item)
    }

    fn create_pending_issue(&self, title: &str, body: &str, status: Status) -> Result<LedgerIssue> {
        let status_label = format!("work-tracker:status:{}", status.as_str());
        self.github.api_json(
            "POST",
            &format!("repos/{}/issues", self.repository),
            &[
                ("title", title),
                ("body", body),
                ("labels[]", "work-tracker:item"),
                ("labels[]", status_label.as_str()),
            ],
        )
    }

    fn find_pending_issue(
        &self,
        event_id: &str,
        creation_fingerprint: &str,
        status: Status,
    ) -> Result<Option<LedgerIssue>> {
        self.find_pending_issue_by(status, event_id, |metadata| {
            metadata.pending_genesis_event_id.as_deref() == Some(event_id)
                && metadata.event_id == event_id
                && metadata.creation_fingerprint == creation_fingerprint
        })
        .map(|found| found.map(|(issue, _)| issue))
    }

    fn find_creation_by_event_id(
        &self,
        event_id: &str,
        fingerprint: &str,
        status: Status,
    ) -> Result<Option<(LedgerIssue, ProjectionMetadata)>> {
        self.find_pending_issue_by(status, event_id, |metadata| {
            metadata.event_id == event_id && metadata.creation_fingerprint == fingerprint
        })
    }

    fn find_pending_issue_by(
        &self,
        status: Status,
        identity: &str,
        matches: impl Fn(&ProjectionMetadata) -> bool,
    ) -> Result<Option<(LedgerIssue, ProjectionMetadata)>> {
        let issues: Vec<LedgerIssue> = self.github.api_paginated_json(
            "GET",
            &format!("repos/{}/issues?state=all&per_page=100", self.repository),
        )?;
        let mut found = None;
        for issue in issues {
            let has_item_label = issue
                .labels
                .iter()
                .any(|label| label.name.eq_ignore_ascii_case("work-tracker:item"));
            let has_metadata_marker = issue.body.contains(PROJECTION_MARKER);
            if !has_item_label && !has_metadata_marker {
                continue;
            }
            if has_item_label != has_metadata_marker {
                return Err(metadata_collision(issue.number).into());
            }
            let metadata =
                parse_projection(&issue.body).map_err(|_| metadata_collision(issue.number))?;
            if metadata.kind != "work_item" || metadata.schema_version != 1 {
                return Err(metadata_collision(issue.number).into());
            }
            if !has_valid_projection_stage(&metadata) {
                return Err(metadata_collision(issue.number).into());
            }
            if matches(&metadata) {
                if metadata.pending_genesis_event_id.is_some() {
                    validate_status_label(&issue, status)?;
                }
                if found.is_some() {
                    return Err(GitHubError::new(
                        GitHubErrorKind::MetadataCollision,
                        format!("multiple GitHub issues claim pending creation {identity}"),
                    )
                    .into());
                }
                found = Some((issue, metadata));
            }
        }
        Ok(found)
    }

    fn find_genesis_comment(
        &self,
        issue_number: i64,
        expected: &CanonicalEvent,
    ) -> Result<Option<LedgerComment>> {
        let comments: Vec<LedgerComment> = self.github.api_paginated_json(
            "GET",
            &format!(
                "repos/{}/issues/{issue_number}/comments?per_page=100",
                self.repository
            ),
        )?;
        let mut found = None;
        for comment in comments {
            if !has_metadata(&comment.body, EVENT_MARKER) {
                continue;
            }
            let event = parse_event(&comment.body).map_err(|_| {
                GitHubError::new(
                    GitHubErrorKind::MetadataCollision,
                    format!("issue #{issue_number} contains invalid Work Tracker event metadata"),
                )
            })?;
            if event.event_id == expected.event_id {
                let mut expected = expected.clone();
                expected.github_actor = event.github_actor.clone();
                if event != expected || event.github_actor != comment.user.login {
                    return Err(GitHubError::new(
                        GitHubErrorKind::MetadataCollision,
                        format!(
                            "issue #{issue_number} genesis event {} does not match the pending creation",
                            expected.event_id
                        ),
                    )
                    .into());
                }
                if found.is_some() {
                    return Err(GitHubError::new(
                        GitHubErrorKind::MetadataCollision,
                        format!(
                            "issue #{issue_number} contains duplicate event {}",
                            expected.event_id
                        ),
                    )
                    .into());
                }
                found = Some(comment);
            }
        }
        Ok(found)
    }

    fn publish_genesis(&self, issue_number: i64, event: &CanonicalEvent) -> Result<LedgerComment> {
        let body = event_comment_body(event)?;
        let comment: LedgerComment = self.github.api_json(
            "POST",
            &format!("repos/{}/issues/{issue_number}/comments", self.repository),
            &[("body", body.as_str())],
        )?;
        if comment.user.login != event.github_actor {
            return Err(GitHubError::new(
                GitHubErrorKind::MetadataCollision,
                format!(
                    "GitHub created issue #{issue_number} genesis as {}, expected {}",
                    comment.user.login, event.github_actor
                ),
            )
            .into());
        }
        Ok(comment)
    }

    fn load_work_item_issue(&self, issue_number: i64) -> Result<(LedgerIssue, ProjectionMetadata)> {
        let issue: LedgerIssue = self.github.api_json(
            "GET",
            &format!("repos/{}/issues/{issue_number}", self.repository),
            &[],
        )?;
        let metadata = validate_completed_issue(&issue)?;
        Ok((issue, metadata))
    }

    fn load_comments(&self, issue_number: i64) -> Result<Vec<LedgerComment>> {
        self.github.api_paginated_json(
            "GET",
            &format!(
                "repos/{}/issues/{issue_number}/comments?per_page=100",
                self.repository
            ),
        )
    }

    fn load_timeline(&self, issue_number: i64) -> Result<Vec<Value>> {
        self.github.api_paginated_json(
            "GET",
            &format!(
                "repos/{}/issues/{issue_number}/timeline?per_page=100",
                self.repository
            ),
        )
    }

    fn cached_integrity_report(
        &self,
        issue: &LedgerIssue,
        comments: &[LedgerComment],
        timeline_evidence: Vec<Value>,
    ) -> Result<Option<IntegrityDoctorReport>> {
        let cached = self.cache.github_event_evidence(issue.number)?;
        if cached.is_empty() {
            return Ok(None);
        }
        let structured = comments
            .iter()
            .filter(|comment| has_metadata(&comment.body, EVENT_MARKER))
            .collect::<Vec<_>>();
        let replayed = replay_history(issue.number, comments).ok();
        for (index, expected) in cached.iter().enumerate() {
            let observed = comments
                .iter()
                .find(|comment| comment.id == expected.comment_id);
            let (kind, observed_copy, observed_hash, github_actor, detail) = match observed {
                None => (
                    IntegrityBreakKind::DeletedEvent,
                    None,
                    None,
                    Some(expected.github_actor.clone()),
                    format!(
                        "cached structured comment {} is absent from the GitHub comment timeline",
                        expected.comment_id
                    ),
                ),
                Some(observed) if observed.body != expected.body => (
                    IntegrityBreakKind::EditedEvent,
                    Some(observed.body.clone()),
                    replayed.as_ref().and_then(|replayed| {
                        replayed
                            .accepted
                            .iter()
                            .find(|entry| entry.id == observed.id)
                            .and_then(|entry| entry.history_hash.clone())
                    }),
                    Some(observed.user.login.clone()),
                    format!(
                        "structured comment {} differs from the cached exact copy",
                        expected.comment_id
                    ),
                ),
                _ => continue,
            };
            let exact_copy_verified = observed.is_some()
                && cached_exact_chain_is_verified(&cached)
                && parse_projection(&issue.body)
                    .is_ok_and(|metadata| cached_chain_matches_projection(&cached, &metadata))
                && exact_chain_preserves_live_archive(issue, &cached);
            let observed_evidence = observed
                .filter(|observed| observed.body != expected.body)
                .map(observed_integrity_evidence)
                .into_iter()
                .collect::<Vec<_>>();
            let mut untrusted_comment_ids = cached[index..]
                .iter()
                .map(|evidence| evidence.comment_id)
                .collect::<HashSet<_>>();
            untrusted_comment_ids.extend(structured.iter().skip(index).map(|comment| comment.id));
            untrusted_comment_ids.extend(
                observed_evidence
                    .iter()
                    .map(|evidence| evidence.github_comment_id),
            );
            let report = IntegrityDoctorReport {
                work_item_id: issue.number,
                integrity_health: IntegrityHealth::LedgerIntegrityError,
                archived: issue.locked
                    || issue.labels.iter().any(|label| {
                        label
                            .name
                            .eq_ignore_ascii_case("work-tracker:status:archived")
                    }),
                first_break: Some(IntegrityBreak {
                    kind,
                    github_comment_id: Some(expected.comment_id),
                    event_id: expected.event_id.clone(),
                    github_actor,
                    expected_hash: expected.history_hash.clone(),
                    observed_hash,
                    cached_exact_copy: Some(expected.body.clone()),
                    observed_copy,
                    detail,
                }),
                trusted_event_count: index,
                untrusted_event_count: untrusted_comment_ids.len(),
                timeline_evidence,
                eligible_repair_modes: if exact_copy_verified {
                    vec![RepairMode::RestoreExactCopy, RepairMode::Rebaseline]
                } else {
                    vec![RepairMode::Rebaseline]
                },
                observed_evidence,
            };
            return Ok(Some(report));
        }
        Ok(None)
    }

    fn cached_integrity_error(&self, issue_number: i64) -> Result<Option<GitHubError>> {
        Ok(self
            .cache
            .github_integrity_report(issue_number)?
            .and_then(|report| report.first_break)
            .map(|first_break| {
                ledger_integrity_error(issue_number, first_break.detail).with_details(json!({
                    "work_item_id": issue_number,
                    "integrity_error": true,
                }))
            }))
    }

    fn ensure_integrity_allows_mutation(&self, issue_number: i64) -> Result<()> {
        if let Some(error) = self.cached_integrity_error(issue_number)? {
            return Err(error.into());
        }
        Ok(())
    }

    fn decorate_integrity(&self, items: &mut [WorkItem]) -> Result<()> {
        for item in items {
            item.ledger_integrity_error = self.cache.github_integrity_report(item.id)?.is_some();
        }
        Ok(())
    }

    fn preserve_integrity_snapshot(
        &mut self,
        issue: &LedgerIssue,
        comments: &[LedgerComment],
        report: &IntegrityDoctorReport,
        observed_history: Option<&[HistoryEntry]>,
    ) -> Result<()> {
        if self.cache.get(issue.number).is_err() {
            let status = issue
                .labels
                .iter()
                .find_map(|label| {
                    label
                        .name
                        .strip_prefix("work-tracker:status:")
                        .and_then(|status| Status::from_str(status).ok())
                })
                .unwrap_or(Status::Pending);
            let observed_at = comments
                .iter()
                .map(|comment| comment.created_at)
                .min()
                .or(issue.updated_at)
                .unwrap_or_else(Utc::now);
            let updated_at = issue.updated_at.unwrap_or(observed_at);
            let visible = projection_visible_text(&issue.body).unwrap_or_default();
            let description =
                (!matches!(visible, "" | "_No description provided._")).then(|| visible.to_owned());
            let mut history = observed_history.unwrap_or_default().to_vec();
            for entry in &mut history {
                entry.trust = EvidenceTrust::Untrusted;
            }
            let evidence = event_evidence(comments, &history);
            self.cache.replace_github_item(&GithubCacheItem {
                item: WorkItem {
                    id: issue.number,
                    title: issue
                        .title
                        .clone()
                        .unwrap_or_else(|| format!("GitHub issue #{}", issue.number)),
                    description,
                    status,
                    created_at: observed_at,
                    updated_at,
                    archived_at: (status == Status::Archived).then_some(updated_at),
                    deleted_at: (status == Status::Archived).then_some(updated_at),
                    purge_after: None,
                    ledger_integrity_error: true,
                },
                history,
                rejected: Vec::new(),
                evidence,
            })?;
        }
        self.cache.record_github_integrity(report)
    }

    fn correlated_deletion(
        metadata: &ProjectionMetadata,
        timeline_evidence: &[Value],
    ) -> Option<CorrelatedDeletion> {
        timeline_evidence.iter().find_map(|event| {
            let is_deletion = event
                .get("event")
                .and_then(Value::as_str)
                .is_some_and(|event| event.contains("deleted"));
            let comment_id = event.get("comment_id").and_then(Value::as_i64);
            let event_id = event.get("event_id").and_then(Value::as_str);
            let matches_projected_endpoint = comment_id == metadata.genesis_comment_id
                || comment_id == metadata.head_comment_id
                || event_id == Some(metadata.event_id.as_str())
                || event_id == metadata.head_event_id.as_deref();
            let matches_structured_interior = match (
                comment_id,
                event_id,
                metadata.genesis_comment_id,
                metadata.head_comment_id,
            ) {
                (Some(comment_id), Some(_), Some(genesis_id), Some(head_id)) => {
                    comment_id > genesis_id.min(head_id) && comment_id < genesis_id.max(head_id)
                }
                _ => false,
            };
            (is_deletion && (matches_projected_endpoint || matches_structured_interior)).then(
                || CorrelatedDeletion {
                    comment_id,
                    event_id: event_id.map(str::to_owned),
                    github_actor: event
                        .get("actor")
                        .and_then(|actor| actor.get("login"))
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                },
            )
        })
    }

    fn replay_failure_report(
        &self,
        issue: &LedgerIssue,
        metadata: &ProjectionMetadata,
        comments: &[LedgerComment],
        error: &anyhow::Error,
        timeline_evidence: Vec<Value>,
    ) -> IntegrityDoctorReport {
        let deletion = Self::correlated_deletion(metadata, &timeline_evidence);
        let first_unreadable_comment = comments
            .iter()
            .filter(|comment| is_projected_or_structured_event(metadata, comment))
            .find(|comment| parse_event(&comment.body).is_err());
        let is_unknown_schema = error
            .downcast_ref::<GitHubError>()
            .is_some_and(|error| error.kind() == GitHubErrorKind::UnknownEventSchema);
        IntegrityDoctorReport {
            work_item_id: issue.number,
            integrity_health: IntegrityHealth::LedgerIntegrityError,
            archived: issue.locked,
            first_break: Some(IntegrityBreak {
                kind: if deletion.is_some() {
                    IntegrityBreakKind::DeletedEvent
                } else if is_unknown_schema {
                    IntegrityBreakKind::UnknownSchemaVersion
                } else {
                    IntegrityBreakKind::UnreadableEvent
                },
                github_comment_id: deletion
                    .as_ref()
                    .and_then(|deletion| deletion.comment_id)
                    .or_else(|| first_unreadable_comment.map(|comment| comment.id)),
                event_id: deletion
                    .as_ref()
                    .and_then(|deletion| deletion.event_id.clone()),
                github_actor: deletion
                    .as_ref()
                    .and_then(|deletion| deletion.github_actor.clone())
                    .or_else(|| first_unreadable_comment.map(|comment| comment.user.login.clone())),
                expected_hash: metadata.history_hash.clone(),
                observed_hash: None,
                cached_exact_copy: None,
                observed_copy: first_unreadable_comment.map(|comment| comment.body.clone()),
                detail: format!("{error:#}"),
            }),
            trusted_event_count: 0,
            untrusted_event_count: comments
                .iter()
                .filter(|comment| is_projected_or_structured_event(metadata, comment))
                .count(),
            timeline_evidence,
            eligible_repair_modes: vec![RepairMode::Rebaseline],
            observed_evidence: first_unreadable_comment
                .map(observed_integrity_evidence)
                .into_iter()
                .collect(),
        }
    }

    fn head_failure_report(
        &self,
        issue: &LedgerIssue,
        metadata: &ProjectionMetadata,
        history: &[HistoryEntry],
        error: &anyhow::Error,
        timeline_evidence: Vec<Value>,
    ) -> IntegrityDoctorReport {
        let deletion = Self::correlated_deletion(metadata, &timeline_evidence);
        let projected = metadata.head_comment_id.and_then(|comment_id| {
            history.iter().find(|entry| {
                entry.id == comment_id
                    && entry.event_id.as_deref() == metadata.head_event_id.as_deref()
            })
        });
        let kind = if deletion.is_some() {
            IntegrityBreakKind::DeletedEvent
        } else if projected.is_some() {
            IntegrityBreakKind::BrokenHashContinuity
        } else {
            IntegrityBreakKind::HeadMismatch
        };
        let observed = projected.or_else(|| history.last());
        IntegrityDoctorReport {
            work_item_id: issue.number,
            integrity_health: IntegrityHealth::LedgerIntegrityError,
            archived: issue.locked,
            first_break: Some(IntegrityBreak {
                kind,
                github_comment_id: deletion
                    .as_ref()
                    .and_then(|deletion| deletion.comment_id)
                    .or_else(|| observed.map(|entry| entry.id)),
                event_id: deletion
                    .as_ref()
                    .and_then(|deletion| deletion.event_id.clone())
                    .or_else(|| observed.and_then(|entry| entry.event_id.clone())),
                github_actor: deletion
                    .as_ref()
                    .and_then(|deletion| deletion.github_actor.clone())
                    .or_else(|| observed.and_then(|entry| entry.github_actor.clone())),
                expected_hash: metadata.history_hash.clone(),
                observed_hash: observed.and_then(|entry| entry.history_hash.clone()),
                cached_exact_copy: None,
                observed_copy: None,
                detail: format!("{error:#}"),
            }),
            trusted_event_count: 0,
            untrusted_event_count: history.len(),
            timeline_evidence,
            eligible_repair_modes: vec![RepairMode::Rebaseline],
            observed_evidence: Vec::new(),
        }
    }

    fn project_history_head(
        &self,
        issue: &LedgerIssue,
        mut metadata: ProjectionMetadata,
        history: &[HistoryEntry],
    ) -> Result<()> {
        let head = history
            .last()
            .context("Work Tracker history has no genesis entry")?;
        metadata.head_event_id = head.event_id.clone();
        metadata.head_comment_id = Some(head.id);
        metadata.history_hash = head.history_hash.clone();
        metadata.state_revision = head
            .state_revision
            .context("Work Tracker history head omitted State Revision")?;
        let item = materialize_item(issue.number, history)?;
        let body = projection_body(item.description.as_deref(), &metadata)?;
        let status_label = format!("work-tracker:status:{}", item.status.as_str());
        let mut projected_labels = issue
            .labels
            .iter()
            .filter(|label| !is_status_label(&label.name))
            .map(|label| label.name.clone())
            .collect::<Vec<_>>();
        projected_labels.push(status_label);
        let state = if item.status.is_actionable() {
            "open"
        } else {
            "closed"
        };
        let mut fields = vec![("title", item.title.as_str())];
        for label in &projected_labels {
            fields.push(("labels[]", label.as_str()));
        }
        fields.push(("state", state));
        if !item.status.is_actionable() {
            fields.push((
                "state_reason",
                if item.status == Status::Done {
                    "completed"
                } else {
                    "not_planned"
                },
            ));
        }
        fields.push(("body", body.as_str()));
        self.github.api_empty(
            "PATCH",
            &format!("repos/{}/issues/{}", self.repository, issue.number),
            &fields,
        )?;
        self.project_lock_state(issue, item.status)
    }

    fn project_lock_state(&self, issue: &LedgerIssue, status: Status) -> Result<()> {
        let should_be_locked = status == Status::Archived;
        if issue.locked == should_be_locked {
            return Ok(());
        }
        self.github.api_empty(
            if should_be_locked { "PUT" } else { "DELETE" },
            &format!("repos/{}/issues/{}/lock", self.repository, issue.number),
            &[],
        )
    }

    fn load_prepared_mutation(&self, issue_number: i64) -> Result<PreparedMutation> {
        let (issue, metadata) = self.load_work_item_issue(issue_number)?;
        let comments = self.load_comments(issue_number)?;
        let replayed = replay_trusted_history(issue_number, &comments)?;
        let legacy_genesis = self.cache.legacy_github_genesis_evidence(issue_number)?;
        let projection_needs_update = projection_head_needs_update(
            issue_number,
            &metadata,
            &replayed.accepted,
            legacy_genesis.as_ref(),
        )?;
        let current = materialize_item(issue_number, &replayed.accepted)?;
        let evidence = event_evidence(&comments, &replayed.accepted);
        Ok(PreparedMutation {
            issue,
            metadata,
            replayed,
            current,
            projection_needs_update,
            evidence,
        })
    }

    fn prepare_mutation(&self, issue_number: i64) -> Result<PreparedMutation> {
        let prepared = self.load_prepared_mutation(issue_number)?;
        if prepared.current.status == Status::Archived {
            bail!("work item {issue_number} is archived and cannot be modified");
        }
        Ok(prepared)
    }

    fn finish_noop_mutation(&mut self, prepared: PreparedMutation) -> Result<WorkItem> {
        let PreparedMutation {
            issue,
            metadata,
            replayed,
            current,
            projection_needs_update,
            evidence,
        } = prepared;
        let repaired_projection = projection_differs(&issue, &current, projection_needs_update)?;
        if repaired_projection {
            self.project_history_head(&issue, metadata, &replayed.accepted)?;
            self.repaired_work_item_ids.push(issue.number);
        }
        self.cache.replace_github_item(&GithubCacheItem {
            item: current.clone(),
            history: replayed.cache_history(),
            rejected: replayed.rejected,
            evidence,
        })?;
        Ok(current)
    }

    fn submit_mutation(
        &mut self,
        issue_number: i64,
        prepared: PreparedMutation,
        event: CanonicalEvent,
    ) -> Result<WorkItem> {
        let operation = event
            .kind
            .operation_name()
            .context("non-mutation event passed to mutation workflow")?;
        let event_id = event.event_id.clone();
        let repairs_preexisting_drift = projection_differs(
            &prepared.issue,
            &prepared.current,
            prepared.projection_needs_update,
        )?;
        let expected_state_revision = event
            .expected_state_revision
            .context("mutation proposal omitted expected State Revision")?;
        let body = event_comment_body(&event)?;
        let comment: LedgerComment = self.github.api_json(
            "POST",
            &format!("repos/{}/issues/{issue_number}/comments", self.repository),
            &[("body", body.as_str())],
        )?;
        if comment.user.login != event.github_actor {
            return Err(metadata_collision(issue_number).into());
        }
        let comments = self.load_comments(issue_number)?;
        let replayed = replay_trusted_history(issue_number, &comments)?;
        let history = replayed.accepted.clone();
        let entry = history
            .iter()
            .find(|entry| entry.event_id.as_deref() == Some(event_id.as_str()))
            .cloned();
        let proposal_confirmed = comments.iter().any(|comment| {
            has_metadata(&comment.body, EVENT_MARKER)
                && parse_event(&comment.body).is_ok_and(|candidate| candidate == event)
                && comment.user.login == event.github_actor
        });
        if !proposal_confirmed {
            bail!("published {operation} was not confirmed from GitHub");
        }
        let item = materialize_item(issue_number, &history)?;
        let evidence = event_evidence(&comments, &history);
        self.cache.replace_github_item(&GithubCacheItem {
            item: item.clone(),
            history: replayed.cache_history(),
            rejected: replayed.rejected,
            evidence,
        })?;
        let projection_error = self
            .project_history_head(&prepared.issue, prepared.metadata, &history)
            .err();
        if projection_error.is_none() && repairs_preexisting_drift {
            self.repaired_work_item_ids.push(issue_number);
        }
        if let Some(entry) = entry {
            validate_retry(&entry, &event)?;
            if let Some(error) = projection_error.as_ref() {
                return Err(GitHubError::new(
                    GitHubErrorKind::ProjectionPending,
                    format!(
                        "{operation} {event_id} is accepted and effective at State Revision {}; current values are title={:?}, description={:?}, Status={}; the readable GitHub projection still needs repair: {error:#}",
                        entry.state_revision.unwrap_or(expected_state_revision + 1),
                        item.title,
                        item.description,
                        item.status,
                    ),
                )
                .with_details(json!({
                    "event_id": event_id,
                    "accepted": true,
                    "effective": true,
                    "current_state_revision": entry.state_revision,
                    "current_values": {
                        "title": item.title,
                        "description": item.description,
                        "status": item.status,
                    },
                    "projection_pending": true,
                    "instruction": "retry after refreshing; Work Tracker will repair the projection without duplicating the effective History Entry",
                }))
                .into());
            }
            return Ok(item);
        }
        let current_revision = history
            .last()
            .and_then(|entry| entry.state_revision)
            .context("Work Tracker history head omitted State Revision")?;
        Err(GitHubError::new(
            GitHubErrorKind::RejectedMutation,
            format!(
                "Rejected Mutation for work item {issue_number}: expected State Revision {expected_state_revision}, current State Revision is {current_revision}; current values are title={:?}, description={:?}, Status={}; refresh current values and retry only by submitting a new proposal",
                item.title, item.description, item.status,
            ),
        )
        .with_details(json!({
            "kind": "rejected_mutation",
            "event_id": event_id,
            "expected_state_revision": expected_state_revision,
            "current_state_revision": current_revision,
            "current_values": {
                "title": item.title,
                "description": item.description,
                "status": item.status,
            },
            "instruction": "refresh current values, decide whether the change is still appropriate, and retry with a new proposal",
            "projection_pending": projection_error.is_some(),
        }))
        .into())
    }

    fn transition_status(
        &mut self,
        issue_number: i64,
        status: Status,
        actor: &str,
        note: Option<&str>,
        requested_event_id: Option<&str>,
    ) -> Result<WorkItem> {
        self.ensure_integrity_allows_mutation(issue_number)?;
        let actor = normalized_required(actor, "actor")?;
        let note = normalized_optional(note);
        let requested_event_id = requested_event_id
            .map(|value| normalized_required(value, "event ID"))
            .transpose()?;
        let github_actor = self.github.authenticated_user()?;
        let prepared = if status == Status::Archived {
            self.load_prepared_mutation(issue_number)?
        } else {
            self.prepare_mutation(issue_number)?
        };
        if let Some(event_id) = requested_event_id.as_deref()
            && let Some(existing) = prepared
                .replayed
                .accepted
                .iter()
                .find(|entry| entry.event_id.as_deref() == Some(event_id))
        {
            if existing.kind
                != if status == Status::Archived {
                    EventKind::Archived.as_str()
                } else {
                    EventKind::StatusChanged.as_str()
                }
                || existing.actor != actor
                || existing.github_actor.as_deref() != Some(github_actor.as_str())
                || existing.note != note
                || !retry_status_matches(&existing.changes, status)
            {
                return Err(GitHubError::new(
                    GitHubErrorKind::MetadataCollision,
                    format!("event ID {event_id} was already used for different content"),
                )
                .into());
            }
            return self.finish_noop_mutation(prepared);
        }
        if let Some(event_id) = requested_event_id.as_deref()
            && let Some(existing) = prepared
                .replayed
                .rejected
                .iter()
                .find(|mutation| mutation.event_id == event_id)
                .cloned()
        {
            if existing.actor != actor
                || existing.github_actor != github_actor
                || existing.note != note
                || !retry_status_matches(&existing.changes, status)
            {
                return Err(GitHubError::new(
                    GitHubErrorKind::MetadataCollision,
                    format!("event ID {event_id} was already used for different content"),
                )
                .into());
            }
            let current = self.finish_noop_mutation(prepared)?;
            return Err(retried_rejected_mutation_error(&existing, &current).into());
        }
        if prepared.current.status == Status::Archived {
            let repaired = projection_differs(
                &prepared.issue,
                &prepared.current,
                prepared.projection_needs_update,
            )?;
            if repaired {
                self.finish_noop_mutation(prepared)?;
            }
            return Err(GitHubError::new(
                GitHubErrorKind::ArchivedImmutable,
                if repaired {
                    format!(
                        "work item {issue_number} is archived and cannot be modified; its GitHub projection drift was repaired from accepted history"
                    )
                } else {
                    format!("work item {issue_number} is archived and cannot be modified")
                },
            )
            .with_details(json!({
                "work_item_id": issue_number,
                "projection_repaired": repaired,
            }))
            .into());
        }
        if prepared.current.status == status {
            return self.finish_noop_mutation(prepared);
        }

        let event_id = requested_event_id.unwrap_or_else(|| {
            new_event_id(if status == Status::Archived {
                "archive"
            } else {
                "status"
            })
        });
        let expected_state_revision = prepared
            .replayed
            .accepted
            .last()
            .and_then(|entry| entry.state_revision)
            .context("Work Tracker history head omitted State Revision")?;
        let event = CanonicalEvent {
            schema_version: 1,
            event_id: event_id.clone(),
            kind: if status == Status::Archived {
                EventKind::Archived
            } else {
                EventKind::StatusChanged
            },
            actor,
            github_actor,
            note,
            changes: serde_json::to_value(StatusChanges {
                status: FieldChange {
                    from: prepared.current.status,
                    to: status,
                },
            })?,
            expected_state_revision: Some(expected_state_revision),
        };
        self.submit_mutation(issue_number, prepared, event)
    }

    fn update_fields(
        &mut self,
        issue_number: i64,
        title: Option<&str>,
        description: Option<Option<&str>>,
        actor: &str,
        note: Option<&str>,
        requested_event_id: Option<&str>,
    ) -> Result<WorkItem> {
        self.ensure_integrity_allows_mutation(issue_number)?;
        let actor = normalized_required(actor, "actor")?;
        let note = normalized_optional(note);
        let requested_event_id = requested_event_id
            .map(|value| normalized_required(value, "event ID"))
            .transpose()?;
        let requested_title = title
            .map(|value| normalized_required(value, "title"))
            .transpose()?;
        let requested_description = description.map(normalized_optional);
        let github_actor = self.github.authenticated_user()?;
        let prepared = self.prepare_mutation(issue_number)?;
        if let Some(event_id) = requested_event_id.as_deref()
            && let Some(existing) = prepared
                .replayed
                .accepted
                .iter()
                .find(|entry| entry.event_id.as_deref() == Some(event_id))
        {
            if existing.kind != EventKind::Updated.as_str()
                || existing.actor != actor
                || existing.github_actor.as_deref() != Some(github_actor.as_str())
                || existing.note != note
                || !retry_fields_match(
                    &existing.changes,
                    requested_title.as_ref(),
                    requested_description.as_ref(),
                )
            {
                return Err(GitHubError::new(
                    GitHubErrorKind::MetadataCollision,
                    format!("event ID {event_id} was already used for different content"),
                )
                .into());
            }
            return self.finish_noop_mutation(prepared);
        }
        if let Some(event_id) = requested_event_id.as_deref()
            && let Some(existing) = prepared
                .replayed
                .rejected
                .iter()
                .find(|mutation| mutation.event_id == event_id)
                .cloned()
        {
            if existing.actor != actor
                || existing.github_actor != github_actor
                || existing.note != note
                || !retry_fields_match(
                    &existing.changes,
                    requested_title.as_ref(),
                    requested_description.as_ref(),
                )
            {
                return Err(GitHubError::new(
                    GitHubErrorKind::MetadataCollision,
                    format!("event ID {event_id} was already used for different content"),
                )
                .into());
            }
            let current = self.finish_noop_mutation(prepared)?;
            return Err(retried_rejected_mutation_error(&existing, &current).into());
        }
        let new_title = match requested_title {
            Some(value) => value,
            None => prepared.current.title.clone(),
        };
        let new_description = match requested_description {
            Some(value) => value,
            None => prepared.current.description.clone(),
        };
        let mut changes = FieldChanges::default();
        if new_title != prepared.current.title {
            changes.title = Some(FieldChange {
                from: prepared.current.title.clone(),
                to: new_title,
            });
        }
        if new_description != prepared.current.description {
            changes.description = Some(FieldChange {
                from: prepared.current.description.clone(),
                to: new_description,
            });
        }
        if changes.is_empty() {
            return self.finish_noop_mutation(prepared);
        }

        let event_id = requested_event_id.unwrap_or_else(|| new_event_id("update"));
        let expected_state_revision = prepared
            .replayed
            .accepted
            .last()
            .and_then(|entry| entry.state_revision)
            .context("Work Tracker history head omitted State Revision")?;
        let event = CanonicalEvent {
            schema_version: 1,
            event_id: event_id.clone(),
            kind: EventKind::Updated,
            actor,
            github_actor,
            note,
            changes: serde_json::to_value(changes)?,
            expected_state_revision: Some(expected_state_revision),
        };
        self.submit_mutation(issue_number, prepared, event)
    }

    fn append_note(
        &mut self,
        issue_number: i64,
        message: &str,
        actor: &str,
        requested_event_id: Option<&str>,
    ) -> Result<HistoryEntry> {
        self.ensure_integrity_allows_mutation(issue_number)?;
        let message = normalized_required(message, "message")?;
        let actor = normalized_required(actor, "actor")?;
        let event_id = requested_event_id
            .map(|value| normalized_required(value, "event ID"))
            .transpose()?
            .unwrap_or_else(|| new_event_id("note"));
        let github_actor = self.github.authenticated_user()?;
        let (issue, metadata) = self.load_work_item_issue(issue_number)?;
        let comments = self.load_comments(issue_number)?;
        let replayed = replay_trusted_history(issue_number, &comments)?;
        let history = &replayed.accepted;
        let legacy_genesis = self.cache.legacy_github_genesis_evidence(issue_number)?;
        let projection_needs_update = projection_head_needs_update(
            issue_number,
            &metadata,
            history,
            legacy_genesis.as_ref(),
        )?;
        if materialize_item(issue_number, history)?.status == Status::Archived {
            bail!("work item {issue_number} is archived and cannot be modified");
        }
        let repairs_preexisting_drift = projection_differs(
            &issue,
            &materialize_item(issue_number, history)?,
            projection_needs_update,
        )?;
        let event = CanonicalEvent {
            schema_version: 1,
            event_id: event_id.clone(),
            kind: EventKind::Noted,
            actor,
            github_actor,
            note: Some(message),
            changes: json!({}),
            expected_state_revision: None,
        };
        if let Some(existing) = history
            .iter()
            .find(|entry| entry.event_id.as_deref() == Some(event_id.as_str()))
            .cloned()
        {
            validate_retry(&existing, &event)?;
            self.project_history_head(&issue, metadata, history)?;
            if repairs_preexisting_drift {
                self.repaired_work_item_ids.push(issue_number);
            }
            let item = materialize_item(issue_number, history)?;
            let evidence = event_evidence(&comments, history);
            self.cache.replace_github_item(&GithubCacheItem {
                item,
                history: replayed.cache_history(),
                rejected: replayed.rejected,
                evidence,
            })?;
            return Ok(existing);
        }

        let body = event_comment_body(&event)?;
        let comment: LedgerComment = self.github.api_json(
            "POST",
            &format!("repos/{}/issues/{issue_number}/comments", self.repository),
            &[("body", body.as_str())],
        )?;
        if comment.user.login != event.github_actor {
            return Err(metadata_collision(issue_number).into());
        }
        let comments = self.load_comments(issue_number)?;
        let replayed = replay_trusted_history(issue_number, &comments)?;
        let history = &replayed.accepted;
        projection_head_needs_update(issue_number, &metadata, history, legacy_genesis.as_ref())?;
        let entry = history
            .iter()
            .find(|entry| entry.event_id.as_deref() == Some(event_id.as_str()))
            .cloned()
            .context("published note was not reconstructed from GitHub")?;
        self.project_history_head(&issue, metadata, history)?;
        if repairs_preexisting_drift {
            self.repaired_work_item_ids.push(issue_number);
        }
        let item = materialize_item(issue_number, history)?;
        let evidence = event_evidence(&comments, history);
        self.cache.replace_github_item(&GithubCacheItem {
            item,
            history: replayed.cache_history(),
            rejected: replayed.rejected,
            evidence,
        })?;
        Ok(entry)
    }

    fn diagnose_loaded(
        &self,
        issue: &LedgerIssue,
        metadata: &ProjectionMetadata,
        comments: &[LedgerComment],
        timeline: Vec<Value>,
    ) -> Result<IntegrityDoctorReport> {
        if let Some(report) = self.cached_integrity_report(issue, comments, timeline.clone())? {
            return Ok(report);
        }
        let replayed = match replay_trusted_history(issue.number, comments) {
            Ok(replayed) => replayed,
            Err(error) => {
                return Ok(self.replay_failure_report(issue, metadata, comments, &error, timeline));
            }
        };
        match projection_head_needs_update(issue.number, metadata, &replayed.accepted, None) {
            Ok(_) => {
                if let Some(mut latched) = self.cache.github_integrity_report(issue.number)? {
                    latched.archived = issue.locked;
                    latched.timeline_evidence = timeline;
                    return Ok(latched);
                }
                Ok(IntegrityDoctorReport {
                    work_item_id: issue.number,
                    integrity_health: IntegrityHealth::Healthy,
                    archived: issue.locked,
                    first_break: None,
                    trusted_event_count: replayed.accepted.len(),
                    untrusted_event_count: 0,
                    timeline_evidence: timeline,
                    eligible_repair_modes: Vec::new(),
                    observed_evidence: Vec::new(),
                })
            }
            Err(error) => {
                Ok(self.head_failure_report(issue, metadata, &replayed.accepted, &error, timeline))
            }
        }
    }

    fn latch_recovery_race(
        &mut self,
        issue: &LedgerIssue,
        comments: &[LedgerComment],
        diagnosis: &IntegrityDoctorReport,
        before: &[ObservedIntegrityEvidence],
        after: &[ObservedIntegrityEvidence],
        additional: &[ObservedIntegrityEvidence],
    ) -> Result<()> {
        self.preserve_integrity_snapshot(issue, comments, diagnosis, None)?;
        let mut latched = self
            .cache
            .github_integrity_report(diagnosis.work_item_id)?
            .unwrap_or_else(|| diagnosis.clone());
        for observed in after {
            let was_already_observed = before.iter().any(|previous| {
                previous.github_comment_id == observed.github_comment_id
                    && previous.github_actor == observed.github_actor
                    && previous.body == observed.body
            });
            let is_already_latched = latched.observed_evidence.iter().any(|previous| {
                previous.github_comment_id == observed.github_comment_id
                    && previous.github_actor == observed.github_actor
                    && previous.body == observed.body
            });
            if !was_already_observed && !is_already_latched {
                latched.observed_evidence.push(observed.clone());
            }
        }
        for observed in additional {
            let is_already_latched = latched.observed_evidence.iter().any(|previous| {
                previous.github_comment_id == observed.github_comment_id
                    && previous.github_actor == observed.github_actor
                    && previous.body == observed.body
            });
            if !is_already_latched {
                latched.observed_evidence.push(observed.clone());
            }
        }
        latched.untrusted_event_count = latched
            .untrusted_event_count
            .max(latched.observed_evidence.len());
        self.cache.record_github_integrity(&latched)
    }

    fn load_rebaseline_validation_snapshot(
        &self,
        issue_number: i64,
        tracked_comment_ids: &HashSet<i64>,
        anchor_id: i64,
    ) -> Result<RebaselineValidationSnapshot> {
        let (issue, metadata) = self.load_work_item_issue(issue_number)?;
        let comments = self.load_comments(issue_number)?;
        let reviewed = reviewed_item_from_issue(&issue, &comments).map_err(|error| {
            recovery_validation_failed(
                issue_number,
                format!("the live GitHub state became ambiguous: {error:#}"),
            )
        })?;
        let evidence =
            recovery_evidence_snapshot(&comments, &metadata, tracked_comment_ids, Some(anchor_id));
        Ok(RebaselineValidationSnapshot {
            issue,
            metadata,
            comments,
            reviewed,
            evidence,
        })
    }

    fn restore_exact_copy(&mut self, issue_number: i64) -> Result<RecoveryReport> {
        let (issue, metadata) = self.load_work_item_issue(issue_number)?;
        let comments = self.load_comments(issue_number)?;
        let diagnosis = self.diagnose_loaded(&issue, &metadata, &comments, Vec::new())?;
        let first_break = diagnosis.first_break.as_ref().ok_or_else(|| {
            recovery_still_blocked(
                issue_number,
                "the diagnosis does not identify a restorable event",
            )
        })?;
        if !diagnosis
            .eligible_repair_modes
            .contains(&RepairMode::RestoreExactCopy)
        {
            return Err(recovery_still_blocked(
                issue_number,
                "doctor did not verify an exact original copy for this break",
            )
            .into());
        }
        let comment_id = first_break.github_comment_id.ok_or_else(|| {
            recovery_still_blocked(
                issue_number,
                "the verified copy has no GitHub comment identity",
            )
        })?;
        let exact_body = first_break.cached_exact_copy.as_deref().ok_or_else(|| {
            recovery_still_blocked(issue_number, "the verified exact copy is unavailable")
        })?;
        let cached = self.cache.github_event_evidence(issue_number)?;
        let target_index = cached
            .iter()
            .position(|evidence| evidence.comment_id == comment_id)
            .ok_or_else(|| {
                recovery_still_blocked(issue_number, "the diagnosed event has no cached evidence")
            })?;
        if !cached_exact_chain_is_verified(&cached)
            || !cached_chain_matches_projection(&cached, &metadata)
            || cached[target_index].body != exact_body
        {
            return Err(recovery_still_blocked(
                issue_number,
                "the cached copy no longer verifies against the expected hash-chain evidence",
            )
            .into());
        }
        if !exact_chain_preserves_live_archive(&issue, &cached) {
            return Err(recovery_still_blocked(
                issue_number,
                "the verified history is active but the live GitHub issue is locked; exact recovery cannot discard the conservative archived state",
            )
            .into());
        }
        if !comments.iter().any(|comment| comment.id == comment_id) {
            return Err(recovery_still_blocked(
                issue_number,
                "the original GitHub comment was deleted and cannot be restored with its identity",
            )
            .into());
        }
        self.preserve_integrity_snapshot(&issue, &comments, &diagnosis, None)?;
        self.github.api_empty(
            "PATCH",
            &format!("repos/{}/issues/comments/{comment_id}", self.repository),
            &[("body", exact_body)],
        )?;

        let (reloaded_issue, metadata) = self.load_work_item_issue(issue_number)?;
        let reloaded_comments = self.load_comments(issue_number)?;
        if !exact_chain_preserves_live_archive(&reloaded_issue, &cached) {
            return Err(recovery_validation_failed(
                issue_number,
                "the live GitHub issue became locked while exact history was restored; recovery remains blocked",
            )
            .into());
        }
        if self
            .cached_integrity_report(&reloaded_issue, &reloaded_comments, Vec::new())?
            .is_some()
        {
            return Err(recovery_validation_failed(
                issue_number,
                "the restored event still differs from verified cached evidence",
            )
            .into());
        }
        let replayed =
            replay_trusted_history(issue_number, &reloaded_comments).map_err(|error| {
                recovery_validation_failed(
                    issue_number,
                    format!("the complete restored history did not replay: {error:#}"),
                )
            })?;
        projection_head_needs_update(issue_number, &metadata, &replayed.accepted, None).map_err(
            |error| {
                recovery_validation_failed(
                    issue_number,
                    format!(
                        "the complete restored history did not match its projection: {error:#}"
                    ),
                )
            },
        )?;
        let item = materialize_item(issue_number, &replayed.accepted).map_err(|error| {
            recovery_validation_failed(
                issue_number,
                format!("the restored current state could not be rebuilt: {error:#}"),
            )
        })?;
        self.project_history_head(&reloaded_issue, metadata, &replayed.accepted)?;
        let evidence = event_evidence(&reloaded_comments, &replayed.accepted);
        let trusted_event_count = replayed.accepted.len();
        self.cache.complete_github_recovery(&GithubCacheItem {
            item: item.clone(),
            history: replayed.cache_history(),
            rejected: replayed.rejected,
            evidence,
        })?;
        Ok(RecoveryReport {
            work_item_id: issue_number,
            outcome: RecoveryOutcome::ExactRestoration,
            archived: item.status == Status::Archived || issue.locked,
            full_history_revalidated: true,
            projection_rebuilt: true,
            trusted_event_count,
            untrusted_event_count: 0,
        })
    }

    fn rebaseline(
        &mut self,
        issue_number: i64,
        actor: Option<&str>,
        reason: Option<&str>,
    ) -> Result<RecoveryReport> {
        let actor = actor
            .ok_or_else(|| {
                recovery_still_blocked(issue_number, "Rebaseline requires an explicit --actor")
            })
            .and_then(|actor| {
                normalized_required(actor, "actor")
                    .map_err(|error| recovery_still_blocked(issue_number, error))
            })?;
        let reason = reason
            .ok_or_else(|| {
                recovery_still_blocked(
                    issue_number,
                    "Rebaseline requires an explicit non-empty --reason",
                )
            })
            .and_then(|reason| {
                normalized_required(reason, "reason")
                    .map_err(|error| recovery_still_blocked(issue_number, error))
            })?;
        let latched_diagnosis = self.cache.github_integrity_report(issue_number)?;
        let (issue, metadata) = self.load_work_item_issue(issue_number)?;
        let comments = self.load_comments(issue_number)?;
        let timeline = self.load_timeline(issue_number)?;
        let mut diagnosis = self.diagnose_loaded(&issue, &metadata, &comments, timeline)?;
        if !diagnosis
            .eligible_repair_modes
            .contains(&RepairMode::Rebaseline)
        {
            return Err(recovery_still_blocked(
                issue_number,
                "doctor did not make Rebaseline eligible",
            )
            .into());
        }
        let has_cached_item = self.cache.get(issue_number).is_ok();
        let reviewed = reviewed_item_from_issue(&issue, &comments).map_err(|error| {
            recovery_still_blocked(
                issue_number,
                format!("the live GitHub state cannot be reviewed safely: {error:#}"),
            )
        })?;
        diagnosis.archived = reviewed.status == Status::Archived;
        let cached_history = if has_cached_item {
            self.cache.history(issue_number)?
        } else {
            Vec::new()
        };
        let cached_evidence = self.cache.github_event_evidence(issue_number)?;
        let cached_comment_ids = cached_evidence
            .iter()
            .map(|evidence| evidence.comment_id)
            .collect::<HashSet<_>>();
        let initial_evidence =
            recovery_evidence_snapshot(&comments, &metadata, &cached_comment_ids, None);
        let mut tracked_comment_ids = cached_comment_ids.clone();
        tracked_comment_ids.extend(
            initial_evidence
                .iter()
                .map(|evidence| evidence.github_comment_id),
        );
        let mut prior_evidence = cached_evidence
            .into_iter()
            .map(|evidence| RetainedRecoveryEvidence {
                evidence_id: evidence.comment_id,
                github_comment_id: evidence.comment_id,
                variant: "cached_exact_copy".to_owned(),
                event_id: evidence.event_id,
                github_actor: evidence.github_actor,
                body: evidence.body,
                history_hash: evidence.history_hash,
                occurred_at: cached_history
                    .iter()
                    .find(|entry| entry.id == evidence.comment_id)
                    .map(|entry| entry.occurred_at),
            })
            .collect::<Vec<_>>();
        for comment in &comments {
            let matching = prior_evidence
                .iter()
                .find(|evidence| evidence.github_comment_id == comment.id);
            if matching.is_none() && !is_projected_or_structured_event(&metadata, comment) {
                continue;
            }
            if matching.is_some_and(|evidence| evidence.body == comment.body) {
                continue;
            }
            let parsed = parse_event(&comment.body).ok();
            prior_evidence.push(RetainedRecoveryEvidence {
                evidence_id: if matching.is_some() {
                    next_observed_evidence_id(&prior_evidence, comment.id)
                } else {
                    comment.id
                },
                github_comment_id: comment.id,
                variant: if matching.is_some() {
                    "observed_damaged_copy".to_owned()
                } else {
                    "observed_copy".to_owned()
                },
                event_id: parsed.as_ref().map(|event| event.event_id.clone()),
                github_actor: comment.user.login.clone(),
                body: comment.body.clone(),
                history_hash: None,
                occurred_at: Some(comment.created_at),
            });
        }
        if let Some(first_break) = latched_diagnosis
            .as_ref()
            .and_then(|diagnosis| diagnosis.first_break.as_ref())
            && let (Some(comment_id), Some(observed_copy)) = (
                first_break.github_comment_id,
                first_break.observed_copy.as_ref(),
            )
            && !prior_evidence.iter().any(|evidence| {
                evidence.github_comment_id == comment_id && evidence.body == *observed_copy
            })
        {
            let parsed = parse_event(observed_copy).ok();
            let github_actor = first_break
                .github_actor
                .clone()
                .or_else(|| {
                    prior_evidence
                        .iter()
                        .find(|evidence| evidence.github_comment_id == comment_id)
                        .map(|evidence| evidence.github_actor.clone())
                })
                .unwrap_or_else(|| "unknown".to_owned());
            let occurred_at = comments
                .iter()
                .find(|comment| comment.id == comment_id)
                .map(|comment| comment.created_at)
                .or_else(|| {
                    cached_history
                        .iter()
                        .find(|entry| entry.id == comment_id)
                        .map(|entry| entry.occurred_at)
                });
            prior_evidence.push(RetainedRecoveryEvidence {
                evidence_id: next_observed_evidence_id(&prior_evidence, comment_id),
                github_comment_id: comment_id,
                variant: "latched_observed_copy".to_owned(),
                event_id: parsed
                    .as_ref()
                    .map(|event| event.event_id.clone())
                    .or_else(|| first_break.event_id.clone()),
                github_actor,
                body: observed_copy.clone(),
                history_hash: first_break.observed_hash.clone(),
                occurred_at,
            });
        }
        if let Some(latched_diagnosis) = latched_diagnosis.as_ref() {
            for observed in &latched_diagnosis.observed_evidence {
                if prior_evidence.iter().any(|evidence| {
                    evidence.github_comment_id == observed.github_comment_id
                        && evidence.body == observed.body
                }) {
                    continue;
                }
                let parsed = parse_event(&observed.body).ok();
                prior_evidence.push(RetainedRecoveryEvidence {
                    evidence_id: next_observed_evidence_id(
                        &prior_evidence,
                        observed.github_comment_id,
                    ),
                    github_comment_id: observed.github_comment_id,
                    variant: "latched_observed_copy".to_owned(),
                    event_id: parsed.as_ref().map(|event| event.event_id.clone()),
                    github_actor: observed.github_actor.clone(),
                    body: observed.body.clone(),
                    history_hash: None,
                    occurred_at: Some(observed.observed_at),
                });
            }
        }
        prior_evidence.sort_by_key(|evidence| (evidence.github_comment_id, evidence.evidence_id));

        let github_actor = self.github.authenticated_user()?;
        let event = CanonicalEvent {
            schema_version: 1,
            event_id: new_event_id("rebaseline"),
            kind: EventKind::Rebaseline,
            actor,
            github_actor,
            note: Some(reason),
            changes: serde_json::to_value(RebaselineChanges {
                title: reviewed.title.clone(),
                description: reviewed.description.clone(),
                status: reviewed.status,
                integrity_context: diagnosis.clone(),
                prior_evidence,
            })?,
            expected_state_revision: None,
        };
        let body = event_comment_body(&event)?;
        let archived = reviewed.status == Status::Archived;

        let recovery = (|| -> Result<(WorkItem, ReplayedHistory, Vec<LedgerComment>)> {
            let published_anchor: LedgerComment = self.github.api_json(
                "POST",
                &format!("repos/{}/issues/{issue_number}/comments", self.repository),
                &[("body", body.as_str())],
            )?;
            if published_anchor.user.login != event.github_actor {
                return Err(metadata_collision(issue_number).into());
            }
            let anchor_id = published_anchor.id;
            let published_anchor_evidence = ObservedIntegrityEvidence {
                github_comment_id: anchor_id,
                github_actor: event.github_actor.clone(),
                body: body.clone(),
                observed_at: published_anchor.created_at,
            };

            let reloaded = self.load_rebaseline_validation_snapshot(
                issue_number,
                &tracked_comment_ids,
                anchor_id,
            )?;
            if !rebaseline_snapshot_matches(
                &issue,
                &metadata,
                &reviewed,
                &initial_evidence,
                &reloaded,
            ) {
                let anchor_evidence =
                    rebaseline_anchor_evidence(&published_anchor_evidence, &reloaded.comments);
                self.latch_recovery_race(
                    &reloaded.issue,
                    &reloaded.comments,
                    &diagnosis,
                    &initial_evidence,
                    &reloaded.evidence,
                    &anchor_evidence,
                )?;
                return Err(recovery_validation_failed(
                    issue_number,
                    "the live GitHub state or integrity evidence changed while the Rebaseline anchor was published; recovery remains blocked and a retry must retain the new evidence",
                )
                .into());
            }
            if !rebaseline_anchor_is_exact(
                &reloaded.comments,
                anchor_id,
                &event.github_actor,
                &body,
            ) {
                let anchor_evidence =
                    rebaseline_anchor_evidence(&published_anchor_evidence, &reloaded.comments);
                self.latch_recovery_race(
                    &reloaded.issue,
                    &reloaded.comments,
                    &diagnosis,
                    &initial_evidence,
                    &reloaded.evidence,
                    &anchor_evidence,
                )?;
                return Err(recovery_validation_failed(
                    issue_number,
                    "the published Rebaseline anchor was absent or changed before validation",
                )
                .into());
            }
            let final_snapshot = self.load_rebaseline_validation_snapshot(
                issue_number,
                &tracked_comment_ids,
                anchor_id,
            )?;
            if !rebaseline_snapshot_matches(
                &reloaded.issue,
                &reloaded.metadata,
                &reloaded.reviewed,
                &reloaded.evidence,
                &final_snapshot,
            ) || !rebaseline_anchor_is_exact(
                &final_snapshot.comments,
                anchor_id,
                &event.github_actor,
                &body,
            ) {
                let anchor_evidence = rebaseline_anchor_evidence(
                    &published_anchor_evidence,
                    &final_snapshot.comments,
                );
                self.latch_recovery_race(
                    &final_snapshot.issue,
                    &final_snapshot.comments,
                    &diagnosis,
                    &initial_evidence,
                    &final_snapshot.evidence,
                    &anchor_evidence,
                )?;
                return Err(recovery_validation_failed(
                    issue_number,
                    "the live GitHub state, lock, anchor, or integrity evidence changed during final Rebaseline validation; recovery remains blocked",
                )
                .into());
            }
            let replayed = replay_trusted_history(issue_number, &final_snapshot.comments).map_err(
                |error| {
                    recovery_validation_failed(
                        issue_number,
                        format!("the Rebaseline sequence did not replay: {error:#}"),
                    )
                },
            )?;
            let anchor = replayed.accepted.first().ok_or_else(|| {
                recovery_validation_failed(
                    issue_number,
                    "the Rebaseline anchor was not reconstructed",
                )
            })?;
            if anchor.id != anchor_id
                || anchor.kind != EventKind::Rebaseline.as_str()
                || anchor.previous_history_hash.is_some()
            {
                return Err(recovery_validation_failed(
                    issue_number,
                    "the Rebaseline did not start a new valid hash sequence",
                )
                .into());
            }
            let mut projected_metadata = final_snapshot.metadata;
            projected_metadata.event_id = event.event_id.clone();
            projected_metadata.pending_genesis_event_id = None;
            projected_metadata.pending_genesis_event = None;
            projected_metadata.genesis_comment_id = Some(anchor_id);
            projected_metadata.state_revision = 1;
            projected_metadata.head_event_id = None;
            projected_metadata.head_comment_id = None;
            projected_metadata.history_hash = None;
            let item = materialize_item(issue_number, &replayed.accepted).map_err(|error| {
                recovery_validation_failed(
                    issue_number,
                    format!("the reviewed current state could not be rebuilt: {error:#}"),
                )
            })?;
            self.project_history_head(
                &final_snapshot.issue,
                projected_metadata,
                &replayed.accepted,
            )?;
            Ok((item, replayed, final_snapshot.comments))
        })();
        let (item, replayed, comments) = recovery?;
        let trusted_event_count = replayed.accepted.len();
        let untrusted_event_count = replayed.untrusted.len();
        let evidence = event_evidence(&comments, &replayed.accepted);
        self.cache.complete_github_recovery(&GithubCacheItem {
            item,
            history: replayed.cache_history(),
            rejected: replayed.rejected,
            evidence,
        })?;
        Ok(RecoveryReport {
            work_item_id: issue_number,
            outcome: RecoveryOutcome::Rebaseline,
            archived,
            full_history_revalidated: false,
            projection_rebuilt: true,
            trusted_event_count,
            untrusted_event_count,
        })
    }
}

impl Ledger for GitHubLedger {
    fn prepare_read(&mut self, policy: ReadPolicy) -> Result<ReadHealth> {
        let repository = self.repository.to_string();
        let last_successful_sync_at = self.cache.github_last_successful_sync_at(&repository)?;
        if policy == ReadPolicy::Offline {
            if let Some(report) = self.cache.first_github_integrity_report()? {
                let first_break = report.first_break.as_ref();
                return Ok(ReadHealth::Integrity {
                    last_successful_sync_at,
                    reason: first_break.map_or_else(
                        || {
                            format!(
                                "Ledger Integrity Error for GitHub issue #{}",
                                report.work_item_id
                            )
                        },
                        |first_break| {
                            format!(
                                "Ledger Integrity Error for GitHub issue #{}: {}",
                                report.work_item_id, first_break.detail
                            )
                        },
                    ),
                    error_kind: if first_break.is_some_and(|first_break| {
                        first_break.kind == IntegrityBreakKind::UnknownSchemaVersion
                    }) {
                        ReadHealthErrorKind::UnknownEventSchema
                    } else {
                        ReadHealthErrorKind::LedgerIntegrity
                    },
                });
            }
            return Ok(match last_successful_sync_at {
                Some(last_successful_sync_at) => ReadHealth::Stale {
                    last_successful_sync_at,
                    reason: "GitHub synchronization was intentionally skipped (--offline)"
                        .to_owned(),
                    offline: true,
                },
                None => ReadHealth::Unavailable {
                    reason: "the cache has never completed a successful GitHub synchronization"
                        .to_owned(),
                },
            });
        }

        match self.synchronize() {
            Ok((synchronized_at, repaired_work_item_ids)) => {
                if let Some(report) = self.cache.first_github_integrity_report()? {
                    return Ok(ReadHealth::Integrity {
                        last_successful_sync_at: Some(synchronized_at),
                        reason: format!(
                            "Ledger Integrity Error for GitHub issue #{} remains cached pending explicit repair",
                            report.work_item_id
                        ),
                        error_kind: ReadHealthErrorKind::LedgerIntegrity,
                    });
                }
                Ok(ReadHealth::Fresh {
                    synchronized_at,
                    repaired_work_item_ids,
                })
            }
            Err(error) if policy == ReadPolicy::Fresh => Err(error),
            Err(error) => {
                let github_error = error.downcast_ref::<GitHubError>();
                let reason = format!("{error:#}");
                if let Some(error_kind) =
                    github_error.and_then(|error| error.kind().read_health_error_kind())
                {
                    return Ok(ReadHealth::Integrity {
                        last_successful_sync_at,
                        reason,
                        error_kind,
                    });
                }
                if github_error.is_some_and(GitHubError::is_availability_failure) {
                    return Ok(match last_successful_sync_at {
                        Some(last_successful_sync_at) => ReadHealth::Stale {
                            last_successful_sync_at,
                            reason,
                            offline: false,
                        },
                        None => ReadHealth::Unavailable { reason },
                    });
                }
                Err(error)
            }
        }
    }

    fn create(
        &mut self,
        title: &str,
        description: Option<&str>,
        status: Status,
        actor: &str,
        note: Option<&str>,
    ) -> Result<WorkItem> {
        self.create_work_item(title, description, status, actor, note, None)
    }

    fn create_with_event_id(
        &mut self,
        title: &str,
        description: Option<&str>,
        status: Status,
        actor: &str,
        note: Option<&str>,
        event_id: Option<&str>,
    ) -> Result<WorkItem> {
        self.create_work_item(title, description, status, actor, note, event_id)
    }

    fn get(&self, id: i64) -> Result<WorkItem> {
        let mut item = self.cache.get(id)?;
        self.decorate_integrity(std::slice::from_mut(&mut item))?;
        Ok(item)
    }

    fn list(
        &mut self,
        filter: ListFilter,
        include_archived: bool,
        limit: usize,
    ) -> Result<Vec<WorkItem>> {
        let mut items = self.cache.list(filter, include_archived, limit)?;
        self.decorate_integrity(&mut items)?;
        Ok(items)
    }

    fn daily_view(&mut self, include_archived: bool) -> Result<Vec<WorkItem>> {
        let mut items = self.cache.daily_view(include_archived)?;
        self.decorate_integrity(&mut items)?;
        Ok(items)
    }

    fn update(
        &mut self,
        id: i64,
        title: Option<&str>,
        description: Option<Option<&str>>,
        actor: &str,
        note: Option<&str>,
    ) -> Result<WorkItem> {
        self.update_fields(id, title, description, actor, note, None)
    }

    fn update_with_event_id(
        &mut self,
        id: i64,
        title: Option<&str>,
        description: Option<Option<&str>>,
        actor: &str,
        note: Option<&str>,
        event_id: Option<&str>,
    ) -> Result<WorkItem> {
        self.update_fields(id, title, description, actor, note, event_id)
    }

    fn set_status(
        &mut self,
        id: i64,
        status: Status,
        actor: &str,
        note: Option<&str>,
    ) -> Result<WorkItem> {
        self.transition_status(id, status, actor, note, None)
    }

    fn set_status_with_event_id(
        &mut self,
        id: i64,
        status: Status,
        actor: &str,
        note: Option<&str>,
        event_id: Option<&str>,
    ) -> Result<WorkItem> {
        self.transition_status(id, status, actor, note, event_id)
    }

    fn add_note(
        &mut self,
        id: i64,
        message: &str,
        actor: &str,
        event_id: Option<&str>,
    ) -> Result<HistoryEntry> {
        self.append_note(id, message, actor, event_id)
    }

    fn history(&mut self, id: i64) -> Result<Vec<HistoryEntry>> {
        self.cache.history(id)
    }

    fn rejected_mutations(&mut self, id: i64) -> Result<Vec<RejectedMutation>> {
        self.cache.rejected_mutations(id)
    }

    fn doctor(&mut self, id: i64) -> Result<IntegrityDoctorReport> {
        let (issue, metadata) = self.load_work_item_issue(id)?;
        let comments = self.load_comments(id)?;
        let timeline = self.load_timeline(id)?;
        self.diagnose_loaded(&issue, &metadata, &comments, timeline)
    }

    fn recover(
        &mut self,
        id: i64,
        mode: RepairMode,
        actor: Option<&str>,
        reason: Option<&str>,
    ) -> Result<RecoveryReport> {
        match mode {
            RepairMode::RestoreExactCopy => self.restore_exact_copy(id),
            RepairMode::Rebaseline => self.rebaseline(id, actor, reason),
        }
    }

    fn take_projection_repairs(&mut self) -> Vec<i64> {
        std::mem::take(&mut self.repaired_work_item_ids)
    }
}

impl GitHub {
    pub fn new() -> Self {
        Self {
            executable: "gh".to_owned(),
            authenticated_login: RefCell::new(None),
        }
    }

    fn with_executable(executable: &Path) -> Self {
        Self {
            executable: executable.to_string_lossy().into_owned(),
            authenticated_login: RefCell::new(None),
        }
    }

    pub fn authenticated_user(&self) -> Result<String> {
        if let Some(login) = self.authenticated_login.borrow().as_ref() {
            return Ok(login.clone());
        }
        let user: User = self.api_json("GET", "user", &[])?;
        if user.login.trim().is_empty() {
            bail!("GitHub returned an empty authenticated login");
        }
        self.authenticated_login.replace(Some(user.login.clone()));
        Ok(user.login)
    }

    pub fn ensure_repository(&self, requested: &RepositoryName) -> Result<(Repository, bool)> {
        match self.api_json_raw::<Repository>("GET", &format!("repos/{requested}"), &[]) {
            Ok(repository) => Ok((repository, false)),
            Err(error) if error.kind == GhFailureKind::NotFound => {
                let authenticated = self.authenticated_user()?;
                let endpoint = if requested.owner().eq_ignore_ascii_case(&authenticated) {
                    "user/repos".to_owned()
                } else {
                    format!("orgs/{}/repos", requested.owner())
                };
                let repository = match self.api_json_raw(
                    "POST",
                    &endpoint,
                    &[
                        ("name", requested.name()),
                        ("private", "true"),
                        ("has_issues", "true"),
                        ("auto_init", "false"),
                    ],
                ) {
                    Ok(repository) => repository,
                    Err(failure)
                        if matches!(
                            failure.kind,
                            GhFailureKind::NotFound | GhFailureKind::PermissionDenied
                        ) || failure
                            .stderr
                            .to_ascii_lowercase()
                            .contains("already exists") =>
                    {
                        return Err(GitHubError::new(
                            GitHubErrorKind::PermissionDenied,
                            format!(
                                "could not access or create GitHub repository {requested}: {}",
                                failure.stderr.trim()
                            ),
                        )
                        .into());
                    }
                    Err(failure) => return Err(classify_failure(failure).into()),
                };
                Ok((repository, true))
            }
            Err(error) => Err(classify_failure(error).into()),
        }
    }

    pub fn validate_repository(&self, repository: &Repository) -> Result<()> {
        if !repository.private {
            return Err(GitHubError::new(
                GitHubErrorKind::InvalidVisibility,
                format!("GitHub repository {} is not private", repository.full_name),
            )
            .into());
        }
        if !repository.has_issues {
            return Err(GitHubError::new(
                GitHubErrorKind::IncompatibleRepository,
                format!(
                    "GitHub repository {} does not have issues enabled",
                    repository.full_name
                ),
            )
            .into());
        }
        if !(repository.permissions.admin || repository.permissions.push) {
            return Err(GitHubError::new(
                GitHubErrorKind::PermissionDenied,
                format!(
                    "GitHub repository {} does not grant write permission",
                    repository.full_name
                ),
            )
            .into());
        }
        Ok(())
    }

    pub fn provision_metadata(&self, repository: &RepositoryName, created: bool) -> Result<()> {
        let endpoint = format!("repos/{repository}/labels?per_page=100");
        let labels: Vec<Label> = self.api_paginated_json("GET", &endpoint)?;
        let existing = validate_reserved_labels(repository, labels, false)?;
        if !created {
            self.validate_contents(repository)?;
        }
        for (name, color, description) in LABELS {
            if !existing.contains_key(&name.to_ascii_lowercase()) {
                self.api_empty(
                    "POST",
                    &format!("repos/{repository}/labels"),
                    &[
                        ("name", name),
                        ("color", color),
                        ("description", description),
                    ],
                )?;
            }
        }
        Ok(())
    }

    pub fn validate_existing_repository(&self, name: &RepositoryName) -> Result<RepositoryName> {
        let repository = match self.api_json_raw::<Repository>("GET", &format!("repos/{name}"), &[])
        {
            Ok(repository) => repository,
            Err(failure) if failure.kind == GhFailureKind::NotFound => {
                return Err(GitHubError::new(
                    GitHubErrorKind::PermissionDenied,
                    format!(
                        "could not access GitHub repository {name}: {}",
                        failure.stderr.trim()
                    ),
                )
                .into());
            }
            Err(failure) => return Err(classify_failure(failure).into()),
        };
        self.validate_repository(&repository)?;
        let canonical_name = repository.full_name.clone();
        let endpoint = format!("repos/{canonical_name}/labels?per_page=100");
        let labels: Vec<Label> = self.api_paginated_json("GET", &endpoint)?;
        validate_reserved_labels(&canonical_name, labels, true)?;
        self.validate_contents(&canonical_name)?;
        Ok(canonical_name)
    }

    fn validate_contents(&self, repository: &RepositoryName) -> Result<()> {
        let issues: Vec<IssueIdentity> = self.api_paginated_json(
            "GET",
            &format!("repos/{repository}/issues?state=all&per_page=100"),
        )?;
        let pulls: Vec<IssueIdentity> = self.api_paginated_json(
            "GET",
            &format!("repos/{repository}/pulls?state=all&per_page=100"),
        )?;
        if let Some(foreign) = pulls.first() {
            return Err(GitHubError::new(
                GitHubErrorKind::IncompatibleRepository,
                format!(
                    "GitHub repository {repository} contains unsupported pull request #{}",
                    foreign.number
                ),
            )
            .into());
        }
        if let Some(foreign) = issues.iter().find(|issue| {
            !issue
                .labels
                .iter()
                .any(|label| label.name.eq_ignore_ascii_case("work-tracker:item"))
        }) {
            return Err(GitHubError::new(
                GitHubErrorKind::IncompatibleRepository,
                format!(
                    "GitHub repository {repository} contains unsupported issue #{}",
                    foreign.number
                ),
            )
            .into());
        }
        if let Some(invalid) = issues.iter().find(|issue| {
            issue
                .labels
                .iter()
                .filter(|label| {
                    LABELS[1..]
                        .iter()
                        .any(|(status, _, _)| label.name.eq_ignore_ascii_case(status))
                })
                .count()
                != 1
        }) {
            return Err(GitHubError::new(
                GitHubErrorKind::IncompatibleRepository,
                format!(
                    "GitHub repository {repository} issue #{} does not have exactly one Work Tracker Status label",
                    invalid.number
                ),
            )
            .into());
        }
        Ok(())
    }

    fn api_paginated_json<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        endpoint: &str,
    ) -> Result<Vec<T>> {
        let output = self
            .api_with_options(method, endpoint, &[], true)
            .map_err(classify_failure)?;
        let pages: Vec<Vec<T>> = serde_json::from_slice(&output)
            .with_context(|| format!("GitHub returned invalid JSON for {method} {endpoint}"))?;
        Ok(pages.into_iter().flatten().collect())
    }

    fn api_conditional_paginated_json<T: for<'de> Deserialize<'de>>(
        &self,
        endpoint: &str,
        etag: Option<&str>,
    ) -> Result<ConditionalResult<T>> {
        let mut values = Vec::new();
        let mut response_etag = None;
        let mut page = 1;
        loop {
            let page_endpoint = format!("{endpoint}&page={page}");
            let headers = if page == 1 {
                etag.map(|etag| format!("If-None-Match: {etag}"))
                    .into_iter()
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            let output = self
                .api_with_request_options("GET", &page_endpoint, &[], false, true, &headers)
                .map_err(classify_failure)?;

            // Compatibility with fixtures written before response metadata was
            // requested: --paginate --slurp returns an outer page array.
            if output.first() == Some(&b'[') {
                let pages: Vec<Vec<T>> = serde_json::from_slice(&output).with_context(|| {
                    format!("GitHub returned invalid JSON for GET {page_endpoint}")
                })?;
                values.extend(pages.into_iter().flatten());
                return Ok(ConditionalResult::Modified { values, etag: None });
            }

            let response = parse_included_response(&output).with_context(|| {
                format!("GitHub returned an invalid included response for GET {page_endpoint}")
            })?;
            if response.status == 304 {
                return Ok(ConditionalResult::NotModified);
            }
            if response.status != 200 {
                bail!(
                    "GitHub returned unexpected HTTP status {} for GET {page_endpoint}",
                    response.status
                );
            }
            if page == 1 {
                response_etag = response.etag;
            }
            let page_values: Vec<T> = serde_json::from_slice(response.body)
                .with_context(|| format!("GitHub returned invalid JSON for GET {page_endpoint}"))?;
            values.extend(page_values);
            if !response.has_next_page {
                return Ok(ConditionalResult::Modified {
                    values,
                    etag: response_etag,
                });
            }
            page += 1;
        }
    }

    fn api_json<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        endpoint: &str,
        fields: &[(&str, &str)],
    ) -> Result<T> {
        let output = self
            .api_with_options(method, endpoint, fields, false)
            .map_err(classify_failure)?;
        serde_json::from_slice(&output)
            .with_context(|| format!("GitHub returned invalid JSON for {method} {endpoint}"))
    }

    fn api_json_raw<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        endpoint: &str,
        fields: &[(&str, &str)],
    ) -> std::result::Result<T, GhFailure> {
        let output = self.api_with_options(method, endpoint, fields, false)?;
        serde_json::from_slice(&output).map_err(|error| GhFailure {
            stderr: format!("GitHub returned invalid JSON for {method} {endpoint}: {error}"),
            kind: GhFailureKind::Other,
        })
    }

    fn api_empty(&self, method: &str, endpoint: &str, fields: &[(&str, &str)]) -> Result<()> {
        self.api_with_options(method, endpoint, fields, false)
            .map_err(classify_failure)?;
        Ok(())
    }

    fn api_with_options(
        &self,
        method: &str,
        endpoint: &str,
        fields: &[(&str, &str)],
        paginate: bool,
    ) -> std::result::Result<Vec<u8>, GhFailure> {
        self.api_with_request_options(method, endpoint, fields, paginate, false, &[])
    }

    fn api_with_request_options(
        &self,
        method: &str,
        endpoint: &str,
        fields: &[(&str, &str)],
        paginate: bool,
        include: bool,
        headers: &[String],
    ) -> std::result::Result<Vec<u8>, GhFailure> {
        let mut command = Command::new(&self.executable);
        command.args(["api", "--method", method, endpoint]);
        if paginate {
            command.args(["--paginate", "--slurp"]);
        }
        if include {
            command.arg("--include");
        }
        for header in headers {
            command.args(["--header", header]);
        }
        for (name, value) in fields {
            command.args(["--field", &format!("{name}={value}")]);
        }
        let output = command.output().map_err(|error| GhFailure {
            stderr: format!("failed to execute gh: {error}"),
            kind: if error.kind() == std::io::ErrorKind::NotFound {
                GhFailureKind::MissingCli
            } else {
                GhFailureKind::Other
            },
        })?;
        if output.status.success() {
            Ok(output.stdout)
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            let lower = stderr.to_ascii_lowercase();
            let kind = if lower.contains("rate limit")
                || lower.contains("secondary rate limit")
                || lower.contains("http 429")
            {
                GhFailureKind::RateLimited
            } else if lower.contains("http 422") || lower.contains("validation failed") {
                GhFailureKind::ValidationFailed
            } else if lower.contains("http 500")
                || lower.contains("http 502")
                || lower.contains("http 503")
                || lower.contains("http 504")
                || lower.contains("service unavailable")
                || lower.contains("bad gateway")
                || lower.contains("gateway timeout")
            {
                GhFailureKind::ServiceFailure
            } else if lower.contains("error connecting")
                || lower.contains("could not resolve host")
                || lower.contains("connection refused")
                || lower.contains("connection reset")
                || lower.contains("network is unreachable")
                || lower.contains("tls handshake timeout")
            {
                GhFailureKind::NetworkFailure
            } else if lower.contains("http 404") || lower.contains("not found") {
                GhFailureKind::NotFound
            } else if lower.contains("not logged")
                || lower.contains("authentication")
                || lower.contains("authenticate")
                || lower.contains("bad credentials")
            {
                GhFailureKind::Unauthenticated
            } else if lower.contains("http 403")
                || lower.contains("forbidden")
                || lower.contains("permission")
                || lower.contains("resource not accessible")
            {
                GhFailureKind::PermissionDenied
            } else {
                GhFailureKind::Other
            };
            Err(GhFailure { stderr, kind })
        }
    }
}

fn parse_included_response(output: &[u8]) -> Result<IncludedResponse<'_>> {
    let (header_bytes, body) =
        if let Some(index) = output.windows(4).position(|part| part == b"\r\n\r\n") {
            (&output[..index], &output[index + 4..])
        } else if let Some(index) = output.windows(2).position(|part| part == b"\n\n") {
            (&output[..index], &output[index + 2..])
        } else {
            bail!("response omitted the HTTP header separator");
        };
    let headers = std::str::from_utf8(header_bytes).context("response headers were not UTF-8")?;
    let mut lines = headers.lines();
    let status = lines
        .next()
        .context("response omitted the HTTP status line")?
        .split_whitespace()
        .nth(1)
        .context("response HTTP status line omitted its code")?
        .parse::<u16>()
        .context("response HTTP status code was invalid")?;
    let mut etag = None;
    let mut has_next_page = false;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("etag") {
            etag = Some(value.trim().to_owned());
        } else if name.eq_ignore_ascii_case("link") {
            has_next_page = value.split(',').any(|link| link.contains("rel=\"next\""));
        }
    }
    Ok(IncludedResponse {
        status,
        etag,
        has_next_page,
        body,
    })
}

fn classify_failure(failure: GhFailure) -> GitHubError {
    let kind = match failure.kind {
        GhFailureKind::MissingCli => GitHubErrorKind::CliMissing,
        GhFailureKind::Unauthenticated => GitHubErrorKind::Unauthenticated,
        GhFailureKind::PermissionDenied => GitHubErrorKind::PermissionDenied,
        GhFailureKind::NotFound => GitHubErrorKind::NotFound,
        GhFailureKind::ValidationFailed => GitHubErrorKind::ValidationFailed,
        GhFailureKind::RateLimited => GitHubErrorKind::RateLimited,
        GhFailureKind::NetworkFailure => GitHubErrorKind::NetworkFailure,
        GhFailureKind::ServiceFailure => GitHubErrorKind::ServiceFailure,
        GhFailureKind::Other => GitHubErrorKind::ApiFailure,
    };
    GitHubError::new(
        kind,
        format!("GitHub API request failed: {}", failure.stderr.trim()),
    )
}

fn new_event_id(kind: &str) -> String {
    format!("{kind}-{}", Uuid::new_v4())
}

fn creation_fingerprint(request_json: &str) -> String {
    format!(
        "creation-{}",
        Uuid::new_v5(&Uuid::NAMESPACE_OID, request_json.as_bytes())
    )
}

fn pending_event_from_request(
    metadata: &ProjectionMetadata,
    request_json: &str,
) -> Result<CanonicalEvent> {
    if creation_fingerprint(request_json) != metadata.creation_fingerprint {
        bail!("pending creation request does not match its remote fingerprint");
    }
    let request: RecoverableCreationRequest =
        serde_json::from_str(request_json).context("invalid pending GitHub creation request")?;
    if request.event_id.as_deref()
        != metadata
            .creation_event_id_supplied
            .then_some(metadata.event_id.as_str())
    {
        bail!("pending creation request has a conflicting event identity");
    }
    Ok(CanonicalEvent {
        schema_version: 1,
        event_id: metadata.event_id.clone(),
        kind: EventKind::Created,
        actor: request.actor,
        github_actor: String::new(),
        note: request.note,
        changes: json!({
            "title": request.title,
            "description": request.description,
            "status": request.status,
        }),
        expected_state_revision: None,
    })
}

fn validate_pending_genesis_intent(
    issue_number: i64,
    metadata: &ProjectionMetadata,
    event: &CanonicalEvent,
) -> Result<()> {
    let values: GenesisValues = serde_json::from_value(event.changes.clone())
        .map_err(|_| metadata_collision(issue_number))?;
    let request_json = serde_json::to_string(&CreationRequest {
        title: &values.title,
        description: values.description.as_deref(),
        status: values.status,
        actor: &event.actor,
        note: event.note.as_deref(),
        event_id: metadata
            .creation_event_id_supplied
            .then_some(event.event_id.as_str()),
    })?;
    if event.schema_version != 1
        || event.event_id != metadata.event_id
        || event.kind != EventKind::Created
        || event.expected_state_revision.is_some()
        || creation_fingerprint(&request_json) != metadata.creation_fingerprint
    {
        return Err(metadata_collision(issue_number).into());
    }
    Ok(())
}

fn projection_body(description: Option<&str>, metadata: &ProjectionMetadata) -> Result<String> {
    let visible = description.unwrap_or("_No description provided._");
    Ok(format!(
        "{visible}\n\n<!-- {PROJECTION_MARKER}\n{}\n-->",
        serde_json::to_string(metadata)?
    ))
}

fn parse_projection(body: &str) -> Result<ProjectionMetadata> {
    serde_json::from_str(extract_metadata(body, PROJECTION_MARKER)?)
        .context("invalid Work Tracker projection metadata")
}

fn parse_event(body: &str) -> Result<CanonicalEvent> {
    let encoded = extract_metadata(body, EVENT_MARKER)?;
    let value: Value =
        serde_json::from_str(encoded).context("invalid Work Tracker event metadata")?;
    let schema_version = value
        .get("schema_version")
        .and_then(Value::as_u64)
        .context("Work Tracker event is missing schema_version")?;
    if schema_version != 1 {
        return Err(GitHubError::new(
            GitHubErrorKind::UnknownEventSchema,
            format!("unsupported Work Tracker event schema version {schema_version}"),
        )
        .into());
    }
    if let Ok(event) = serde_json::from_value::<CanonicalEvent>(value.clone()) {
        return Ok(event);
    }
    let legacy: LegacyGenesisEvent =
        serde_json::from_value(value).context("invalid Work Tracker event metadata")?;
    if legacy.kind != "created"
        || legacy.occurred_at_source != "github_comment.created_at"
        || legacy.schema_version != 1
    {
        bail!("invalid Work Tracker legacy genesis event");
    }
    Ok(CanonicalEvent {
        schema_version: 1,
        event_id: legacy.event_id,
        kind: EventKind::Created,
        actor: legacy.actor,
        github_actor: legacy.github_actor,
        note: legacy.note,
        changes: json!({
            "title": legacy.initial_values.title,
            "description": legacy.initial_values.description,
            "status": legacy.initial_values.status,
        }),
        expected_state_revision: None,
    })
}

fn materialize_item(issue_number: i64, history: &[HistoryEntry]) -> Result<WorkItem> {
    let genesis = history
        .first()
        .context("Work Tracker history has no genesis entry")?;
    let mut values = if genesis.kind == EventKind::Rebaseline.as_str() {
        let changes: RebaselineChanges = serde_json::from_value(genesis.changes.clone())
            .context("invalid Rebaseline changes")?;
        GenesisValues {
            title: changes.title,
            description: changes.description,
            status: changes.status,
        }
    } else {
        serde_json::from_value(genesis.changes.clone()).context("invalid genesis changes")?
    };
    for entry in &history[1..] {
        if let Some(kind) = EventKind::from_history_name(&entry.kind)
            && let Some(changes) = MutationChanges::parse(kind, entry.changes.clone())?
        {
            changes.apply_to(&mut values);
        }
    }
    let updated_at = history
        .last()
        .map(|entry| entry.occurred_at)
        .unwrap_or(genesis.occurred_at);
    let archived_at = (values.status == Status::Archived).then_some(updated_at);
    Ok(WorkItem {
        id: issue_number,
        title: values.title,
        description: values.description,
        status: values.status,
        created_at: genesis.occurred_at,
        updated_at,
        archived_at,
        deleted_at: archived_at,
        purge_after: None,
        ledger_integrity_error: false,
    })
}

fn reviewed_item_from_issue(issue: &LedgerIssue, comments: &[LedgerComment]) -> Result<WorkItem> {
    let status = if issue.locked {
        Status::Archived
    } else {
        let mut statuses = issue.labels.iter().filter_map(|label| {
            LABELS[1..]
                .iter()
                .find(|(canonical, _, _)| label.name.eq_ignore_ascii_case(canonical))
                .and_then(|(canonical, _, _)| canonical.rsplit(':').next())
                .and_then(|status| Status::from_str(status).ok())
        });
        let status = statuses
            .next()
            .context("unlocked issue has no valid Work Tracker status label")?;
        if statuses.next().is_some() {
            bail!("unlocked issue has multiple Work Tracker status labels");
        }
        status
    };
    let observed_at = comments
        .iter()
        .map(|comment| comment.created_at)
        .min()
        .or(issue.updated_at)
        .unwrap_or_else(Utc::now);
    let updated_at = issue.updated_at.unwrap_or(observed_at);
    let visible = projection_visible_text(&issue.body).unwrap_or_default();
    let description =
        (!matches!(visible, "" | "_No description provided._")).then(|| visible.to_owned());
    let archived_at = (status == Status::Archived).then_some(updated_at);
    Ok(WorkItem {
        id: issue.number,
        title: issue
            .title
            .clone()
            .unwrap_or_else(|| format!("GitHub issue #{}", issue.number)),
        description,
        status,
        created_at: observed_at,
        updated_at,
        archived_at,
        deleted_at: archived_at,
        purge_after: None,
        ledger_integrity_error: true,
    })
}

fn extract_metadata<'a>(body: &'a str, marker: &str) -> Result<&'a str> {
    let prefix = format!("<!-- {marker}\n");
    let start = body
        .rfind(&prefix)
        .map(|index| index + prefix.len())
        .with_context(|| format!("missing {marker} metadata"))?;
    let end = body[start..]
        .find("\n-->")
        .map(|index| start + index)
        .with_context(|| format!("unterminated {marker} metadata"))?;
    Ok(&body[start..end])
}

fn has_metadata(body: &str, marker: &str) -> bool {
    body.contains(&format!("<!-- {marker}\n"))
}

fn is_projected_or_structured_event(
    metadata: &ProjectionMetadata,
    comment: &LedgerComment,
) -> bool {
    has_metadata(&comment.body, EVENT_MARKER)
        || metadata.genesis_comment_id == Some(comment.id)
        || metadata.head_comment_id == Some(comment.id)
}

fn recovery_evidence_snapshot(
    comments: &[LedgerComment],
    metadata: &ProjectionMetadata,
    cached_comment_ids: &HashSet<i64>,
    excluded_comment_id: Option<i64>,
) -> Vec<ObservedIntegrityEvidence> {
    let mut evidence = comments
        .iter()
        .filter(|comment| Some(comment.id) != excluded_comment_id)
        .filter(|comment| {
            cached_comment_ids.contains(&comment.id)
                || is_projected_or_structured_event(metadata, comment)
        })
        .map(observed_integrity_evidence)
        .collect::<Vec<_>>();
    evidence.sort_by_key(|evidence| evidence.github_comment_id);
    evidence
}

fn rebaseline_snapshot_matches(
    issue: &LedgerIssue,
    metadata: &ProjectionMetadata,
    reviewed: &WorkItem,
    evidence: &[ObservedIntegrityEvidence],
    candidate: &RebaselineValidationSnapshot,
) -> bool {
    candidate.metadata == *metadata
        && candidate.issue.locked == issue.locked
        && candidate.reviewed.title == reviewed.title
        && candidate.reviewed.description == reviewed.description
        && candidate.reviewed.status == reviewed.status
        && candidate.evidence == evidence
}

fn rebaseline_anchor_is_exact(
    comments: &[LedgerComment],
    anchor_id: i64,
    github_actor: &str,
    body: &str,
) -> bool {
    comments.iter().any(|comment| {
        comment.id == anchor_id && comment.user.login == github_actor && comment.body == body
    })
}

fn rebaseline_anchor_evidence(
    published: &ObservedIntegrityEvidence,
    comments: &[LedgerComment],
) -> Vec<ObservedIntegrityEvidence> {
    let mut evidence = vec![published.clone()];
    if let Some(observed) = comments
        .iter()
        .find(|comment| comment.id == published.github_comment_id)
        .map(observed_integrity_evidence)
        && (observed.github_actor != published.github_actor || observed.body != published.body)
    {
        evidence.push(observed);
    }
    evidence
}

fn observed_integrity_evidence(comment: &LedgerComment) -> ObservedIntegrityEvidence {
    ObservedIntegrityEvidence {
        github_comment_id: comment.id,
        github_actor: comment.user.login.clone(),
        body: comment.body.clone(),
        observed_at: comment.created_at,
    }
}

fn event_comment_body(event: &CanonicalEvent) -> Result<String> {
    let note = event
        .note
        .as_deref()
        .map(|note| format!("\n\nNote: {note}"))
        .unwrap_or_default();
    let record_type = if event.expected_state_revision.is_some() {
        "Mutation Proposal"
    } else {
        "History Entry"
    };
    Ok(format!(
        "Work Tracker {record_type}: {} by {}{note}\n\n<!-- {EVENT_MARKER}\n{}\n-->",
        event.kind.as_str(),
        event.actor,
        serde_json::to_string(event)?
    ))
}

fn history_hash(
    previous_hash: Option<&str>,
    event: &CanonicalEvent,
    comment_id: i64,
) -> Result<String> {
    let canonical = serde_json::to_vec(event)?;
    let mut hasher = Sha256::new();
    hasher.update(b"work-tracker-history-v1");
    hash_part(&mut hasher, previous_hash.unwrap_or("").as_bytes());
    hash_part(&mut hasher, &canonical);
    hash_part(&mut hasher, &comment_id.to_be_bytes());
    Ok(format!("{:x}", hasher.finalize()))
}

fn event_evidence(
    comments: &[LedgerComment],
    history: &[HistoryEntry],
) -> Vec<GithubEventEvidence> {
    comments
        .iter()
        .filter(|comment| {
            has_metadata(&comment.body, EVENT_MARKER)
                && history.iter().any(|entry| entry.id == comment.id)
        })
        .map(|comment| {
            let event_id = parse_event(&comment.body).ok().map(|event| event.event_id);
            let history_hash = history
                .iter()
                .find(|entry| entry.id == comment.id)
                .and_then(|entry| entry.history_hash.clone());
            GithubEventEvidence {
                comment_id: comment.id,
                event_id,
                github_actor: comment.user.login.clone(),
                body: comment.body.clone(),
                history_hash,
            }
        })
        .collect()
}

fn cached_exact_chain_is_verified(evidence: &[GithubEventEvidence]) -> bool {
    let mut previous_hash: Option<String> = None;
    for expected in evidence {
        let Ok(event) = parse_event(&expected.body) else {
            return false;
        };
        if event.github_actor != expected.github_actor
            || expected.event_id.as_deref() != Some(event.event_id.as_str())
        {
            return false;
        }
        let Ok(observed_hash) = history_hash(previous_hash.as_deref(), &event, expected.comment_id)
        else {
            return false;
        };
        if expected.history_hash.as_deref() != Some(observed_hash.as_str()) {
            return false;
        }
        previous_hash = Some(observed_hash);
    }
    true
}

fn cached_chain_matches_projection(
    evidence: &[GithubEventEvidence],
    metadata: &ProjectionMetadata,
) -> bool {
    let (Some(genesis), Some(head)) = (evidence.first(), evidence.last()) else {
        return false;
    };
    genesis.comment_id == metadata.genesis_comment_id.unwrap_or_default()
        && genesis.event_id.as_deref() == Some(metadata.event_id.as_str())
        && head.comment_id == metadata.head_comment_id.unwrap_or_default()
        && head.event_id.as_deref() == metadata.head_event_id.as_deref()
        && head.history_hash.as_deref() == metadata.history_hash.as_deref()
}

fn exact_chain_preserves_live_archive(
    issue: &LedgerIssue,
    evidence: &[GithubEventEvidence],
) -> bool {
    if !issue.locked {
        return true;
    }
    let comments = evidence
        .iter()
        .map(|evidence| LedgerComment {
            id: evidence.comment_id,
            created_at: Utc::now(),
            user: User {
                login: evidence.github_actor.clone(),
            },
            body: evidence.body.clone(),
        })
        .collect::<Vec<_>>();
    replay_trusted_history(issue.number, &comments)
        .and_then(|replayed| materialize_item(issue.number, &replayed.accepted))
        .is_ok_and(|item| item.status == Status::Archived)
}

fn next_observed_evidence_id(evidence: &[RetainedRecoveryEvidence], github_comment_id: i64) -> i64 {
    let mut candidate = -github_comment_id;
    while evidence
        .iter()
        .any(|evidence| evidence.evidence_id == candidate)
    {
        candidate -= 1;
    }
    candidate
}

fn hash_part(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

fn replay_history(issue_number: i64, comments: &[LedgerComment]) -> Result<ReplayedHistory> {
    let mut comments = comments.to_vec();
    comments.sort_by_key(|comment| comment.id);
    if let Some(anchor) = comments.iter().rposition(|comment| {
        has_metadata(&comment.body, EVENT_MARKER)
            && parse_event(&comment.body).is_ok_and(|event| event.kind == EventKind::Rebaseline)
    }) {
        comments = comments.split_off(anchor);
    }
    let mut history = Vec::new();
    let mut untrusted = Vec::new();
    let mut rejected = Vec::new();
    let mut seen = HashMap::<String, CanonicalEvent>::new();
    let mut previous_hash: Option<String> = None;
    let mut state_revision = 0_u64;
    let mut archived = false;
    for comment in comments {
        if !has_metadata(&comment.body, EVENT_MARKER) {
            continue;
        }
        let event = parse_event(&comment.body)?;
        if event.github_actor != comment.user.login {
            return Err(metadata_collision(issue_number).into());
        }
        if let Some(prior) = seen.get(&event.event_id) {
            if prior == &event {
                continue;
            }
            return Err(metadata_collision(issue_number).into());
        }
        if archived {
            seen.insert(event.event_id.clone(), event);
            continue;
        }
        match event.kind {
            EventKind::Created if history.is_empty() => {
                if !event.changes.is_object() || event.expected_state_revision.is_some() {
                    return Err(metadata_collision(issue_number).into());
                }
                state_revision = 1;
            }
            EventKind::Rebaseline if history.is_empty() => {
                let changes: RebaselineChanges = serde_json::from_value(event.changes.clone())
                    .context("invalid Rebaseline changes")?;
                if event.expected_state_revision.is_some()
                    || event
                        .note
                        .as_deref()
                        .is_none_or(|reason| reason.trim().is_empty())
                    || event.actor.trim().is_empty()
                    || changes.title.trim().is_empty()
                {
                    return Err(metadata_collision(issue_number).into());
                }
                untrusted = retained_evidence_history(
                    issue_number,
                    &changes.prior_evidence,
                    comment.created_at,
                );
                state_revision = 1;
            }
            EventKind::Noted if !history.is_empty() => {
                if event.note.is_none()
                    || event.changes != json!({})
                    || event.expected_state_revision.is_some()
                {
                    return Err(metadata_collision(issue_number).into());
                }
            }
            EventKind::Updated | EventKind::StatusChanged | EventKind::Archived
                if !history.is_empty() =>
            {
                seen.insert(event.event_id.clone(), event.clone());
                let Some(expected) = event.expected_state_revision else {
                    // Invalid proposals are not Rejected Mutations: that domain
                    // term is reserved for otherwise-valid proposals whose
                    // expected State Revision is stale. Remembering the event
                    // ID above still makes duplicate/collision handling stable.
                    continue;
                };
                let Some(item) = materialize_at_revision(issue_number, &history, expected) else {
                    continue;
                };
                let Ok(Some(changes)) = MutationChanges::parse(event.kind, event.changes.clone())
                else {
                    continue;
                };
                if !changes.is_valid_for(event.kind, &item) {
                    continue;
                }
                if expected != state_revision {
                    rejected.push(rejected_mutation(
                        issue_number,
                        &comment,
                        &event,
                        expected,
                        state_revision,
                    ));
                    continue;
                }
                state_revision += 1;
            }
            _ => return Err(metadata_collision(issue_number).into()),
        }
        let current_hash = history_hash(previous_hash.as_deref(), &event, comment.id)?;
        history.push(HistoryEntry {
            id: comment.id,
            work_item_id: issue_number,
            event_id: Some(event.event_id.clone()),
            kind: event.kind.as_str().to_owned(),
            actor: event.actor.clone(),
            github_actor: Some(event.github_actor.clone()),
            note: event.note.clone(),
            occurred_at: comment.created_at,
            changes: event.changes.clone(),
            previous_history_hash: previous_hash.clone(),
            history_hash: Some(current_hash.clone()),
            state_revision: Some(state_revision),
            trust: EvidenceTrust::Trusted,
        });
        previous_hash = Some(current_hash);
        archived = event.kind == EventKind::Archived
            || (event.kind == EventKind::Rebaseline
                && serde_json::from_value::<RebaselineChanges>(event.changes.clone())
                    .is_ok_and(|values| values.status == Status::Archived));
        seen.insert(event.event_id.clone(), event);
    }
    if history
        .first()
        .is_none_or(|entry| !matches!(entry.kind.as_str(), "created" | "rebaseline"))
    {
        return Err(metadata_collision(issue_number).into());
    }
    Ok(ReplayedHistory {
        accepted: history,
        untrusted,
        rejected,
    })
}

fn retained_evidence_history(
    issue_number: i64,
    evidence: &[RetainedRecoveryEvidence],
    fallback_time: DateTime<Utc>,
) -> Vec<HistoryEntry> {
    evidence
        .iter()
        .map(|evidence| {
            let parsed = parse_event(&evidence.body).ok();
            HistoryEntry {
                id: evidence.evidence_id,
                work_item_id: issue_number,
                event_id: evidence
                    .event_id
                    .clone()
                    .or_else(|| parsed.as_ref().map(|event| event.event_id.clone())),
                kind: parsed
                    .as_ref()
                    .map_or("untrusted_evidence", |event| event.kind.as_str())
                    .to_owned(),
                actor: parsed.as_ref().map_or_else(
                    || evidence.github_actor.clone(),
                    |event| event.actor.clone(),
                ),
                github_actor: Some(evidence.github_actor.clone()),
                note: parsed.as_ref().and_then(|event| event.note.clone()),
                occurred_at: evidence.occurred_at.unwrap_or(fallback_time),
                changes: parsed.as_ref().map_or_else(
                    || json!({"retained_body": evidence.body}),
                    |event| {
                        let mut changes = event.changes.clone();
                        if let Some(changes) = changes.as_object_mut() {
                            changes.insert(
                                "retained_body".to_owned(),
                                Value::String(evidence.body.clone()),
                            );
                            return Value::Object(changes.clone());
                        }
                        json!({
                            "observed_changes": changes,
                            "retained_body": evidence.body,
                        })
                    },
                ),
                previous_history_hash: None,
                history_hash: evidence.history_hash.clone(),
                state_revision: None,
                trust: EvidenceTrust::Untrusted,
            }
        })
        .collect()
}

fn rejected_mutation(
    issue_number: i64,
    comment: &LedgerComment,
    event: &CanonicalEvent,
    expected_state_revision: u64,
    current_state_revision: u64,
) -> RejectedMutation {
    RejectedMutation {
        id: comment.id,
        work_item_id: issue_number,
        event_id: event.event_id.clone(),
        actor: event.actor.clone(),
        github_actor: event.github_actor.clone(),
        note: event.note.clone(),
        occurred_at: comment.created_at,
        expected_state_revision,
        current_state_revision,
        changes: event.changes.clone(),
    }
}

fn retried_rejected_mutation_error(mutation: &RejectedMutation, current: &WorkItem) -> GitHubError {
    GitHubError::new(
        GitHubErrorKind::RejectedMutation,
        format!(
            "Rejected Mutation for work item {}: expected State Revision {}, current State Revision is {}; current values are title={:?}, description={:?}, Status={}; this stable event ID already records the rejected result",
            mutation.work_item_id,
            mutation.expected_state_revision,
            mutation.current_state_revision,
            current.title,
            current.description,
            current.status,
        ),
    )
    .with_details(json!({
        "kind": "rejected_mutation",
        "event_id": mutation.event_id,
        "expected_state_revision": mutation.expected_state_revision,
        "current_state_revision": mutation.current_state_revision,
        "current_values": {
            "title": current.title,
            "description": current.description,
            "status": current.status,
        },
        "instruction": "refresh current values, decide whether the change is still appropriate, and retry with a new event ID only when submitting a new proposal",
        "projection_pending": false,
    }))
}

fn retry_status_matches(changes: &Value, requested: Status) -> bool {
    serde_json::from_value::<StatusChanges>(changes.clone())
        .is_ok_and(|changes| changes.status.to == requested)
}

fn retry_fields_match(
    changes: &Value,
    requested_title: Option<&String>,
    requested_description: Option<&Option<String>>,
) -> bool {
    serde_json::from_value::<FieldChanges>(changes.clone()).is_ok_and(|changes| {
        let title_matches = requested_title.map_or_else(
            || changes.title.is_none(),
            |requested| {
                changes
                    .title
                    .as_ref()
                    .is_some_and(|change| &change.to == requested)
            },
        );
        let description_matches = requested_description.map_or_else(
            || changes.description.is_none(),
            |requested| {
                changes
                    .description
                    .as_ref()
                    .is_some_and(|change| &change.to == requested)
            },
        );
        title_matches && description_matches
    })
}

fn materialize_at_revision(
    issue_number: i64,
    history: &[HistoryEntry],
    revision: u64,
) -> Option<WorkItem> {
    let entries = history
        .iter()
        .take_while(|entry| entry.state_revision.is_some_and(|value| value <= revision))
        .cloned()
        .collect::<Vec<_>>();
    entries
        .last()
        .filter(|entry| entry.state_revision == Some(revision))?;
    materialize_item(issue_number, &entries).ok()
}

fn replay_trusted_history(
    issue_number: i64,
    comments: &[LedgerComment],
) -> Result<ReplayedHistory> {
    replay_history(issue_number, comments).map_err(|error| {
        if error
            .downcast_ref::<GitHubError>()
            .is_some_and(|error| error.kind() == GitHubErrorKind::UnknownEventSchema)
        {
            error
        } else {
            ledger_integrity_error(issue_number, format!("history replay failed: {error:#}")).into()
        }
    })
}

fn projection_head_needs_update(
    issue_number: i64,
    metadata: &ProjectionMetadata,
    history: &[HistoryEntry],
    legacy_genesis: Option<&HistoryEntry>,
) -> Result<bool> {
    let genesis = history
        .first()
        .ok_or_else(|| ledger_integrity_error(issue_number, "history has no genesis entry"))?;
    if metadata.genesis_comment_id != Some(genesis.id)
        || genesis.event_id.as_deref() != Some(metadata.event_id.as_str())
        || !matches!(genesis.kind.as_str(), "created" | "rebaseline")
    {
        return Err(ledger_integrity_error(
            issue_number,
            "the replayed genesis does not match the stored projection",
        )
        .into());
    }

    let head = history
        .last()
        .context("Work Tracker history has no genesis entry")?;
    match (
        metadata.head_event_id.as_deref(),
        metadata.head_comment_id,
        metadata.history_hash.as_deref(),
    ) {
        (None, None, None) => {
            if legacy_genesis.is_some_and(|evidence| legacy_genesis_matches(evidence, genesis)) {
                Ok(true)
            } else {
                Err(ledger_integrity_error(
                    issue_number,
                    "the projection is missing a trusted history head and no matching pre-head cache evidence exists",
                )
                .into())
            }
        }
        (Some(event_id), Some(comment_id), Some(history_hash)) => {
            let projected = history
                .iter()
                .find(|entry| entry.id == comment_id && entry.event_id.as_deref() == Some(event_id))
                .ok_or_else(|| {
                    ledger_integrity_error(
                        issue_number,
                        "the stored projection head is absent from replayed history",
                    )
                })?;
            if projected.history_hash.as_deref() != Some(history_hash)
                || projected.state_revision != Some(metadata.state_revision)
            {
                return Err(ledger_integrity_error(
                    issue_number,
                    "the replayed hash chain does not match the stored projection head",
                )
                .into());
            }
            Ok(projected.id != head.id)
        }
        _ => Err(
            ledger_integrity_error(issue_number, "the stored projection head is incomplete").into(),
        ),
    }
}

fn legacy_genesis_matches(evidence: &HistoryEntry, replayed: &HistoryEntry) -> bool {
    evidence.id == replayed.id
        && evidence.work_item_id == replayed.work_item_id
        && evidence.kind == replayed.kind
        && evidence.actor == replayed.actor
        && evidence.note == replayed.note
        && evidence.occurred_at == replayed.occurred_at
        && evidence.changes == replayed.changes
        && evidence.event_id.is_none()
        && evidence.github_actor.is_none()
        && evidence.previous_history_hash.is_none()
        && evidence.history_hash.is_none()
        && evidence.state_revision.is_none()
}

fn validate_retry(entry: &HistoryEntry, event: &CanonicalEvent) -> Result<()> {
    if entry.kind != event.kind.as_str()
        || entry.actor != event.actor
        || entry.github_actor.as_deref() != Some(event.github_actor.as_str())
        || entry.note != event.note
        || entry.changes != event.changes
    {
        return Err(GitHubError::new(
            GitHubErrorKind::MetadataCollision,
            format!(
                "event ID {} was already used for different content",
                event.event_id
            ),
        )
        .into());
    }
    Ok(())
}

fn projection_visible_text(body: &str) -> Result<&str> {
    let marker = format!("\n\n<!-- {PROJECTION_MARKER}\n");
    body.rfind(&marker)
        .map(|index| &body[..index])
        .context("missing Work Tracker projection metadata")
}

fn readable_projection_differs(issue: &LedgerIssue, item: &WorkItem) -> Result<bool> {
    let visible_description = projection_visible_text(&issue.body)?;
    let description_differs = match item.description.as_deref() {
        Some(expected) => visible_description != expected,
        None => !matches!(visible_description, "" | "_No description provided._"),
    };
    Ok(issue
        .title
        .as_deref()
        .is_some_and(|title| title != item.title)
        || description_differs)
}

fn validate_completed_issue(issue: &LedgerIssue) -> Result<ProjectionMetadata> {
    let has_item_label = issue
        .labels
        .iter()
        .any(|label| label.name.eq_ignore_ascii_case("work-tracker:item"));
    let metadata = parse_projection(&issue.body).map_err(|_| metadata_collision(issue.number))?;
    if !has_item_label
        || metadata.schema_version != 1
        || metadata.kind != "work_item"
        || metadata.pending_genesis_event_id.is_some()
        || metadata.genesis_comment_id.is_none()
        || metadata.state_revision == 0
    {
        return Err(metadata_collision(issue.number).into());
    }
    Ok(metadata)
}

fn validate_creation_issue(
    issue: &LedgerIssue,
    event_id: &str,
    creation_fingerprint: &str,
    status: Status,
) -> Result<ProjectionMetadata> {
    let has_item_label = issue
        .labels
        .iter()
        .any(|label| label.name.eq_ignore_ascii_case("work-tracker:item"));
    if !has_item_label {
        return Err(metadata_collision(issue.number).into());
    }
    let metadata = parse_projection(&issue.body).map_err(|_| metadata_collision(issue.number))?;
    if metadata.schema_version != 1
        || metadata.kind != "work_item"
        || metadata.event_id != event_id
        || metadata.creation_fingerprint != creation_fingerprint
        || !has_valid_projection_stage(&metadata)
    {
        return Err(metadata_collision(issue.number).into());
    }
    if metadata.pending_genesis_event_id.is_some() {
        validate_status_label(issue, status)?;
    }
    Ok(metadata)
}

fn has_valid_projection_stage(metadata: &ProjectionMetadata) -> bool {
    (metadata.pending_genesis_event_id.as_deref() == Some(metadata.event_id.as_str())
        && metadata.genesis_comment_id.is_none()
        && metadata.state_revision == 0)
        || (metadata.pending_genesis_event_id.is_none()
            && metadata.genesis_comment_id.is_some()
            && metadata.state_revision > 0)
}

fn validate_status_label(issue: &LedgerIssue, status: Status) -> Result<()> {
    if !status_label_matches(issue, status) {
        return Err(metadata_collision(issue.number).into());
    }
    Ok(())
}

fn status_projection_differs(issue: &LedgerIssue, status: Status) -> bool {
    if !status_label_matches(issue, status) {
        return true;
    }
    let Some(state) = issue.state.as_deref() else {
        return false;
    };
    if status.is_actionable() {
        !state.eq_ignore_ascii_case("open")
    } else {
        let expected_reason = if status == Status::Done {
            "completed"
        } else {
            "not_planned"
        };
        !state.eq_ignore_ascii_case("closed")
            || issue
                .state_reason
                .as_deref()
                .is_none_or(|reason| !reason.eq_ignore_ascii_case(expected_reason))
    }
}

fn projection_differs(
    issue: &LedgerIssue,
    item: &WorkItem,
    history_head_differs: bool,
) -> Result<bool> {
    Ok(history_head_differs
        || readable_projection_differs(issue, item)?
        || status_projection_differs(issue, item.status)
        || lock_projection_differs(issue, item.status))
}

fn lock_projection_differs(issue: &LedgerIssue, status: Status) -> bool {
    issue.locked != (status == Status::Archived)
}

fn status_label_matches(issue: &LedgerIssue, status: Status) -> bool {
    let expected = format!("work-tracker:status:{}", status.as_str());
    let mut labels = issue
        .labels
        .iter()
        .filter(|label| is_status_label(&label.name));
    labels
        .next()
        .is_some_and(|label| label.name.eq_ignore_ascii_case(&expected))
        && labels.next().is_none()
}

fn is_status_label(name: &str) -> bool {
    LABELS[1..]
        .iter()
        .any(|(status, _, _)| name.eq_ignore_ascii_case(status))
}

fn metadata_collision(issue_number: i64) -> GitHubError {
    GitHubError::new(
        GitHubErrorKind::MetadataCollision,
        format!("GitHub issue #{issue_number} has conflicting Work Tracker metadata"),
    )
}

fn ledger_integrity_error(issue_number: i64, detail: impl fmt::Display) -> GitHubError {
    GitHubError::new(
        GitHubErrorKind::LedgerIntegrity,
        format!("Ledger Integrity Error for GitHub issue #{issue_number}: {detail}"),
    )
}

fn recovery_validation_failed(issue_number: i64, detail: impl fmt::Display) -> GitHubError {
    GitHubError::new(
        GitHubErrorKind::RecoveryValidationFailed,
        format!("recovery validation failed for work item {issue_number}: {detail}"),
    )
    .with_details(json!({
        "work_item_id": issue_number,
        "outcome": "failed_validation",
        "integrity_error": true,
    }))
}

fn recovery_still_blocked(issue_number: i64, detail: impl fmt::Display) -> GitHubError {
    GitHubError::new(
        GitHubErrorKind::RecoveryStillBlocked,
        format!("work item {issue_number} is still blocked by Ledger Integrity Error: {detail}"),
    )
    .with_details(json!({
        "work_item_id": issue_number,
        "outcome": "still_blocked",
        "integrity_error": true,
    }))
}

fn validate_reserved_labels(
    repository: &RepositoryName,
    labels: Vec<Label>,
    require_all: bool,
) -> Result<HashMap<String, Label>> {
    let existing: HashMap<String, Label> = labels
        .into_iter()
        .map(|label| (label.name.to_ascii_lowercase(), label))
        .collect();
    for (expected, color, description) in LABELS {
        let Some(label) = existing.get(&expected.to_ascii_lowercase()) else {
            if require_all {
                return Err(GitHubError::new(
                    GitHubErrorKind::IncompatibleRepository,
                    format!("GitHub repository {repository} is missing reserved label {expected}"),
                )
                .into());
            }
            continue;
        };
        if !label.color.eq_ignore_ascii_case(color)
            || label.description.as_deref() != Some(description)
        {
            return Err(GitHubError::new(
                GitHubErrorKind::IncompatibleRepository,
                format!(
                    "GitHub repository {repository} has an incompatible reserved label {expected}"
                ),
            )
            .into());
        }
    }
    Ok(existing)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_event_bytes_and_genesis_hash_match_the_schema_v1_vector() -> Result<()> {
        let event = CanonicalEvent {
            schema_version: 1,
            event_id: "note-known".to_owned(),
            kind: EventKind::Noted,
            actor: "agent-a".to_owned(),
            github_actor: "octocat".to_owned(),
            note: Some("known".to_owned()),
            changes: json!({}),
            expected_state_revision: None,
        };

        assert_eq!(
            serde_json::to_string(&event)?,
            r#"{"schema_version":1,"event_id":"note-known","kind":"noted","actor":"agent-a","github_actor":"octocat","note":"known","changes":{}}"#
        );
        assert_eq!(
            history_hash(None, &event, 42)?,
            "79e879fd72adacc4ae4131da76b1cadb83afe65479ad8e5f7ca2924b0a916e40"
        );
        Ok(())
    }
}
