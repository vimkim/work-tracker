#![deny(clippy::print_stdout)]

use std::{
    io::{self, Read as _, Write as _},
    path::{Path, PathBuf},
    process::ExitCode,
};

use anyhow::Result;
use clap::Parser;
use serde_json::json;
use work_tracker::{
    brief::{self, Brief, BriefItem},
    cli::{self, Cli, Command, DepCommand, HookEvent, ListArgs},
    db::{ListFilter, NewWorkItem, Tracker, WorkDirectoryFilter, WorkItemUpdate, backup},
    domain::Status,
    output,
    todo::TodoWindow,
    web,
};

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Command::Hook(args) = &cli.command {
        run_hook(cli.database.clone(), args.event);
        return ExitCode::SUCCESS;
    }
    let json_output = cli.json;
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        // A reader that closes stdout early (`| head`) is the consumer's choice. Failing here
        // would make an agent retry a mutation that already committed.
        Err(error) if is_broken_pipe(&error) => ExitCode::SUCCESS,
        Err(error) => {
            if json_output {
                eprintln!("{}", json!({"error": format!("{error:#}")}));
            } else {
                eprintln!("error: {error:#}");
            }
            ExitCode::FAILURE
        }
    }
}

fn is_broken_pipe(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<io::Error>()
            .is_some_and(|io_error| io_error.kind() == io::ErrorKind::BrokenPipe)
    })
}

