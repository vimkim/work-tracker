use std::{cell::RefCell, collections::HashMap, fmt, process::Command, str::FromStr};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

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

#[derive(Debug, Deserialize)]
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

#[derive(Debug, Deserialize)]
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

impl Default for GitHub {
    fn default() -> Self {
        Self::new()
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
        let mut command = Command::new(&self.executable);
        command.args(["api", "--method", method, endpoint]);
        if paginate {
            command.args(["--paginate", "--slurp"]);
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
