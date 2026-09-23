use std::path::Path;

use anyhow::{Error, Result};
use chrono::{Local, Utc};
use serde::Serialize;
use serde_json::json;

use crate::{
    domain::{
        DomainValidationError, HistoryEntry, IntegrityDoctorReport, RecoveryReport,
        RejectedMutation, WorkItem,
    },
    github::{GitHubError, GitHubErrorKind, RepositoryName},
    ledger::{ReadHealth, ReadHealthError, ReadHealthErrorKind, ReadHealthKind},
};

pub fn print_cli_error(error: &clap::Error, json_output: bool) {
    if json_output && error.use_stderr() {
        eprintln!(
            "{}",
            json!({"error": {
                "code": "cli_validation_failed",
                "message": error.to_string(),
            }})
        );
    } else if let Err(print_error) = error.print() {
        eprintln!("error: failed to print command-line diagnostic: {print_error}");
    }
}

pub fn print_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

pub fn print_sqlite_path(database: &Path, json_output: bool) -> Result<()> {
    if json_output {
        print_json(&json!({"backend": "sqlite", "database": database}))
    } else {
        println!("{}", database.display());
        Ok(())
    }
}

pub fn print_github_path(
    repository: &RepositoryName,
    cache: &Path,
    json_output: bool,
) -> Result<()> {
    if json_output {
        print_json(&json!({
            "backend": "github",
            "repository": repository,
            "cache": cache,
            "database": cache,
        }))
    } else {
        println!("Repository: {repository}");
        println!("Cache:      {}", cache.display());
        Ok(())
    }
}

pub fn print_github_initialization(
    repository: &RepositoryName,
    private: bool,
    created: bool,
    is_default: bool,
    cache: &Path,
    json_output: bool,
) -> Result<()> {
    if json_output {
        print_json(&json!({
            "backend": "github",
            "repository": repository,
            "private": private,
            "created": created,
            "default": is_default,
            "cache": cache,
        }))
    } else {
        println!("GitHub ledger: {repository}");
        println!("Private:       {}", if private { "yes" } else { "no" });
        println!(
            "Default:       {}",
            if is_default { "yes" } else { "no (override)" }
        );
        println!("Cache:         {}", cache.display());
        Ok(())
    }
}

pub fn print_read_warning(health: &ReadHealth, json_output: bool) {
    if !health.repaired_work_item_ids().is_empty() {
        print_projection_repair_warning(health.repaired_work_item_ids(), json_output);
        return;
    }
    let (code, label) = match health.kind() {
        ReadHealthKind::Stale => (
            if health.is_offline() {
                "offline_cache"
            } else {
                "stale_cache"
            },
            "STALE CACHE",
        ),
        ReadHealthKind::Integrity => ("ledger_integrity", "LEDGER INTEGRITY ERROR"),
        ReadHealthKind::Local | ReadHealthKind::Fresh | ReadHealthKind::Unavailable => return,
    };
    let mut message = label.to_owned();
    if let Some(synchronized_at) = health.last_successful_sync_at() {
        message.push_str(&format!(
            ": last successful synchronization was {}",
            synchronized_at.to_rfc3339()
        ));
    }
    if let Some(reason) = health.reason() {
        message.push_str(&format!(": {reason}"));
    }
    let stale_age_seconds = health
        .last_successful_sync_at()
        .map(|time| Utc::now().signed_duration_since(*time).num_seconds().max(0));
    if json_output {
        eprintln!(
            "{}",
            json!({
                "warning": {
                    "code": code,
                    "message": message,
                    "last_successful_sync_at": health.last_successful_sync_at(),
                    "stale_age_seconds": stale_age_seconds,
                }
            })
        );
    } else {
        eprintln!("warning: {message}");
    }
}

pub fn print_projection_repair_warning(work_item_ids: &[i64], json_output: bool) {
    if work_item_ids.is_empty() {
        return;
    }
    let message = format!(
        "GITHUB PROJECTION REPAIRED: restored Work Items {:?} from accepted history after unsupported direct edits or interrupted writes",
        work_item_ids
    );
    if json_output {
        eprintln!(
            "{}",
            json!({
                "warning": {
                    "code": "github_projection_repaired",
                    "message": message,
                    "work_item_ids": work_item_ids,
                }
            })
        );
    } else {
        eprintln!("warning: {message}");
    }
}

