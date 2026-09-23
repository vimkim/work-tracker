use anyhow::{Error, Result};
use chrono::{Local, Utc};
use serde::Serialize;
use serde_json::json;

use crate::{
    domain::{HistoryEntry, WorkItem},
    github::{GitHubError, GitHubErrorKind},
    ledger::{ReadHealth, ReadHealthError, ReadHealthErrorKind, ReadHealthKind},
};

pub fn print_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

pub fn print_read_warning(health: &ReadHealth, json_output: bool) {
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
        GitHubErrorKind::ApiFailure => "github_api_failure",
        GitHubErrorKind::InvalidVisibility => "github_invalid_visibility",
        GitHubErrorKind::IncompatibleRepository => "github_incompatible_repository",
        GitHubErrorKind::StateConflict => "github_state_conflict",
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
