use std::fmt::Write;

use anyhow::Result;
use chrono::Local;
use serde::Serialize;

use crate::domain::{HistoryEntry, WorkItem};
use crate::todo::{TodoView, seoul_offset};

pub fn print_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

pub fn print_item(item: &WorkItem) {
    println!("ID:          {}", item.id);
    println!("Status:      {}", item.status);
    println!("Title:       {}", item.title);
    println!("Priority:    {}", item.schedule.priority);
    if let Some(date) = item.schedule.planned_date {
        println!("Planned:     {date}");
    }
    if let Some(date) = item.schedule.due_date {
        println!("Due:         {date}");
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
    if let Some(purge_after) = item.purge_after {
        println!(
            "Purge after: {}",
            purge_after
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
        println!(
            "{}  {:<14} by {}",
            entry
                .occurred_at
                .with_timezone(&Local)
                .format("%Y-%m-%d %H:%M:%S %:z"),
            entry.kind,
            entry.actor
        );
        if let Some(note) = &entry.note {
            println!("  Note: {note}");
        }
        if entry.changes != serde_json::json!({}) {
            println!("  Changes: {}", entry.changes);
        }
    }
}

pub fn format_todo(view: &TodoView) -> String {
    let mut text = format!(
        "Todo: {} through {} ({}; {} calendar day(s), including today)\n",
        view.window.start, view.window.end, view.window.timezone, view.window.days
    );
    if view.actions.is_empty() && view.blocked_waiting.is_empty() {
        text.push_str("No scheduled work matches this window.\n");
        return text;
    }
    for (name, rows) in [
        ("Actions", &view.actions),
        ("Blocked / waiting", &view.blocked_waiting),
    ] {
        if rows.is_empty() {
            continue;
        }
        writeln!(text, "\n{name}").unwrap();
        writeln!(
            text,
            "{:<7} {:<8} {:<8} {:<10} {:<10} {:<16} TITLE",
            "ID", "PRIORITY", "STATUS", "DUE", "PLANNED", "UPDATED (KST)"
        )
        .unwrap();
        for row in rows {
            let item = &row.item;
            writeln!(
                text,
                "{:<7} {:<8} {:<8} {:<10} {:<10} {} {}{}{}",
                item.id,
                item.schedule.priority,
                item.status,
                item.schedule
                    .due_date
                    .map(|d| d.to_string())
                    .unwrap_or_else(|| "-".into()),
                item.schedule
                    .planned_date
                    .map(|d| d.to_string())
                    .unwrap_or_else(|| "-".into()),
                item.updated_at
                    .with_timezone(&seoul_offset())
                    .format("%Y-%m-%d %H:%M"),
                item.title.replace(['\n', '\r', '\t'], " "),
                if row.overdue { " [overdue]" } else { "" },
                if row.carried_over {
                    " [carried over]"
                } else {
                    ""
                },
            )
            .unwrap();
        }
    }
    text.push_str("\nUpdated refers to the local ledger; external status is not refreshed.\n");
    text
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