async fn run(cli: Cli) -> Result<()> {
    let colors = cli.color.resolve(cli.json);
    let database = cli::database_path(cli.database)?;
    match &cli.command {
        Command::Backup(args) => {
            let directory = backup::directory(&database, args.directory.clone());
            let snapshot = backup::create(&database, &directory)?;
            return output::print_snapshot(&snapshot, cli.json);
        }
        Command::Backups(args) => {
            let directory = backup::directory(&database, args.directory.clone());
            return output::print_snapshots(&backup::list(&directory)?, cli.json);
        }
        Command::Doctor(args) => {
            let directory = backup::directory(&database, args.directory.clone());
            let actor = std::env::var("WORK_TRACKER_ACTOR").ok();
            let checks = work_tracker::doctor::run(
                &database,
                &directory,
                actor.as_deref(),
                chrono::Utc::now(),
            );
            output::print_checks(&checks, cli.json, colors)?;
            let failed = checks
                .iter()
                .filter(|check| check.failed_required())
                .count();
            if failed > 0 {
                anyhow::bail!("{failed} required check(s) failed");
            }
            return Ok(());
        }
        Command::Restore(args) => {
            let recovery = backup::restore(&args.snapshot, &args.to)?;
            return output::print_recovery(&recovery, cli.json);
        }
        _ => {}
    }
    cli::prepare_database_path(&database)?;

    if let Command::Serve(args) = &cli.command {
        Tracker::open(&database)?;
        return web::serve(database, &args.bind, args.allow_hosts.clone()).await;
    }
    if matches!(cli.command, Command::Path) {
        if cli.json {
            return output::print_json(&json!({"database": database}));
        }
        writeln!(io::stdout(), "{}", database.display())?;
        return Ok(());
    }

    let mut tracker = Tracker::open(&database)?;
    match cli.command {
        Command::Add(args) => {
            let workdir =
                cli::work_directory(args.workdir.as_deref().unwrap_or(std::path::Path::new(".")))?;
            let item = tracker.create_item_with_blockers(
                NewWorkItem {
                    title: &args.title,
                    description: args.description.as_deref(),
                    status: args.status,
                    schedule: args.schedule.for_creation(),
                    workdir: Some(&workdir),
                },
                &args.blocked_by,
                &args.actor.resolved(),
                args.note.as_deref(),
            )?;
            if cli.json {
                show_item(&item, true, colors)
            } else {
                let deps = tracker.dependencies(item.id)?;
                output::print_item_with_deps(&item, &deps, colors)
            }
        }
        Command::Show(args) => {
            tracker.check_work_item_output_fields(output::item_field_names())?;
            let item = tracker.get(args.id)?;
            if cli.json {
                show_item(&item, true, colors)
            } else {
                let deps = tracker.dependencies(args.id)?;
                output::print_item_with_deps(&item, &deps, colors)
            }
        }
        Command::List(args) => {
            if cli.json || args.details {
                tracker.check_work_item_output_fields(output::item_field_names())?;
            }
            let filter = list_filter(&args);
            let workdir = args
                .workdir
                .as_deref()
                .or_else(|| args.here.then_some(std::path::Path::new(".")))
                .map(cli::work_directory_query)
                .transpose()?;
            let directory = match workdir.as_deref() {
                Some(path) => WorkDirectoryFilter::Exact(path),
                None if args.without_workdir => WorkDirectoryFilter::Unknown,
                None => WorkDirectoryFilter::All,
            };
            let items =
                tracker.list_in_directory(filter, directory, args.include_deleted, args.limit)?;
            if cli.json && args.compact.compact {
                output::print_compact_json(&items)
            } else if cli.json {
                for item in &items {
                    output::item_json(item)?;
                }
                output::print_json(&items)
            } else if args.details {
                let detailed = items
                    .into_iter()
                    .map(|item| {
                        let deps = tracker.dependencies(item.id)?;
                        Ok((item, deps))
                    })
                    .collect::<Result<Vec<_>>>()?;
                output::print_item_details_with_deps(&detailed, filter, colors)
            } else if filter == ListFilter::Actionable {
                output::print_actionable_items(&items, colors)
            } else if filter == ListFilter::Ready {
                output::print_ready_items(&items, colors)
            } else {
                output::print_items(&items, colors)
            }
        }
        Command::Today(args) => {
            let items = tracker.daily_view(args.include_deleted)?;
            if cli.json && args.compact.compact {
                output::print_compact_json(&items)
            } else {
                show_items(&items, cli.json, colors)
            }
        }
        Command::Todo(args) => {
            let window = TodoWindow::new(chrono::Utc::now(), args.days.unwrap_or(1))?;
            let view = tracker.todo_view(window, args.all)?;
            if cli.json {
                output::print_json(&view)
            } else {
                write!(
                    io::stdout(),
                    "{}",
                    output::format_todo_colored(&view, colors)
                )?;
                Ok(())
            }
        }
        Command::Update(args) => {
            let description = if args.clear_description {
                Some(None)
            } else {
                args.description.as_deref().map(Some)
            };
            let workdir = args
                .workdir
                .as_deref()
                .map(cli::work_directory)
                .transpose()?;
            let item = tracker.update_item(
                args.id,
                WorkItemUpdate {
                    title: args.title.as_deref(),
                    description,
                    schedule: args.schedule_update(),
                    workdir: if args.clear_workdir {
                        Some(None)
                    } else {
                        workdir.as_deref().map(Some)
                    },
                },
                &args.actor.resolved(),
                args.note.as_deref(),
            )?;
            show_item(&item, cli.json, colors)
        }
        Command::Status(args) => {
            let transition = tracker.set_status_with_effects(
                args.id,
                args.status,
                &args.actor.resolved(),
                args.note.as_deref(),
            )?;
            if cli.json {
                show_item(&transition.item, true, colors)
            } else {
                let deps = tracker.dependencies(transition.item.id)?;
                output::print_status_transition(&transition, &deps, colors)
            }
        }
        Command::Dep(args) => match args.command {
            DepCommand::Add(edit) => {
                let mutation = tracker.add_dependencies(
                    edit.id,
                    &edit.blocked_by,
                    &edit.actor.resolved(),
                    edit.note.as_deref(),
                )?;
                output::print_dependency_mutation(&mutation, "added", cli.json, colors)
            }
            DepCommand::Remove(edit) => {
                let mutation = tracker.remove_dependencies(
                    edit.id,
                    &edit.blocked_by,
                    &edit.actor.resolved(),
                    edit.note.as_deref(),
                )?;
                output::print_dependency_mutation(&mutation, "removed", cli.json, colors)
            }
            DepCommand::List(id_args) => {
                let deps = tracker.dependencies(id_args.id)?;
                output::print_dependencies(&deps, cli.json, colors)
            }
        },
        Command::Note(args) => {
            let entry = tracker.add_note(args.id, &args.message, &args.actor.resolved())?;
            if cli.json {
                output::print_json(&entry)
            } else {
                output::print_history(&[entry], 0)
            }
        }
        Command::Delete(args) => {
            let transition = tracker.set_status_with_effects(
                args.id,
                Status::Deleted,
                &args.actor.resolved(),
                args.note.as_deref(),
            )?;
            if cli.json {
                show_item(&transition.item, true, colors)
            } else {
                let deps = tracker.dependencies(transition.item.id)?;
                output::print_status_transition(&transition, &deps, colors)
            }
        }
        Command::History(args) => {
            let mut entries = tracker.history(args.id)?;
            let omitted = args
                .limit
                .map_or(0, |limit| entries.len().saturating_sub(limit));
            entries.drain(..omitted);
            if cli.json {
                output::print_json(&entries)
            } else {
                output::print_history(&entries, omitted)
            }
        }
        Command::Search(args) => {
            let terms: Vec<&str> = args
                .terms
                .iter()
                .flat_map(|t| t.split_whitespace())
                .collect();
            let hits = tracker.search(&terms, args.include_deleted, args.limit)?;
            if cli.json {
                output::print_search_json(&hits)
            } else {
                output::print_search(&hits, colors)
            }
        }
        Command::Brief(args) => {
            let path = args.workdir.as_deref().unwrap_or(Path::new("."));
            let brief = build_brief(&tracker, path, args.max_chars)?;
            if cli.json {
                output::print_json(&brief)
            } else if brief.context.is_empty() && brief.omitted == 0 {
                writeln!(
                    io::stdout(),
                    "No actionable work items in this Work Directory."
                )?;
                Ok(())
            } else {
                write!(io::stdout(), "{}", brief.context)?;
                Ok(())
            }
        }
        Command::Path
        | Command::Hook(_)
        | Command::Doctor(_)
        | Command::Serve(_)
        | Command::Backup(_)
        | Command::Backups(_)
        | Command::Restore(_) => unreachable!(),
    }
}

