use anyhow::Result;
use chrono::Local;
use serde::Serialize;

use crate::domain::{HistoryEntry, WorkItem};

pub fn print_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
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