pub fn print_error(error: &Error, json_output: bool) {
    if !json_output {
        eprintln!("error: {error:#}");
        return;
    }
    if let Some(github_error) = error.downcast_ref::<GitHubError>() {
        let mut diagnostic = json!({
            "code": github_error_code(github_error.kind()),
            "message": format!("{error:#}"),
        });
        if let (Some(target), Some(details)) = (diagnostic.as_object_mut(), github_error.details())
            && let Some(details) = details.as_object()
        {
            target.extend(details.clone());
        }
        eprintln!("{}", json!({"error": diagnostic}));
    } else if let Some(read_error) = error.downcast_ref::<ReadHealthError>() {
        eprintln!(
            "{}",
            json!({"error": {
                "code": read_health_error_code(read_error.kind()),
                "message": format!("{error:#}"),
            }})
        );
    } else if error.downcast_ref::<DomainValidationError>().is_some() {
        eprintln!(
            "{}",
            json!({"error": {
                "code": "domain_validation_failed",
                "message": format!("{error:#}"),
            }})
        );
    } else {
        eprintln!("{}", json!({"error": format!("{error:#}")}));
    }
}

fn github_error_code(kind: GitHubErrorKind) -> &'static str {
    if let Some(read_error_kind) = kind.read_health_error_kind() {
        return read_health_error_code(read_error_kind);
    }
    match kind {
        GitHubErrorKind::CliMissing => "github_cli_missing",
        GitHubErrorKind::Unauthenticated => "github_unauthenticated",
        GitHubErrorKind::PermissionDenied => "github_permission_denied",
        GitHubErrorKind::NotFound => "github_not_found",
        GitHubErrorKind::ValidationFailed => "github_validation_failed",
        GitHubErrorKind::RateLimited => "github_rate_limited",
        GitHubErrorKind::NetworkFailure => "github_network_failure",
        GitHubErrorKind::ServiceFailure => "github_service_failure",
        GitHubErrorKind::ApiFailure => "github_api_failure",
        GitHubErrorKind::InvalidVisibility => "github_invalid_visibility",
        GitHubErrorKind::IncompatibleRepository => "github_incompatible_repository",
        GitHubErrorKind::RejectedMutation => "github_rejected_mutation",
        GitHubErrorKind::ProjectionPending => "github_projection_pending",
        GitHubErrorKind::ArchivedImmutable => "github_archived_immutable",
        GitHubErrorKind::RecoveryValidationFailed => "github_recovery_validation_failed",
        GitHubErrorKind::RecoveryStillBlocked => "github_recovery_still_blocked",
        GitHubErrorKind::LedgerIntegrity
        | GitHubErrorKind::UnknownEventSchema
        | GitHubErrorKind::IncompatibleMetadata
        | GitHubErrorKind::MetadataCollision => unreachable!("handled as a read-health error kind"),
    }
}

fn read_health_error_code(kind: ReadHealthErrorKind) -> &'static str {
    match kind {
        ReadHealthErrorKind::CacheUnavailable => "cache_unavailable",
        ReadHealthErrorKind::LedgerIntegrity => "github_ledger_integrity",
        ReadHealthErrorKind::MetadataCollision => "github_metadata_collision",
        ReadHealthErrorKind::IncompatibleMetadata => "github_incompatible_metadata",
        ReadHealthErrorKind::UnknownEventSchema => "github_unknown_event_schema",
    }
}

pub fn print_item(item: &WorkItem) {
    println!("ID:          {}", item.id);
    println!("Status:      {}", item.status);
    println!("Title:       {}", item.title);
    if item.ledger_integrity_error {
        println!("Integrity:   Ledger Integrity Error");
    }
    if let Some(description) = &item.description {
        println!("Description: {description}");
    }
    println!(
        "Created:     {}",
        item.created_at
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M:%S %:z")
    );
    println!(
        "Updated:     {}",
        item.updated_at
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M:%S %:z")
    );
    if let Some(archived_at) = item.archived_at {
        println!(
            "Archived:    {}",
            archived_at
                .with_timezone(&Local)
                .format("%Y-%m-%d %H:%M:%S %:z")
        );
    }
}

pub fn print_items(items: &[WorkItem]) {
    if items.is_empty() {
        println!("No work items.");
        return;
    }
    print_table(items);
}

/// Prints the default `list` view and tells the reader how to widen it.
pub fn print_actionable_items(items: &[WorkItem]) {
    if items.is_empty() {
        println!("No actionable work items. Use --all to include done and cancelled.");
        return;
    }
    print_table(items);
    println!("{}", actionable_footer(items.len()));
}

fn actionable_footer(count: usize) -> String {
    let noun = if count == 1 { "item" } else { "items" };
    format!("Showing {count} actionable work {noun}. Use --all to include done and cancelled.")
}

fn print_table(items: &[WorkItem]) {
    println!("{:<7} {:<10} {:<17} TITLE", "ID", "STATUS", "UPDATED");
    for item in items {
        println!(
            "{:<7} {:<10} {:<17} {}",
            item.id,
            item.status,
            item.updated_at
                .with_timezone(&Local)
                .format("%Y-%m-%d %H:%M"),
            item.title
        );
    }
}