fn build_brief(tracker: &Tracker, path: &Path, max_chars: usize) -> Result<Brief> {
    let workdir = cli::work_directory_query(path)?;
    let items = tracker
        .list_in_directory(
            ListFilter::Actionable,
            WorkDirectoryFilter::Exact(&workdir),
            false,
            1_000,
        )?
        .into_iter()
        .map(|item| {
            let latest_note = tracker.latest_note(item.id)?;
            let unresolved_blockers = tracker
                .dependencies(item.id)?
                .blocked_by
                .into_iter()
                .filter(|dep| dep.status != Status::Done)
                .collect();
            Ok(BriefItem {
                item,
                latest_note,
                unresolved_blockers,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(brief::render(&workdir, &items, max_chars))
}

/// Hooks must never block the agent: every failure becomes an empty context
/// plus one line on standard error, and the process exits 0.
fn run_hook(database: Option<PathBuf>, event: HookEvent) {
    let context = match event {
        HookEvent::SessionStart => session_start_context(database).unwrap_or_else(|error| {
            eprintln!("work-tracker hook: {error:#}");
            String::new()
        }),
    };
    let envelope = json!({
        "hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": context,
        }
    });
    let _ = writeln!(io::stdout(), "{envelope}");
}

fn session_start_context(database: Option<PathBuf>) -> Result<String> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    let input: serde_json::Value = if input.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&input)?
    };
    // A resumed session already holds its context; injecting again would
    // invalidate the prompt cache.
    if input["source"] == "resume" {
        return Ok(String::new());
    }
    let database = cli::database_path(database)?;
    if !database.exists() {
        return Ok(String::new());
    }
    let cwd = input["cwd"]
        .as_str()
        .map_or_else(|| PathBuf::from("."), PathBuf::from);
    let tracker = Tracker::open(&database)?;
    Ok(build_brief(&tracker, &cwd, brief::DEFAULT_MAX_CHARS)?.context)
}

fn list_filter(args: &ListArgs) -> ListFilter {
    match (args.all, args.ready, args.status) {
        (_, _, Some(status)) => ListFilter::Status(status),
        (true, _, None) => ListFilter::All,
        (false, true, None) => ListFilter::Ready,
        (false, false, None) => ListFilter::Actionable,
    }
}

fn show_item(
    item: &work_tracker::domain::WorkItem,
    json: bool,
    colors: output::Colors,
) -> Result<()> {
    if json {
        output::item_json(item)?;
        output::print_json(item)
    } else {
        output::print_item(item, colors)
    }
}

fn show_items(
    items: &[work_tracker::domain::WorkItem],
    json: bool,
    colors: output::Colors,
) -> Result<()> {
    if json {
        output::print_json(&items)
    } else {
        output::print_items(items, colors)
    }
}
