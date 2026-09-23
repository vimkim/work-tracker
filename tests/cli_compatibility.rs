mod support;

use anyhow::{Context, Result, ensure};
use serde_json::Value;
use support::{CliHarness, assert_database_exists, assert_success, stderr, stdout};

fn json(output: &std::process::Output) -> Result<Value> {
    serde_json::from_slice(&output.stdout).context("stdout was not valid JSON")
}

#[test]
fn default_path_and_storage_are_isolated() -> Result<()> {
    let cli = CliHarness::new()?;

    let path = cli.run(["--json", "path"])?;
    assert_success(&path)?;
    assert_eq!(stderr(&path)?, "");
    assert_eq!(
        json(&path)?["database"],
        cli.database_path().to_string_lossy().as_ref()
    );

    let add = cli.run(["--json", "add", "Isolated item"])?;
    assert_success(&add)?;
    assert_database_exists(&cli.database_path())?;
    Ok(())
}

#[test]
fn compiled_cli_preserves_the_sqlite_lifecycle_contract() -> Result<()> {
    let cli = CliHarness::new()?;

    let add = cli.run([
        "--json",
        "add",
        "Watch CI",
        "--description",
        "Wait for the suite",
        "--actor",
        "agent-a",
    ])?;
    assert_success(&add)?;
    assert_eq!(stderr(&add)?, "");
    let item = json(&add)?;
    let id = item["id"].as_i64().context("add output omitted id")?;
    assert_eq!(item["status"], "pending");

    let id_arg = id.to_string();
    let note = cli.run(["note", &id_arg, "Queue position 12", "--actor", "agent-b"])?;
    assert_success(&note)?;
    ensure!(stdout(&note)?.contains("noted"));

    let update = cli.run([
        "--json",
        "update",
        &id_arg,
        "--title",
        "Watch full CI",
        "--actor",
        "agent-a",
    ])?;
    assert_success(&update)?;
    assert_eq!(json(&update)?["title"], "Watch full CI");

    let status = cli.run(["--json", "status", &id_arg, "waiting", "--actor", "agent-a"])?;
    assert_success(&status)?;
    assert_eq!(json(&status)?["status"], "waiting");

    let show = cli.run(["show", &id_arg])?;
    assert_success(&show)?;
    ensure!(stdout(&show)?.contains("Status:      waiting"));
    ensure!(stdout(&show)?.contains("Title:       Watch full CI"));

    let list = cli.run(["--json", "list"])?;
    assert_success(&list)?;
    assert_eq!(
        json(&list)?
            .as_array()
            .context("list was not an array")?
            .len(),
        1
    );

    let today = cli.run(["--json", "today"])?;
    assert_success(&today)?;
    assert_eq!(
        json(&today)?
            .as_array()
            .context("today was not an array")?
            .len(),
        1
    );

    let history = cli.run(["--json", "history", &id_arg])?;
    assert_success(&history)?;
    let entries = json(&history)?;
    let entries = entries.as_array().context("history was not an array")?;
    assert_eq!(entries.len(), 4);
    assert_eq!(entries[0]["kind"], "created");
    assert_eq!(entries[1]["kind"], "noted");
    assert_eq!(entries[2]["kind"], "updated");
    assert_eq!(entries[3]["kind"], "status_changed");

    let delete = cli.run(["--json", "delete", &id_arg, "--actor", "agent-a"])?;
    assert_success(&delete)?;
    assert_eq!(json(&delete)?["status"], "deleted");

    let hidden = cli.run(["--json", "list", "--all"])?;
    assert_success(&hidden)?;
    assert!(
        json(&hidden)?
            .as_array()
            .context("list was not an array")?
            .is_empty()
    );

    let included = cli.run(["--json", "list", "--include-deleted", "--all"])?;
    assert_success(&included)?;
    assert_eq!(
        json(&included)?
            .as_array()
            .context("list was not an array")?
            .len(),
        1
    );
    Ok(())
}

#[test]
fn compiled_cli_separates_json_errors_from_stdout() -> Result<()> {
    let cli = CliHarness::new()?;

    let output = cli.run(["--json", "show", "999"])?;

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stdout(&output)?, "");
    let error: Value = serde_json::from_slice(&output.stderr)?;
    ensure!(
        error["error"]
            .as_str()
            .is_some_and(|message| message.contains("not found"))
    );
    Ok(())
}