pub fn print_history(entries: &[HistoryEntry]) {
    if entries.is_empty() {
        println!("No history entries.");
        return;
    }
    for entry in entries {
        let attribution = entry
            .github_actor
            .as_deref()
            .map(|github_actor| format!("{} (GitHub: {github_actor})", entry.actor))
            .unwrap_or_else(|| entry.actor.clone());
        println!(
            "{}  {:<14} by {}",
            entry
                .occurred_at
                .with_timezone(&Local)
                .format("%Y-%m-%d %H:%M:%S %:z"),
            entry.kind,
            attribution
        );
        if let Some(event_id) = &entry.event_id {
            println!("  Event: {event_id}");
        }
        if let Some(note) = &entry.note {
            println!("  Note: {note}");
        }
        if entry.changes != serde_json::json!({}) {
            println!("  Changes: {}", entry.changes);
        }
        if let Some(state_revision) = entry.state_revision {
            println!("  State revision: {state_revision}");
        }
        if let Some(history_hash) = &entry.history_hash {
            println!("  History hash: {history_hash}");
        }
        println!("  Trust: {}", entry.trust.as_str());
    }
}

pub fn print_doctor_report(report: &IntegrityDoctorReport) {
    println!("Work Item:   {}", report.work_item_id);
    println!("Integrity health: {}", report.integrity_health.as_str());
    println!("Archived:    {}", report.archived);
    if let Some(first_break) = &report.first_break {
        println!("First break: {:?}", first_break.kind);
        if let Some(comment_id) = first_break.github_comment_id {
            println!("  GitHub comment: {comment_id}");
        }
        if let Some(event_id) = &first_break.event_id {
            println!("  Event: {event_id}");
        }
        if let Some(github_actor) = &first_break.github_actor {
            println!("  GitHub Actor: {github_actor}");
        }
        println!(
            "  Expected hash: {}",
            first_break
                .expected_hash
                .as_deref()
                .unwrap_or("unavailable")
        );
        println!(
            "  Observed hash: {}",
            first_break
                .observed_hash
                .as_deref()
                .unwrap_or("unavailable")
        );
        println!(
            "  Cached exact copy: {}",
            first_break
                .cached_exact_copy
                .as_deref()
                .unwrap_or("unavailable")
        );
        println!(
            "  Observed copy: {}",
            first_break
                .observed_copy
                .as_deref()
                .unwrap_or("unavailable")
        );
        println!("  Detail: {}", first_break.detail);
    }
    println!(
        "Evidence:    {} trusted, {} untrusted",
        report.trusted_event_count, report.untrusted_event_count
    );
    let repair_modes = report
        .eligible_repair_modes
        .iter()
        .map(|mode| mode.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    println!("Timeline evidence: {}", json!(report.timeline_evidence));
    println!("Repair modes: {repair_modes}");
}

pub fn print_recovery_report(report: &RecoveryReport) {
    println!("Work Item:   {}", report.work_item_id);
    println!("Recovery:    {}", report.outcome.as_str());
    println!("Archived:    {}", report.archived);
    println!(
        "Validation:  {}",
        if report.full_history_revalidated {
            "full history revalidated"
        } else {
            "new sequence validated"
        }
    );
    println!("Projection:  rebuilt");
    println!(
        "Evidence:    {} trusted, {} untrusted",
        report.trusted_event_count, report.untrusted_event_count
    );
}

pub fn print_rejected_mutations(rejected: &[RejectedMutation]) {
    if rejected.is_empty() {
        println!("No Rejected Mutations.");
        return;
    }
    for mutation in rejected {
        println!(
            "{}  Rejected Mutation by {} (GitHub: {})",
            mutation
                .occurred_at
                .with_timezone(&Local)
                .format("%Y-%m-%d %H:%M:%S %:z"),
            mutation.actor,
            mutation.github_actor,
        );
        println!("  Event: {}", mutation.event_id);
        println!(
            "  State revision: expected {:?}, current {}",
            mutation.expected_state_revision, mutation.current_state_revision
        );
        println!("  Reason: stale State Revision");
        println!("  Changes: {}", mutation.changes);
        if let Some(note) = &mutation.note {
            println!("  Note: {note}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actionable_footer_names_the_all_flag_and_agrees_in_number() {
        assert_eq!(
            actionable_footer(1),
            "Showing 1 actionable work item. Use --all to include done and cancelled."
        );
        assert_eq!(
            actionable_footer(10),
            "Showing 10 actionable work items. Use --all to include done and cancelled."
        );
    }
}
