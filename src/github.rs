use std::{cell::RefCell, collections::HashMap, fmt, path::Path, process::Command, str::FromStr};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    db::SqliteLedger,
    domain::{HistoryEntry, Status, WorkItem, normalized_optional, normalized_required},
    ledger::{Ledger, ListFilter},
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
    Other,
}

#[derive(Debug)]
pub struct GitHubError {
    code: &'static str,
    message: String,
}

impl GitHubError {
    pub fn code(&self) -> &'static str {
        self.code
    }

    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
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
}

#[derive(Debug, Serialize)]
struct CreationRequest<'a> {
    title: &'a str,
    description: Option<&'a str>,
    status: Status,
    actor: &'a str,
    note: Option<&'a str>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProjectionMetadata {
    schema_version: u32,
    kind: String,
    event_id: String,
    creation_fingerprint: String,
    pending_genesis_event_id: Option<String>,
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
    kind: String,
    actor: String,
    github_actor: String,
    note: Option<String>,
    changes: Value,
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
    body: String,
    #[serde(default)]
    labels: Vec<IssueLabel>,
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
    pub(crate) fn open(repository: RepositoryName, cache_path: &Path) -> Result<Self> {
        let cache = SqliteLedger::open(cache_path)?;
        let recover_pending_remotely =
            !cache.github_cache_is_initialized(&repository.to_string())?;
        Ok(Self {
            repository,
            github: GitHub::new(),
            cache,
            recover_pending_remotely,
        })
    }

    fn create_work_item(
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
        let note = normalized_optional(note);
        let request = CreationRequest {
            title: &title,
            description: description.as_deref(),
            status,
            actor: &actor,
            note: note.as_deref(),
        };
        let request_json = serde_json::to_string(&request)?;
        let creation_fingerprint = creation_fingerprint(&request_json);
        let existing = self.cache.pending_github_creation(&request_json)?;
        let github_actor = self.github.authenticated_user()?;
        let (event_id, known_issue, retrying, recovered_issue) = if let Some(pending) = existing {
            (pending.event_id, pending.issue_number, true, None)
        } else if self.recover_pending_remotely {
            if let Some((issue, metadata)) =
                self.find_pending_issue_by_fingerprint(&creation_fingerprint, status)?
            {
                self.cache
                    .begin_github_creation(&request_json, &metadata.event_id)?;
                (metadata.event_id, Some(issue.number), true, Some(issue))
            } else {
                let event_id = new_event_id("genesis");
                self.cache.begin_github_creation(&request_json, &event_id)?;
                (event_id, None, false, None)
            }
        } else {
            let event_id = new_event_id("genesis");
            self.cache.begin_github_creation(&request_json, &event_id)?;
            (event_id, None, false, None)
        };
        let pending_metadata = ProjectionMetadata {
            schema_version: 1,
            kind: "work_item".to_owned(),
            event_id: event_id.clone(),
            creation_fingerprint: creation_fingerprint.clone(),
            pending_genesis_event_id: Some(event_id.clone()),
            genesis_comment_id: None,
            state_revision: 0,
            head_event_id: None,
            head_comment_id: None,
            history_hash: None,
        };
        let pending_body = projection_body(description.as_deref(), &pending_metadata)?;

        let issue = if let Some(issue) = recovered_issue {
            issue
        } else if let Some(issue_number) = known_issue {
            self.load_pending_issue(issue_number, &event_id, &creation_fingerprint, status)?
        } else if retrying {
            match self.find_pending_issue(&event_id, &creation_fingerprint, status)? {
                Some(issue) => issue,
                None => self.create_pending_issue(&title, &pending_body, status)?,
            }
        } else {
            self.create_pending_issue(&title, &pending_body, status)?
        };
        self.cache
            .remember_github_issue(&request_json, issue.number)?;

        let event = CanonicalEvent {
            schema_version: 1,
            event_id: event_id.clone(),
            kind: "created".to_owned(),
            actor: actor.clone(),
            github_actor,
            note: note.clone(),
            changes: json!({
                "title": title.clone(),
                "description": description.clone(),
                "status": status,
            }),
        };
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
            pending_genesis_event_id: None,
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
        };
        let history = HistoryEntry {
            id: comment.id,
            work_item_id: issue.number,
            event_id: Some(event_id),
            kind: "created".to_owned(),
            actor,
            github_actor: Some(event.github_actor),
            note,
            occurred_at: comment.created_at,
            changes: event.changes,
            previous_history_hash: None,
            history_hash: Some(history_hash),
            state_revision: Some(1),
        };
        self.cache
            .finish_github_creation(&request_json, &item, &history)?;
        Ok(item)
    }

    fn synchronize(&mut self) -> Result<()> {
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
            ConditionalResult::NotModified => return Ok(()),
            ConditionalResult::Modified { values, etag } => (values, etag),
        };
        let mut synchronized = Vec::new();
        let mut projection_updates = Vec::new();
        let mut removed_item_ids = Vec::new();
        let mut cursor = None;
        for issue in issues {
            let issue_updated_at = issue.updated_at.ok_or_else(|| {
                GitHubError::new(
                    "github_incompatible_metadata",
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
            let has_metadata = issue.body.contains(PROJECTION_MARKER);
            if !has_item_label && !has_metadata {
                removed_item_ids.push(issue.number);
                continue;
            }
            if has_item_label != has_metadata {
                return Err(metadata_collision(issue.number).into());
            }
            let metadata =
                parse_projection(&issue.body).map_err(|_| metadata_collision(issue.number))?;
            if metadata.schema_version != 1
                || metadata.kind != "work_item"
                || !has_valid_projection_stage(&metadata)
            {
                return Err(metadata_collision(issue.number).into());
            }
            if metadata.pending_genesis_event_id.is_some() {
                continue;
            }
            let comments: Vec<LedgerComment> = self.github.api_paginated_json(
                "GET",
                &format!(
                    "repos/{}/issues/{}/comments?per_page=100",
                    self.repository, issue.number
                ),
            )?;
            let history = replay_trusted_history(issue.number, &comments)?;
            let legacy_genesis = self.cache.legacy_github_genesis_evidence(issue.number)?;
            let projection_needs_update = projection_head_needs_update(
                issue.number,
                &metadata,
                &history,
                legacy_genesis.as_ref(),
            )?;
            let item = materialize_item(issue.number, &history)?;
            validate_status_label(&issue, item.status)?;
            if projection_needs_update {
                projection_updates.push((issue, metadata, history.clone()));
            }
            synchronized.push((item, history));
        }
        for (issue, metadata, history) in projection_updates {
            self.project_history_head(&issue, metadata, &history)?;
        }
        let advanced = cursor.is_some_and(|cursor| previous_cursor.is_none_or(|old| cursor > old));
        self.cache.replace_github_cache_batch(
            &repository,
            &synchronized,
            &removed_item_ids,
            cursor,
            (!advanced).then_some(response_etag.as_deref()).flatten(),
        )
    }

    fn load_pending_issue(
        &self,
        issue_number: i64,
        event_id: &str,
        creation_fingerprint: &str,
        status: Status,
    ) -> Result<LedgerIssue> {
        let issue: LedgerIssue = self.github.api_json(
            "GET",
            &format!("repos/{}/issues/{issue_number}", self.repository),
            &[],
        )?;
        validate_pending_issue(&issue, event_id, creation_fingerprint, status)?;
        Ok(issue)
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

    fn find_pending_issue_by_fingerprint(
        &self,
        fingerprint: &str,
        status: Status,
    ) -> Result<Option<(LedgerIssue, ProjectionMetadata)>> {
        self.find_pending_issue_by(status, fingerprint, |metadata| {
            metadata.pending_genesis_event_id.as_deref() == Some(metadata.event_id.as_str())
                && metadata.creation_fingerprint == fingerprint
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
                validate_status_label(&issue, status)?;
                if found.is_some() {
                    return Err(GitHubError::new(
                        "github_metadata_collision",
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
                    "github_metadata_collision",
                    format!("issue #{issue_number} contains invalid Work Tracker event metadata"),
                )
            })?;
            if event.event_id == expected.event_id {
                let mut expected = expected.clone();
                expected.github_actor = event.github_actor.clone();
                if event != expected || event.github_actor != comment.user.login {
                    return Err(GitHubError::new(
                        "github_metadata_collision",
                        format!(
                            "issue #{issue_number} genesis event {} does not match the pending creation",
                            expected.event_id
                        ),
                    )
                    .into());
                }
                if found.is_some() {
                    return Err(GitHubError::new(
                        "github_metadata_collision",
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
                "github_metadata_collision",
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
        let visible = projection_visible_text(&issue.body)?;
        let body = projection_body(Some(visible), &metadata)?;
        self.github.api_empty(
            "PATCH",
            &format!("repos/{}/issues/{}", self.repository, issue.number),
            &[("body", body.as_str())],
        )
    }

    fn append_note(
        &mut self,
        issue_number: i64,
        message: &str,
        actor: &str,
        requested_event_id: Option<&str>,
    ) -> Result<HistoryEntry> {
        let message = normalized_required(message, "message")?;
        let actor = normalized_required(actor, "actor")?;
        let event_id = requested_event_id
            .map(|value| normalized_required(value, "event ID"))
            .transpose()?
            .unwrap_or_else(|| new_event_id("note"));
        let github_actor = self.github.authenticated_user()?;
        let (issue, metadata) = self.load_work_item_issue(issue_number)?;
        if status_from_issue(&issue)? == Status::Archived {
            bail!("work item {issue_number} is archived and cannot be modified");
        }
        let comments = self.load_comments(issue_number)?;
        let history = replay_trusted_history(issue_number, &comments)?;
        let legacy_genesis = self.cache.legacy_github_genesis_evidence(issue_number)?;
        projection_head_needs_update(issue_number, &metadata, &history, legacy_genesis.as_ref())?;
        let event = CanonicalEvent {
            schema_version: 1,
            event_id: event_id.clone(),
            kind: "noted".to_owned(),
            actor,
            github_actor,
            note: Some(message),
            changes: json!({}),
        };
        if let Some(existing) = history
            .iter()
            .find(|entry| entry.event_id.as_deref() == Some(event_id.as_str()))
        {
            validate_retry(existing, &event)?;
            self.project_history_head(&issue, metadata, &history)?;
            self.cache.replace_github_history(issue_number, &history)?;
            return Ok(existing.clone());
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
        let history = replay_trusted_history(issue_number, &comments)?;
        projection_head_needs_update(issue_number, &metadata, &history, legacy_genesis.as_ref())?;
        let entry = history
            .iter()
            .find(|entry| entry.event_id.as_deref() == Some(event_id.as_str()))
            .cloned()
            .context("published note was not reconstructed from GitHub")?;
        self.project_history_head(&issue, metadata, &history)?;
        self.cache.replace_github_history(issue_number, &history)?;
        Ok(entry)
    }
}

impl Ledger for GitHubLedger {
    fn create(
        &mut self,
        title: &str,
        description: Option<&str>,
        status: Status,
        actor: &str,
        note: Option<&str>,
    ) -> Result<WorkItem> {
        self.create_work_item(title, description, status, actor, note)
    }

    fn get(&self, id: i64) -> Result<WorkItem> {
        self.cache.get(id)
    }

    fn list(
        &mut self,
        filter: ListFilter,
        include_archived: bool,
        limit: usize,
    ) -> Result<Vec<WorkItem>> {
        self.synchronize()?;
        self.cache.list(filter, include_archived, limit)
    }

    fn daily_view(&mut self, include_archived: bool) -> Result<Vec<WorkItem>> {
        self.synchronize()?;
        self.cache.daily_view(include_archived)
    }

    fn update(
        &mut self,
        _id: i64,
        _title: Option<&str>,
        _description: Option<Option<&str>>,
        _actor: &str,
        _note: Option<&str>,
    ) -> Result<WorkItem> {
        bail!("GitHub-backed update is not implemented yet")
    }

    fn set_status(
        &mut self,
        _id: i64,
        _status: Status,
        _actor: &str,
        _note: Option<&str>,
    ) -> Result<WorkItem> {
        bail!("GitHub-backed Status mutation is not implemented yet")
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
        self.synchronize()?;
        self.cache.history(id)
    }
}

impl GitHub {
    pub fn new() -> Self {
        Self {
            executable: "gh".to_owned(),
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
                            .contains("already exists")
                            || failure.stderr.to_ascii_lowercase().contains("http 422") =>
                    {
                        return Err(GitHubError::new(
                            "github_permission_denied",
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
                "github_invalid_visibility",
                format!("GitHub repository {} is not private", repository.full_name),
            )
            .into());
        }
        if !repository.has_issues {
            return Err(GitHubError::new(
                "github_incompatible_repository",
                format!(
                    "GitHub repository {} does not have issues enabled",
                    repository.full_name
                ),
            )
            .into());
        }
        if !(repository.permissions.admin || repository.permissions.push) {
            return Err(GitHubError::new(
                "github_permission_denied",
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
                    "github_permission_denied",
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
                "github_incompatible_repository",
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
                "github_incompatible_repository",
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
                "github_incompatible_repository",
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
            let kind = if lower.contains("http 404") || lower.contains("not found") {
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
    let code = match failure.kind {
        GhFailureKind::MissingCli => "github_cli_missing",
        GhFailureKind::Unauthenticated => "github_unauthenticated",
        GhFailureKind::PermissionDenied => "github_permission_denied",
        GhFailureKind::NotFound | GhFailureKind::Other => "github_api_failure",
    };
    GitHubError::new(
        code,
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
            "github_unknown_event_schema",
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
        kind: legacy.kind,
        actor: legacy.actor,
        github_actor: legacy.github_actor,
        note: legacy.note,
        changes: json!({
            "title": legacy.initial_values.title,
            "description": legacy.initial_values.description,
            "status": legacy.initial_values.status,
        }),
    })
}

fn materialize_item(issue_number: i64, history: &[HistoryEntry]) -> Result<WorkItem> {
    let genesis = history
        .first()
        .context("Work Tracker history has no genesis entry")?;
    let values: GenesisValues =
        serde_json::from_value(genesis.changes.clone()).context("invalid genesis changes")?;
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

fn event_comment_body(event: &CanonicalEvent) -> Result<String> {
    let note = event
        .note
        .as_deref()
        .map(|note| format!("\n\nNote: {note}"))
        .unwrap_or_default();
    Ok(format!(
        "Work Tracker History Entry: {} by {}{note}\n\n<!-- {EVENT_MARKER}\n{}\n-->",
        event.kind,
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

fn hash_part(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

fn replay_history(issue_number: i64, comments: &[LedgerComment]) -> Result<Vec<HistoryEntry>> {
    let mut comments = comments.to_vec();
    comments.sort_by_key(|comment| comment.id);
    let mut history = Vec::new();
    let mut accepted = HashMap::<String, CanonicalEvent>::new();
    let mut previous_hash: Option<String> = None;
    let mut state_revision = 0_u64;
    for comment in comments {
        if !has_metadata(&comment.body, EVENT_MARKER) {
            continue;
        }
        let event = parse_event(&comment.body)?;
        if event.github_actor != comment.user.login {
            return Err(metadata_collision(issue_number).into());
        }
        if let Some(prior) = accepted.get(&event.event_id) {
            if prior == &event {
                continue;
            }
            return Err(metadata_collision(issue_number).into());
        }
        match event.kind.as_str() {
            "created" if history.is_empty() => {
                if !event.changes.is_object() {
                    return Err(metadata_collision(issue_number).into());
                }
                state_revision = 1;
            }
            "noted" if !history.is_empty() => {
                if event.note.is_none() || event.changes != json!({}) {
                    return Err(metadata_collision(issue_number).into());
                }
            }
            _ => return Err(metadata_collision(issue_number).into()),
        }
        let current_hash = history_hash(previous_hash.as_deref(), &event, comment.id)?;
        history.push(HistoryEntry {
            id: comment.id,
            work_item_id: issue_number,
            event_id: Some(event.event_id.clone()),
            kind: event.kind.clone(),
            actor: event.actor.clone(),
            github_actor: Some(event.github_actor.clone()),
            note: event.note.clone(),
            occurred_at: comment.created_at,
            changes: event.changes.clone(),
            previous_history_hash: previous_hash.clone(),
            history_hash: Some(current_hash.clone()),
            state_revision: Some(state_revision),
        });
        previous_hash = Some(current_hash);
        accepted.insert(event.event_id.clone(), event);
    }
    if history.first().is_none_or(|entry| entry.kind != "created") {
        return Err(metadata_collision(issue_number).into());
    }
    Ok(history)
}

fn replay_trusted_history(
    issue_number: i64,
    comments: &[LedgerComment],
) -> Result<Vec<HistoryEntry>> {
    replay_history(issue_number, comments).map_err(|error| {
        if error
            .downcast_ref::<GitHubError>()
            .is_some_and(|error| error.code() == "github_unknown_event_schema")
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
        || genesis.kind != "created"
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
    if entry.kind != event.kind
        || entry.actor != event.actor
        || entry.github_actor.as_deref() != Some(event.github_actor.as_str())
        || entry.note != event.note
        || entry.changes != event.changes
    {
        return Err(GitHubError::new(
            "github_metadata_collision",
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
    status_from_issue(issue)?;
    Ok(metadata)
}

fn status_from_issue(issue: &LedgerIssue) -> Result<Status> {
    let statuses = issue
        .labels
        .iter()
        .filter_map(|label| {
            LABELS[1..]
                .iter()
                .find(|(name, _, _)| label.name.eq_ignore_ascii_case(name))
                .map(|(name, _, _)| name.trim_start_matches("work-tracker:status:"))
        })
        .collect::<Vec<_>>();
    if statuses.len() != 1 {
        return Err(metadata_collision(issue.number).into());
    }
    statuses[0]
        .parse()
        .map_err(|_| metadata_collision(issue.number).into())
}

fn validate_pending_issue(
    issue: &LedgerIssue,
    event_id: &str,
    creation_fingerprint: &str,
    status: Status,
) -> Result<()> {
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
    validate_status_label(issue, status)
}

fn has_valid_projection_stage(metadata: &ProjectionMetadata) -> bool {
    (metadata.pending_genesis_event_id.as_deref() == Some(metadata.event_id.as_str())
        && metadata.genesis_comment_id.is_none()
        && metadata.state_revision == 0)
        || (metadata.pending_genesis_event_id.is_none()
            && metadata.genesis_comment_id.is_some()
            && metadata.state_revision == 1)
}

fn validate_status_label(issue: &LedgerIssue, status: Status) -> Result<()> {
    let expected = format!("work-tracker:status:{}", status.as_str());
    let labels = issue.labels.iter().filter(|label| {
        LABELS[1..]
            .iter()
            .any(|(name, _, _)| label.name.eq_ignore_ascii_case(name))
    });
    let names = labels.map(|label| label.name.as_str()).collect::<Vec<_>>();
    if names.len() != 1 || !names[0].eq_ignore_ascii_case(&expected) {
        return Err(metadata_collision(issue.number).into());
    }
    Ok(())
}

fn metadata_collision(issue_number: i64) -> GitHubError {
    GitHubError::new(
        "github_metadata_collision",
        format!("GitHub issue #{issue_number} has conflicting Work Tracker metadata"),
    )
}

fn ledger_integrity_error(issue_number: i64, detail: impl fmt::Display) -> GitHubError {
    GitHubError::new(
        "github_ledger_integrity",
        format!("Ledger Integrity Error for GitHub issue #{issue_number}: {detail}"),
    )
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
                    "github_incompatible_repository",
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
                "github_incompatible_repository",
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
            kind: "noted".to_owned(),
            actor: "agent-a".to_owned(),
            github_actor: "octocat".to_owned(),
            note: Some("known".to_owned()),
            changes: json!({}),
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
