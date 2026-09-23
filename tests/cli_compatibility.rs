mod support;

use anyhow::{Context, Result, ensure};
use rusqlite::Connection;
use serde_json::Value;
use support::{CliHarness, assert_database_exists, assert_success, stderr, stdout};

fn json(output: &std::process::Output) -> Result<Value> {
    serde_json::from_slice(&output.stdout).context("stdout was not valid JSON")
}

fn create_legacy_database(path: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(path.parent().context("database path omitted parent")?)?;
    let connection = Connection::open(path)?;
    connection.execute_batch(
        r#"
        CREATE TABLE work_items (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL CHECK (length(trim(title)) > 0),
            description TEXT,
            status TEXT NOT NULL CHECK (
                status IN ('pending', 'active', 'waiting', 'blocked', 'done', 'cancelled', 'deleted')
            ),
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            deleted_at TEXT,
            purge_after TEXT,
            CHECK (
                (status = 'deleted' AND deleted_at IS NOT NULL AND purge_after IS NOT NULL)
                OR
                (status != 'deleted' AND deleted_at IS NULL AND purge_after IS NULL)
            )
        );
        CREATE TABLE history_entries (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            work_item_id INTEGER NOT NULL REFERENCES work_items(id) ON DELETE CASCADE,
            kind TEXT NOT NULL,
            actor TEXT NOT NULL CHECK (length(trim(actor)) > 0),
            note TEXT,
            occurred_at TEXT NOT NULL,
            changes_json TEXT NOT NULL
        );
        INSERT INTO work_items
            (id, title, status, created_at, updated_at, deleted_at, purge_after)
        VALUES
            (7, 'Legacy evidence', 'deleted', '2020-01-01T00:00:00.000Z',
             '2020-01-02T00:00:00.000Z', '2020-01-02T00:00:00.000Z',
             '2020-03-02T00:00:00.000Z');
        INSERT INTO history_entries
            (work_item_id, kind, actor, occurred_at, changes_json)
        VALUES
            (7, 'created', 'legacy-agent', '2020-01-01T00:00:00.000Z',
             '{"title":"Legacy evidence","status":"pending"}'),
            (7, 'deleted', 'legacy-agent', '2020-01-02T00:00:00.000Z',
             '{"status":{"from":"pending","to":"deleted"}}');
        PRAGMA journal_mode = WAL;
        PRAGMA user_version = 1;
        "#,
    )?;
    Ok(())
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
    assert_eq!(json(&delete)?["status"], "archived");

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
fn archive_emits_canonical_status_and_additive_compatibility_metadata() -> Result<()> {
    let cli = CliHarness::new()?;
    let add = cli.run(["--json", "add", "Keep the evidence"])?;
    assert_success(&add)?;
    let id = json(&add)?["id"]
        .as_i64()
        .context("add output omitted id")?;

    let archive = cli.run(["--json", "archive", &id.to_string(), "--actor", "agent-a"])?;
    assert_success(&archive)?;
    assert_eq!(stderr(&archive)?, "");
    let item = json(&archive)?;
    assert_eq!(item["status"], "archived");
    assert!(item["archived_at"].is_string());
    assert_eq!(item["deleted_at"], item["archived_at"]);
    assert!(item["purge_after"].is_null());

    let history = cli.run(["--json", "history", &id.to_string()])?;
    assert_success(&history)?;
    let entries = json(&history)?;
    assert_eq!(entries[1]["kind"], "archived");
    assert_eq!(entries[1]["changes"]["status"]["to"], "archived");

    let today = cli.run(["--json", "today", "--include-archived"])?;
    assert_success(&today)?;
    assert_eq!(
        json(&today)?
            .as_array()
            .context("today was not an array")?
            .len(),
        1
    );
    Ok(())
}

#[test]
fn deprecated_deletion_inputs_select_archival_and_emit_only_canonical_language() -> Result<()> {
    let cli = CliHarness::new()?;
    let first = cli.run(["--json", "add", "Archive by command alias"])?;
    assert_success(&first)?;
    let first_id = json(&first)?["id"]
        .as_i64()
        .context("add output omitted id")?
        .to_string();
    let second = cli.run(["--json", "add", "Archive by status alias"])?;
    assert_success(&second)?;
    let second_id = json(&second)?["id"]
        .as_i64()
        .context("add output omitted id")?
        .to_string();

    let delete = cli.run(["delete", &first_id, "--actor", "legacy-agent"])?;
    assert_success(&delete)?;
    ensure!(stdout(&delete)?.contains("Status:      archived"));
    ensure!(!stdout(&delete)?.contains("deleted"));

    let deleted_status = cli.run([
        "--json",
        "status",
        &second_id,
        "deleted",
        "--actor",
        "legacy-agent",
    ])?;
    assert_success(&deleted_status)?;
    assert_eq!(json(&deleted_status)?["status"], "archived");

    let included = cli.run(["--json", "list", "--all", "--include-deleted"])?;
    assert_success(&included)?;
    let items = json(&included)?;
    let items = items.as_array().context("list was not an array")?;
    assert_eq!(items.len(), 2);
    assert!(items.iter().all(|item| item["status"] == "archived"));

    let filtered = cli.run(["--json", "list", "--status", "deleted"])?;
    assert_success(&filtered)?;
    assert_eq!(
        json(&filtered)?
            .as_array()
            .context("list was not an array")?
            .len(),
        2
    );

    let today = cli.run(["--json", "today", "--include-deleted"])?;
    assert_success(&today)?;
    assert_eq!(
        json(&today)?
            .as_array()
            .context("today was not an array")?
            .len(),
        2
    );
    Ok(())
}

#[test]
fn opening_a_legacy_database_archives_deleted_items_without_purging_history() -> Result<()> {
    let cli = CliHarness::new()?;
    let database = cli.database_path();
    create_legacy_database(&database)?;

    let shown = cli.run(["--json", "show", "7"])?;
    assert_success(&shown)?;
    let item = json(&shown)?;
    assert_eq!(item["status"], "archived");
    assert_eq!(item["archived_at"], "2020-01-02T00:00:00Z");
    assert_eq!(item["deleted_at"], item["archived_at"]);
    assert!(item["purge_after"].is_null());

    let history = cli.run(["--json", "history", "7"])?;
    assert_success(&history)?;
    let entries = json(&history)?;
    assert_eq!(
        entries
            .as_array()
            .context("history was not an array")?
            .len(),
        2
    );
    assert_eq!(entries[1]["kind"], "archived");
    assert_eq!(entries[1]["changes"]["status"]["to"], "archived");
    let human_history = cli.run(["history", "7"])?;
    assert_success(&human_history)?;
    ensure!(stdout(&human_history)?.contains("archived"));
    ensure!(!stdout(&human_history)?.contains("deleted"));

    let reopened = cli.run(["--json", "show", "7"])?;
    assert_success(&reopened)?;
    assert_eq!(json(&reopened)?["status"], "archived");
    Ok(())
}

#[test]
fn concurrent_legacy_opens_apply_the_migration_once() -> Result<()> {
    let cli = CliHarness::new()?;
    create_legacy_database(&cli.database_path())?;
    let barrier = std::sync::Barrier::new(16);

    std::thread::scope(|scope| -> Result<()> {
        let handles = (0..16)
            .map(|_| {
                scope.spawn(|| -> Result<std::process::Output> {
                    barrier.wait();
                    cli.run(["--json", "show", "7"])
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            assert_success(&handle.join().expect("CLI thread panicked")?)?;
        }
        Ok(())
    })?;

    let history = cli.run(["--json", "history", "7"])?;
    assert_success(&history)?;
    assert_eq!(
        json(&history)?
            .as_array()
            .context("history was not an array")?
            .len(),
        2
    );
    let connection = Connection::open(cli.database_path())?;
    let schema_version: i64 =
        connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    assert_eq!(schema_version, 5);
    let sync_columns: i64 = connection.query_row(
        "SELECT count(*) FROM pragma_table_info('github_cache_state')
         WHERE name IN ('sync_cursor', 'etag', 'last_successful_sync_at')",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(sync_columns, 3);
    Ok(())
}

#[test]
fn archived_work_items_are_readable_but_immutable() -> Result<()> {
    let cli = CliHarness::new()?;
    let add = cli.run(["--json", "add", "Final evidence"])?;
    assert_success(&add)?;
    let id = json(&add)?["id"]
        .as_i64()
        .context("add output omitted id")?
        .to_string();
    assert_success(&cli.run(["archive", &id, "--actor", "agent-a"])?)?;

    for args in [
        vec!["update", &id, "--title", "Rewritten"],
        vec!["status", &id, "active"],
        vec!["note", &id, "More context"],
    ] {
        let output = cli.run(args)?;
        assert_eq!(output.status.code(), Some(1));
        ensure!(stderr(&output)?.contains("is archived and cannot be modified"));
    }

    let shown = cli.run(["--json", "show", &id])?;
    assert_success(&shown)?;
    assert_eq!(json(&shown)?["title"], "Final evidence");
    let history = cli.run(["--json", "history", &id])?;
    assert_success(&history)?;
    assert_eq!(
        json(&history)?
            .as_array()
            .context("history was not an array")?
            .len(),
        2
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
