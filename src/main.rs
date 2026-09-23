use std::process::ExitCode;
use std::str::FromStr;

use anyhow::Result;
use clap::Parser;
use serde_json::json;
use work_tracker::{
    cli::{self, Cli, Command, InitBackend, ListArgs},
    config::{self, AppConfig},
    domain::Status,
    github::{GitHub, GitHubError, RepositoryName},
    ledger::{LedgerConfig, ListFilter},
    output, web,
};

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let json_output = cli.json;
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if json_output {
                if let Some(github_error) = error.downcast_ref::<GitHubError>() {
                    eprintln!(
                        "{}",
                        json!({"error": {"code": github_error.code(), "message": format!("{error:#}")}})
                    );
                } else {
                    eprintln!("{}", json!({"error": format!("{error:#}")}));
                }
            } else {
                eprintln!("error: {error:#}");
            }
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<()> {
    if let Command::Init(args) = &cli.command {
        return match &args.backend {
            InitBackend::Github(args) => init_github(&cli, args.target.as_deref()),
        };
    }
    if matches!(cli.command, Command::Path) {
        return show_path(&cli);
    }
    let app_config = AppConfig::load(&config::config_path()?)?;
    if cli.database.is_none() {
        let selected = cli
            .repository
            .as_deref()
            .map(RepositoryName::from_str)
            .transpose()?
            .or_else(|| {
                app_config
                    .as_ref()
                    .map(|config| config.default_repository.clone())
            });
        if let Some(repository) = selected {
            anyhow::bail!(
                "GitHub ledger {repository} is selected; Work Item commands are not available until the GitHub ledger adapter is installed (use --database for the explicit local backend)"
            );
        }
    }
    let database = cli::database_path(cli.database)?;
    cli::prepare_database_path(&database)?;
    let ledger_config = LedgerConfig::sqlite(&database);

    if let Command::Serve(args) = &cli.command {
        ledger_config.open()?;
        return web::serve(ledger_config, &args.bind).await;
    }
    let mut ledger = ledger_config.open()?;
    match cli.command {
        Command::Add(args) => {
            let item = ledger.create(
                &args.title,
                args.description.as_deref(),
                args.status,
                &args.actor.resolved(),
                args.note.as_deref(),
            )?;
            show_item(&item, cli.json)
        }
        Command::Show(args) => show_item(&ledger.get(args.id)?, cli.json),
        Command::List(args) => {
            let filter = list_filter(&args);
            let items = ledger.list(filter, args.include_archived, args.limit)?;
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
            let items = ledger.daily_view(args.include_archived)?;
            show_items(&items, cli.json)
        }
        Command::Update(args) => {
            let description = if args.clear_description {
                Some(None)
            } else {
                args.description.as_deref().map(Some)
            };
            let item = ledger.update(
                args.id,
                args.title.as_deref(),
                description,
                &args.actor.resolved(),
                args.note.as_deref(),
            )?;
            show_item(&item, cli.json)
        }
        Command::Status(args) => {
            let item = ledger.set_status(
                args.id,
                args.status,
                &args.actor.resolved(),
                args.note.as_deref(),
            )?;
            show_item(&item, cli.json)
        }
        Command::Note(args) => {
            let entry = ledger.add_note(args.id, &args.message, &args.actor.resolved())?;
            if cli.json {
                output::print_json(&entry)
            } else {
                output::print_history(&[entry]);
                Ok(())
            }
        }
        Command::Archive(args) => {
            let item = ledger.set_status(
                args.id,
                Status::Archived,
                &args.actor.resolved(),
                args.note.as_deref(),
            )?;
            show_item(&item, cli.json)
        }
        Command::History(args) => {
            let entries = ledger.history(args.id)?;
            if cli.json {
                output::print_json(&entries)
            } else {
                output::print_history(&entries);
                Ok(())
            }
        }
        Command::Init(_) | Command::Path | Command::Serve(_) => unreachable!(),
    }
}

fn show_path(cli: &Cli) -> Result<()> {
    if let Some(database) = cli.database.as_ref() {
        if cli.json {
            return output::print_json(&json!({"backend": "sqlite", "database": database}));
        }
        println!("{}", database.display());
        return Ok(());
    }
    let app_config = AppConfig::load(&config::config_path()?)?;
    let requested_override = cli
        .repository
        .as_deref()
        .map(RepositoryName::from_str)
        .transpose()?;
    let explicit = requested_override
        .as_ref()
        .map(|repository| GitHub::new().validate_existing_repository(repository))
        .transpose()?;
    let repository = explicit
        .as_ref()
        .or_else(|| app_config.as_ref().map(|config| &config.default_repository));
    if let Some(repository) = repository {
        let cache = config::github_cache_path(repository)?;
        if cli.json {
            return output::print_json(&json!({
                "backend": "github",
                "repository": repository,
                "cache": cache,
                "database": cache,
            }));
        }
        println!("Repository: {repository}");
        println!("Cache:      {}", cache.display());
        return Ok(());
    }

    let database = cli::database_path(None)?;
    if cli.json {
        return output::print_json(&json!({"backend": "sqlite", "database": database}));
    }
    println!("{}", database.display());
    Ok(())
}

fn init_github(cli: &Cli, positional_repository: Option<&str>) -> Result<()> {
    let config_path = config::config_path()?;
    let existing_config = AppConfig::load(&config_path)?;
    let github = GitHub::new();
    let explicit_repository = positional_repository.or(cli.repository.as_deref());
    let requested = match (explicit_repository, existing_config.as_ref()) {
        (Some(repository), _) => repository.parse()?,
        (None, Some(config)) => config.default_repository.clone(),
        (None, None) => format!("{}/work-tracker-data", github.authenticated_user()?).parse()?,
    };
    let (repository, created) = github.ensure_repository(&requested)?;
    github.validate_repository(&repository)?;
    github.provision_metadata(&repository.full_name, created)?;

    let cache = config::github_cache_path(&repository.full_name)?;
    cli::prepare_database_path(&cache)?;
    LedgerConfig::sqlite(&cache).open()?;

    let is_default = existing_config.as_ref().is_none_or(|config| {
        config
            .default_repository
            .eq_ignore_case(&repository.full_name)
    });
    if existing_config.is_none() {
        AppConfig::new(repository.full_name.clone()).save(&config_path)?;
    }

    let result = json!({
        "backend": "github",
        "repository": repository.full_name,
        "private": repository.private,
        "created": created,
        "default": is_default,
        "cache": cache,
    });
    if cli.json {
        output::print_json(&result)
    } else {
        println!(
            "GitHub ledger: {}",
            result["repository"].as_str().unwrap_or_default()
        );
        println!("Private:       yes");
        println!(
            "Default:       {}",
            if is_default { "yes" } else { "no (override)" }
        );
        println!("Cache:         {}", cache.display());
        Ok(())
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
