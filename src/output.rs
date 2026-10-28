use std::{fmt::Write, io::IsTerminal};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local};
use serde::Serialize;
use serde_json::Value;

use crate::domain::{HistoryEntry, Priority, Status, WorkItem};
use crate::todo::{TodoView, seoul_offset};

#[derive(Debug, Default, Clone, Copy, clap::ValueEnum)]
pub enum ColorMode {
    #[default]
    Auto,
    Always,
    Never,
}

impl ColorMode {
    pub fn resolve(self, json: bool) -> Colors {
        Colors(self.enabled(
            json,
            std::io::stdout().is_terminal(),
            std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty()),
            std::env::var_os("TERM").is_some_and(|value| value == "dumb"),
        ))
    }

    fn enabled(self, json: bool, terminal: bool, no_color: bool, dumb: bool) -> bool {
        !json
            && match self {
                Self::Auto => terminal && !no_color && !dumb,
                Self::Always => true,
                Self::Never => false,
            }
    }
}

/// Styles already-padded cells so escape sequences do not affect column widths.
#[derive(Debug, Default, Clone, Copy)]
pub struct Colors(bool);

impl Colors {
    fn paint(self, text: impl std::fmt::Display, style: &str) -> String {
        if self.0 {
            format!("\x1b[{style}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }

    fn status(self, status: Status, width: usize) -> String {
        let text = format!("{status:<width$}");
        match status {
            Status::Active | Status::Done => self.paint(text, "32"),
            Status::Waiting => self.paint(text, "33"),
            Status::Blocked => self.paint(text, "31"),
            Status::Cancelled | Status::Deleted => self.paint(text, "2"),
            Status::Pending => text,
        }
    }

    fn priority(self, priority: Priority, width: usize) -> String {
        let text = format!("{priority:<width$}");
        match priority {
            Priority::High => self.paint(text, "1;35"),
            Priority::Low => self.paint(text, "2"),
            Priority::Normal => text,
        }
    }
}

pub fn print_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

// This is the rendering order, not a substitute for inspecting the live schema.
// Both the schema and each serialized item must match these rendered fields.
const ITEM_FIELDS: &[(&str, &str)] = &[
    ("id", "ID:"),
    ("status", "Status:"),
    ("title", "Title:"),
    ("workdir", "Work dir:"),
    ("priority", "Priority:"),
    ("planned_date", "Planned:"),
    ("due_date", "Due:"),
    ("description", "Description:"),
    ("created_at", "Created:"),
    ("updated_at", "Updated:"),
    ("deleted_at", "Deleted:"),
    ("purge_after", "Purge after:"),
];

pub fn item_field_names() -> impl Iterator<Item = &'static str> {
    ITEM_FIELDS.iter().map(|(name, _)| *name)
}

/// Validate the actual serializer, so adding a label alone cannot satisfy coverage.
pub fn item_json(item: &WorkItem) -> Result<Value> {
    let value = serde_json::to_value(item)?;
    check_item_fields(&value)?;
    Ok(value)
}

fn check_item_fields(value: &Value) -> Result<()> {
    let fields = value
        .as_object()
        .context("Work Item must serialize to an object")?;
    let missing: Vec<_> = item_field_names()
        .filter(|name| !fields.contains_key(*name))
        .collect();
    let unknown: Vec<_> = fields
        .keys()
        .filter(|name| !item_field_names().any(|field| field == name.as_str()))
        .collect();
    if !missing.is_empty() || !unknown.is_empty() {
        bail!(
            "Work Item serialization does not match detail fields: missing fields {missing:?}; unknown fields {unknown:?}"
        );
    }
    Ok(())
}

pub fn format_item(item: &WorkItem, colors: Colors) -> Result<String> {
    let fields = item_json(item)?;
    let mut text = String::new();
    for &(name, label) in ITEM_FIELDS {
        let value = &fields[name];
        let rendered = match name {
            "status" => colors.status(item.status, 0),
            "priority" => colors.priority(item.schedule.priority, 0),
            "workdir" => value.as_str().unwrap_or("unknown").to_owned(),
            "created_at" | "updated_at" | "deleted_at" | "purge_after" if !value.is_null() => {
                DateTime::parse_from_rfc3339(
                    value
                        .as_str()
                        .context("Work Item timestamp must be a string")?,
                )?
                .with_timezone(&Local)
                .format("%Y-%m-%d %H:%M:%S%.f %:z")
                .to_string()
            }
            _ if value.is_null() => "-".to_owned(),
            _ => value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string()),
        };
        writeln!(text, "{label:<12} {rendered}").unwrap();
    }
    Ok(text)
}

pub fn print_item(item: &WorkItem, colors: Colors) -> Result<()> {
    print!("{}", format_item(item, colors)?);
    Ok(())
}

pub fn print_item_details(items: &[WorkItem], actionable: bool, colors: Colors) -> Result<()> {
    if items.is_empty() {
        if actionable {
            print_actionable_items(items, colors);
        } else {
            print_items(items, colors);
        }
        return Ok(());
    }
    // Format every item before writing, so validation errors cannot leave partial output.
    let details = items
        .iter()
        .map(|item| format_item(item, colors))
        .collect::<Result<Vec<_>>>()?;
    print!("{}", details.join("\n"));
    if actionable {
        println!("{}", colors.paint(actionable_footer(items.len()), "2"));
    }
    Ok(())
}

pub fn print_items(items: &[WorkItem], colors: Colors) {
    if items.is_empty() {
        println!("No work items.");
        return;
    }
    print_table(items, colors);
}

/// Prints the default `list` view and tells the reader how to widen it.
pub fn print_actionable_items(items: &[WorkItem], colors: Colors) {
    if items.is_empty() {
        println!("No actionable work items. Use --all to include done and cancelled.");
        return;
    }
    print_table(items, colors);
    println!("{}", colors.paint(actionable_footer(items.len()), "2"));
}

fn actionable_footer(count: usize) -> String {
    let noun = if count == 1 { "item" } else { "items" };
    format!("Showing {count} actionable work {noun}. Use --all to include done and cancelled.")
}

fn print_table(items: &[WorkItem], colors: Colors) {
    println!(
        "{}",
        colors.paint(
            format!("{:<7} {:<10} {:<17} TITLE", "ID", "STATUS", "UPDATED"),
            "1"
        )
    );
    for item in items {
        println!(
            "{:<7} {} {} {}",
            item.id,
            colors.status(item.status, 10),
            colors.paint(
                format!(
                    "{:<17}",
                    item.updated_at
                        .with_timezone(&Local)
                        .format("%Y-%m-%d %H:%M")
                ),
                "2"
            ),
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
    format_todo_colored(view, Colors::default())
}

pub fn format_todo_colored(view: &TodoView, colors: Colors) -> String {
    let mut text = format!(
        "Todo: {} through {} ({}; {} calendar day(s), including today)\n",
        view.window.start, view.window.end, view.window.timezone, view.window.days
    );
    text = colors.paint(text.trim_end(), "1") + "\n";
    if view.actions.is_empty() && view.blocked_waiting.is_empty() && view.finished.is_empty() {
        text.push_str("No scheduled work matches this window.\n");
        return text;
    }
    for (name, rows) in [
        ("Actions", &view.actions),
        ("Blocked / waiting", &view.blocked_waiting),
        ("Done / cancelled", &view.finished),
    ] {
        if rows.is_empty() {
            continue;
        }
        writeln!(text, "\n{}", colors.paint(name, "1")).unwrap();
        writeln!(
            text,
            "{}",
            colors.paint(
                format!(
                    "{:<7} {:<8} {:<8} {:<10} {:<10} {:<16} TITLE",
                    "ID", "PRIORITY", "STATUS", "DUE", "PLANNED", "UPDATED (KST)"
                ),
                "1"
            )
        )
        .unwrap();
        for row in rows {
            let item = &row.item;
            writeln!(
                text,
                "{:<7} {} {} {} {:<10} {} {}{}{}",
                item.id,
                colors.priority(item.schedule.priority, 8),
                colors.status(item.status, 8),
                colors.paint(
                    format!(
                        "{:<10}",
                        item.schedule
                            .due_date
                            .map(|d| d.to_string())
                            .unwrap_or_else(|| "-".into())
                    ),
                    if row.overdue { "31" } else { "0" },
                ),
                item.schedule
                    .planned_date
                    .map(|d| d.to_string())
                    .unwrap_or_else(|| "-".into()),
                colors.paint(
                    item.updated_at
                        .with_timezone(&seoul_offset())
                        .format("%Y-%m-%d %H:%M"),
                    "2"
                ),
                item.title.replace(['\n', '\r', '\t'], " "),
                if row.overdue {
                    colors.paint(" [overdue]", "31")
                } else {
                    String::new()
                },
                if row.carried_over {
                    colors.paint(" [carried over]", "33")
                } else {
                    String::new()
                },
            )
            .unwrap();
        }
    }
    writeln!(
        text,
        "\n{}",
        colors.paint(
            "Updated refers to the local ledger; external status is not refreshed.",
            "2"
        )
    )
    .unwrap();
    text
}

pub fn print_snapshot(snapshot: &crate::db::backup::Snapshot, json: bool) -> anyhow::Result<()> {
    if json {
        return print_json(snapshot);
    }
    println!("Snapshot: {}", snapshot.path.display());
    println!("Saved at: {}", snapshot.created_at);
    println!(
        "{} Work Items, {} History Entries, {} bytes; integrity verified",
        snapshot.contents.work_items, snapshot.contents.history_entries, snapshot.size_bytes
    );
    Ok(())
}

pub fn print_snapshots(
    snapshots: &[crate::db::backup::Snapshot],
    json: bool,
) -> anyhow::Result<()> {
    if json {
        return print_json(&snapshots);
    }
    if snapshots.is_empty() {
        println!("No snapshots yet. Run work-tracker backup to save one.");
    }
    for snapshot in snapshots {
        println!(
            "{}  {} items  {} history  {} bytes  {}",
            snapshot.created_at.format("%Y-%m-%d %H:%M:%S UTC"),
            snapshot.contents.work_items,
            snapshot.contents.history_entries,
            snapshot.size_bytes,
            snapshot.path.display()
        );
    }
    Ok(())
}

pub fn print_recovery(recovery: &crate::db::backup::Recovery, json: bool) -> anyhow::Result<()> {
    if json {
        return print_json(recovery);
    }
    println!("Recovered database: {}", recovery.database.display());
    println!("From snapshot: {}", recovery.snapshot.display());
    println!(
        "{} Work Items, {} History Entries; integrity verified",
        recovery.contents.work_items, recovery.contents.history_entries
    );
    println!("Inspect this database with --database PATH, then point clients at it to resume.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialized_fields_must_match_the_fields_rendered_in_details() {
        let mut fields: serde_json::Map<String, Value> = item_field_names()
            .map(|name| (name.to_owned(), Value::Null))
            .collect();
        check_item_fields(&Value::Object(fields.clone())).unwrap();
        fields.remove("description");
        fields.insert("unrendered_context".to_owned(), Value::Null);
        let error = check_item_fields(&Value::Object(fields))
            .unwrap_err()
            .to_string();
        assert!(error.contains("missing fields [\"description\"]"));
        assert!(error.contains("unknown fields [\"unrendered_context\"]"));
        assert!(check_item_fields(&Value::Null).is_err());
    }

    #[test]
    fn color_policy_keeps_machine_output_plain_and_allows_explicit_overrides() {
        for terminal in [false, true] {
            for no_color in [false, true] {
                for dumb in [false, true] {
                    assert_eq!(
                        ColorMode::Auto.enabled(false, terminal, no_color, dumb),
                        terminal && !no_color && !dumb
                    );
                    assert!(ColorMode::Always.enabled(false, terminal, no_color, dumb));
                    assert!(!ColorMode::Never.enabled(false, terminal, no_color, dumb));
                    for mode in [ColorMode::Auto, ColorMode::Always, ColorMode::Never] {
                        assert!(!mode.enabled(true, terminal, no_color, dumb));
                    }
                }
            }
        }
    }

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
