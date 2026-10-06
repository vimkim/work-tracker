use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;
use serde_json::json;
use work_tracker::{
    cli::{self, Cli, Command, ListArgs},
    db::{ListFilter, Tracker},
    domain::Status,
    output,
    todo::TodoWindow,
    web,
};

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let json_output = cli.json;
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
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

async fn run(cli: Cli) -> Result<()> {
    let database = cli::database_path(cli.database)?;
    cli::prepare_database_path(&database)?;

    if let Command::Serve(args) = &cli.command {
        Tracker::open(&database)?;
        return web::serve(database, &args.bind).await;
    }
    if matches!(cli.command, Command::Path) {
        if cli.json {
            return output::print_json(&json!({"database": database}));
        }
        println!("{}", database.display());
        return Ok(());
    }

    let mut tracker = Tracker::open(&database)?;
    match cli.command {
        Command::Add(args) => {
            let item = tracker.create_scheduled(
                &args.title,
                args.description.as_deref(),
                args.status,
                &args.actor.resolved(),
                args.note.as_deref(),
                args.schedule.for_creation(),
            )?;
            show_item(&item, cli.json)
        }
        Command::Show(args) => show_item(&tracker.get(args.id)?, cli.json),
        Command::List(args) => {
            let filter = list_filter(&args);
            let items = tracker.list(filter, args.include_deleted, args.limit)?;
            if cli.json {
                output::print_json(&items)
            } else if filter == ListFilter::Actionable {
                output::print_actionable_items(&items);
                Ok(())
            } else {
                output::print_items(&items);
                Ok(())
            }
        }
        Command::Today(args) => {
            let items = tracker.daily_view(args.include_deleted)?;
            show_items(&items, cli.json)
        }
        Command::Todo(args) => {
            let window = TodoWindow::new(chrono::Utc::now(), args.days.unwrap_or(1))?;
            let view = tracker.todo_view(window)?;
            if cli.json {
                output::print_json(&view)
            } else {
                print!("{}", output::format_todo(&view));
                Ok(())
            }
        }
        Command::Update(args) => {
            let description = if args.clear_description {
                Some(None)
            } else {
                args.description.as_deref().map(Some)
            };
            let item = tracker.update_scheduled(
                args.id,
                args.title.as_deref(),
                description,
                &args.actor.resolved(),
                args.note.as_deref(),
                args.schedule_update(),
            )?;
            show_item(&item, cli.json)
        }
        Command::Status(args) => {
            let item = tracker.set_status(
                args.id,
                args.status,
                &args.actor.resolved(),
                args.note.as_deref(),
            )?;
            show_item(&item, cli.json)
        }
        Command::Note(args) => {
            let entry = tracker.add_note(args.id, &args.message, &args.actor.resolved())?;
            if cli.json {
                output::print_json(&entry)
            } else {
                output::print_history(&[entry]);
                Ok(())
            }
        }
        Command::Delete(args) => {
            let item = tracker.set_status(
                args.id,
                Status::Deleted,
                &args.actor.resolved(),
                args.note.as_deref(),
            )?;
            show_item(&item, cli.json)
        }
        Command::History(args) => {
            let entries = tracker.history(args.id)?;
            if cli.json {
                output::print_json(&entries)
            } else {
                output::print_history(&entries);
                Ok(())
            }
        }
        Command::Path | Command::Serve(_) => unreachable!(),
    }
}

fn list_filter(args: &ListArgs) -> ListFilter {
    match (args.all, args.status) {
        (_, Some(status)) => ListFilter::Status(status),
        (true, None) => ListFilter::All,
        (false, None) => ListFilter::Actionable,
    }
}

fn show_item(item: &work_tracker::domain::WorkItem, json: bool) -> Result<()> {
    if json {
        output::print_json(item)
    } else {
        output::print_item(item);
        Ok(())
    }
}

fn show_items(items: &[work_tracker::domain::WorkItem], json: bool) -> Result<()> {
    if json {
        output::print_json(&items)
    } else {
        output::print_items(items);
        Ok(())
    }
}
