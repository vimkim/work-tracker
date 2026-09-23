mod support;

use std::fs;

use anyhow::{Context, Result, ensure};
use chrono::{Duration, FixedOffset, TimeZone, Utc};
use rusqlite::Connection;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use support::{CliHarness, FakeGh, assert_success, stderr};
use uuid::Uuid;

fn configure_github(cli: &CliHarness) -> Result<()> {
    let path = cli.config_path();
    fs::create_dir_all(path.parent().context("configuration path omitted parent")?)?;
    fs::write(
        path,
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "default_repository": "octocat/work-tracker-data"
        }))?,
    )?;
    Ok(())
}

fn projection_body(description: Option<&str>, event_id: &str, comment_id: i64) -> String {
    let visible = description.unwrap_or("");
    format!(
        "{visible}\n\n<!-- work-tracker:projection\n{}\n-->",
        json!({
            "schema_version": 1,
            "kind": "work_item",
            "event_id": event_id,
            "creation_fingerprint": "fingerprint-41",
            "pending_genesis_event_id": null,
            "genesis_comment_id": comment_id,
            "state_revision": 1
        })
    )
}

#[derive(Serialize)]
struct CanonicalGenesis<'a> {
    schema_version: u32,
    event_id: &'a str,
    kind: &'a str,
    actor: &'a str,
    github_actor: &'a str,
    note: &'a str,
    changes: Value,
}

fn trusted_projection_body(
    title: &str,
    description: Option<&str>,
    status: &str,
    event_id: &str,
    comment_id: i64,
) -> Result<String> {
    let canonical = serde_json::to_vec(&CanonicalGenesis {
        schema_version: 1,
        event_id,
        kind: "created",
        actor: "agent-a",
        github_actor: "octocat",
        note: "created remotely",
        changes: json!({
            "title": title,
            "description": description,
            "status": status,
        }),
    })?;
    let mut hasher = Sha256::new();
    hasher.update(b"work-tracker-history-v1");
    for part in [&[][..], canonical.as_slice(), &comment_id.to_be_bytes()] {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    let hash = format!("{:x}", hasher.finalize());
    let visible = description.unwrap_or("");
    Ok(format!(
        "{visible}\n\n<!-- work-tracker:projection\n{}\n-->",
        json!({
            "schema_version": 1,
            "kind": "work_item",
            "event_id": event_id,
            "creation_fingerprint": "fingerprint-41",
            "pending_genesis_event_id": null,
            "genesis_comment_id": comment_id,
            "state_revision": 1,
            "head_event_id": event_id,
            "head_comment_id": comment_id,
            "history_hash": hash
        })
    ))
}

fn seed_v4_cache_with_genesis(
    cli: &CliHarness,
    title: &str,
    description: Option<&str>,
    status: &str,
    comment_id: i64,
    occurred_at: &str,
) -> Result<()> {
    let path = cli.github_cache_path("octocat", "work-tracker-data");
    fs::create_dir_all(path.parent().context("cache path omitted parent")?)?;
    let connection = Connection::open(path)?;
    connection.execute_batch(
        "CREATE TABLE work_items (
             id INTEGER PRIMARY KEY,
             title TEXT NOT NULL,
             description TEXT,
             status TEXT NOT NULL,
             created_at TEXT NOT NULL,
             updated_at TEXT NOT NULL,
             archived_at TEXT
         );
         CREATE TABLE history_entries (
             id INTEGER PRIMARY KEY,
             work_item_id INTEGER NOT NULL,
             kind TEXT NOT NULL,
             actor TEXT NOT NULL,
             note TEXT,
             occurred_at TEXT NOT NULL,
             changes_json TEXT NOT NULL
         );
         CREATE TABLE pending_github_creations (
             request_json TEXT PRIMARY KEY,
             event_id TEXT NOT NULL UNIQUE,
             issue_number INTEGER
         );
         CREATE TABLE github_cache_state (
             repository TEXT PRIMARY KEY,
             sync_cursor TEXT,
             etag TEXT
         );
         PRAGMA user_version = 4;",
    )?;
    connection.execute(
        "INSERT INTO work_items
         (id, title, description, status, created_at, updated_at, archived_at)
         VALUES (41, ?1, ?2, ?3, ?4, ?4, NULL)",
        rusqlite::params![title, description, status, occurred_at],
    )?;
    connection.execute(
        "INSERT INTO history_entries
         (id, work_item_id, kind, actor, note, occurred_at, changes_json)
         VALUES (?1, 41, 'created', 'agent-a', 'created remotely', ?2, ?3)",
        rusqlite::params![
            comment_id,
            occurred_at,
            json!({"title": title, "description": description, "status": status}).to_string()
        ],
    )?;
    connection.execute(
        "INSERT INTO github_cache_state (repository, sync_cursor, etag)
         VALUES ('octocat/work-tracker-data', '2026-09-22T15:00:00.000Z', 'v4-etag')",
        [],
    )?;
    Ok(())
}

fn genesis_body(title: &str, description: Option<&str>, status: &str, event_id: &str) -> String {
    format!(
        "Work Tracker History Entry: created by agent-a\n\n<!-- work-tracker:event\n{}\n-->",
        json!({
            "schema_version": 1,
            "event_id": event_id,
            "kind": "created",
            "actor": "agent-a",
            "github_actor": "octocat",
            "note": "created remotely",
            "initial_values": {
                "title": title,
                "description": description,
                "status": status
            },
            "occurred_at_source": "github_comment.created_at"
        })
    )
}

fn pending_projection_body(
    description: Option<&str>,
    event_id: &str,
    creation_fingerprint: &str,
    pending_genesis_event: Option<Value>,
) -> String {
    let visible = description.unwrap_or("_No description provided._");
    let creation_event_id_supplied = pending_genesis_event.is_some();
    format!(
        "{visible}\n\n<!-- work-tracker:projection\n{}\n-->",
        json!({
            "schema_version": 1,
            "kind": "work_item",
            "event_id": event_id,
            "creation_fingerprint": creation_fingerprint,
            "creation_event_id_supplied": creation_event_id_supplied,
            "pending_genesis_event_id": event_id,
            "pending_genesis_event": pending_genesis_event,
            "genesis_comment_id": null,
            "state_revision": 0,
            "head_event_id": null,
            "head_comment_id": null,
            "history_hash": null
        })
    )
}

fn json_stdout(output: &std::process::Output) -> Result<Value> {
    serde_json::from_slice(&output.stdout).context("stdout was not valid JSON")
}

#[test]
fn synchronization_completes_a_published_pending_genesis() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    let gh = FakeGh::new()?;
    let event_id = "genesis-interrupted-41";
    gh.respond(
        1,
        0,
        &json!([[{
            "number": 41,
            "title": "Interrupted creation",
            "body": pending_projection_body(
                Some("Recover me"),
                event_id,
                "creation-interrupted",
                None,
            ),
            "labels": [
                {"name": "work-tracker:item"},
                {"name": "work-tracker:status:active"}
            ],
            "state": "open",
            "locked": false,
            "updated_at": "2026-09-23T09:00:00Z"
        }]])
        .to_string(),
        "",
    )?;
    gh.respond(
        2,
        0,
        &json!([[{
            "id": 9001,
            "created_at": "2026-09-23T08:59:00Z",
            "user": {"login": "octocat"},
            "body": genesis_body("Interrupted creation", Some("Recover me"), "active", event_id)
        }]])
        .to_string(),
        "",
    )?;
    gh.respond(3, 0, "{}", "")?;

    let shown = cli.run_with_fake_gh(&gh, ["--json", "show", "41"])?;
    assert_success(&shown)?;
    let item = json_stdout(&shown)?;
    assert_eq!(item["title"], "Interrupted creation");
    assert_eq!(item["description"], "Recover me");
    assert_eq!(item["status"], "active");
    let warning: Value = serde_json::from_slice(&shown.stderr)?;
    assert_eq!(warning["warning"]["code"], "github_projection_repaired");
    assert_eq!(warning["warning"]["work_item_ids"], json!([41]));
    let calls = gh.calls()?;
    ensure!(calls.contains("\tPATCH\trepos/octocat/work-tracker-data/issues/41"));
    ensure!(calls.contains("\"pending_genesis_event_id\":null"));
    ensure!(calls.contains("\"genesis_comment_id\":9001"));
    Ok(())
}

#[test]
fn synchronization_publishes_pending_genesis_after_disposable_cache_loss() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    let event_id = "genesis-local-pending-41";
    let request_json = r#"{"title":"Pending publication","description":"Publish me","status":"active","actor":"agent-a","note":"creation context","event_id":"genesis-local-pending-41"}"#;
    let creation_fingerprint = format!(
        "creation-{}",
        Uuid::new_v5(&Uuid::NAMESPACE_OID, request_json.as_bytes())
    );
    let pending_event = json!({
        "schema_version": 1,
        "event_id": event_id,
        "kind": "created",
        "actor": "agent-a",
        "github_actor": "octocat",
        "note": "creation context",
        "changes": {
            "title": "Pending publication",
            "description": "Publish me",
            "status": "active"
        }
    });

    let gh = FakeGh::new()?;
    gh.respond(
        1,
        0,
        &json!([[{
            "number": 41,
            "title": "Pending publication",
            "body": pending_projection_body(
                Some("Publish me"),
                event_id,
                &creation_fingerprint,
                Some(pending_event),
            ),
            "labels": [
                {"name": "work-tracker:item"},
                {"name": "work-tracker:status:active"}
            ],
            "state": "open",
            "locked": false,
            "updated_at": "2026-09-23T09:00:00Z"
        }]])
        .to_string(),
        "",
    )?;
    gh.respond(2, 0, "[[]]", "")?;
    gh.respond(3, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(
        4,
        0,
        r#"{"id":9001,"created_at":"2026-09-23T08:59:00Z","user":{"login":"octocat"}}"#,
        "",
    )?;
    gh.respond(5, 0, "{}", "")?;

    let shown = cli.run_with_fake_gh(&gh, ["--json", "show", "41"])?;
    assert_success(&shown)?;
    assert_eq!(json_stdout(&shown)?["title"], "Pending publication");
    let calls = gh.calls()?;
    assert_eq!(
        calls
            .matches("\tPOST\trepos/octocat/work-tracker-data/issues/41/comments")
            .count(),
        1
    );
    ensure!(calls.contains(event_id));
    let connection = Connection::open(cli.github_cache_path("octocat", "work-tracker-data"))?;
    let pending: i64 = connection.query_row(
        "SELECT count(*) FROM pending_github_creations WHERE event_id = ?1",
        [event_id],
        |row| row.get(0),
    )?;
    assert_eq!(pending, 0);
    Ok(())
}

#[test]
fn migrated_v4_cache_replays_history_and_bootstraps_the_legacy_projection() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    let gh = FakeGh::new()?;
    let event_id = "11111111-1111-4111-8111-111111111111";
    seed_v4_cache_with_genesis(
        &cli,
        "Watch CI",
        Some("Wait for the suite"),
        "waiting",
        9001,
        "2026-09-22T14:00:00Z",
    )?;
    let body = projection_body(Some("Wait for the suite"), event_id, 9001);
    gh.respond(
        1,
        0,
        &json!([[{
            "number": 41,
            "title": "Watch CI",
            "body": body,
            "labels": [
                {"name": "work-tracker:item"},
                {"name": "work-tracker:status:waiting"}
            ],
            "updated_at": "2026-09-22T15:00:00Z"
        }]])
        .to_string(),
        "",
    )?;
    gh.respond(
        2,
        0,
        &json!([[{
            "id": 9001,
            "created_at": "2026-09-22T14:00:00Z",
            "user": {"login": "octocat"},
            "body": genesis_body(
                "Watch CI",
                Some("Wait for the suite"),
                "waiting",
                event_id
            )
        }]])
        .to_string(),
        "",
    )?;
    let listed = cli.run_with_fake_gh(&gh, ["--json", "list", "--all"])?;
    assert_success(&listed)?;
    assert_eq!(stderr(&listed)?, "");
    let items = json_stdout(&listed)?;
    assert_eq!(items.as_array().context("list was not an array")?.len(), 1);
    assert_eq!(items[0]["id"], 41);
    assert_eq!(items[0]["title"], "Watch CI");
    assert_eq!(items[0]["description"], "Wait for the suite");
    assert_eq!(items[0]["status"], "waiting");

    let cache = cli.github_cache_path("octocat", "work-tracker-data");
    let history = cli.run([
        "--json",
        "--database",
        cache.to_str().context("cache path was not UTF-8")?,
        "history",
        "41",
    ])?;
    assert_success(&history)?;
    let entries = json_stdout(&history)?;
    assert_eq!(
        entries
            .as_array()
            .context("history was not an array")?
            .len(),
        1
    );
    assert_eq!(entries[0]["id"], 9001);
    assert_eq!(entries[0]["kind"], "created");
    assert_eq!(entries[0]["actor"], "agent-a");
    assert_eq!(entries[0]["note"], "created remotely");
    let calls = gh.calls()?;
    assert_eq!(
        calls
            .matches("\tPATCH\trepos/octocat/work-tracker-data/issues/41")
            .count(),
        1,
        "history replay should bootstrap the trusted head on a legacy projection"
    );
    assert!(calls.contains(&format!("\"head_event_id\":\"{event_id}\"")));
    assert!(calls.contains("\"head_comment_id\":9001"));
    assert!(calls.contains("\"history_hash\":"));
    Ok(())
}

#[test]
fn unknown_headless_projection_is_not_automatically_rebaselined() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    let gh = FakeGh::new()?;
    let event_id = "11111111-1111-4111-8111-111111111111";
    gh.respond(
        1,
        0,
        &json!([[{
            "number": 41,
            "title": "Watch CI",
            "body": projection_body(None, event_id, 9001),
            "labels": [
                {"name": "work-tracker:item"},
                {"name": "work-tracker:status:waiting"}
            ],
            "updated_at": "2026-09-22T15:00:00Z"
        }]])
        .to_string(),
        "",
    )?;
    gh.respond(
        2,
        0,
        &json!([[{
            "id": 9001,
            "created_at": "2026-09-22T14:00:00Z",
            "user": {"login": "octocat"},
            "body": genesis_body("Watch CI", None, "waiting", event_id)
        }]])
        .to_string(),
        "",
    )?;

    let failed = cli.run_with_fake_gh(&gh, ["--json", "list", "--all"])?;
    assert!(!failed.status.success());
    let diagnostic: Value = serde_json::from_slice(&failed.stderr)?;
    assert_eq!(diagnostic["error"]["code"], "github_ledger_integrity");
    assert!(
        diagnostic["error"]["message"]
            .as_str()
            .context("integrity diagnostic omitted message")?
            .contains("missing a trusted history head")
    );
    let calls = gh.calls()?;
    assert!(!calls.contains("\tPATCH\trepos/octocat/work-tracker-data/issues/41"));
    let connection = Connection::open(cli.github_cache_path("octocat", "work-tracker-data"))?;
    let count: i64 =
        connection.query_row("SELECT count(*) FROM work_items", [], |row| row.get(0))?;
    assert_eq!(count, 0);
    Ok(())
}

#[test]
fn subsequent_sync_uses_the_committed_cursor_and_tolerates_duplicate_observations() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    let gh = FakeGh::new()?;
    let first_event = "11111111-1111-4111-8111-111111111111";
    let second_event = "22222222-2222-4222-8222-222222222222";
    let first_issue = json!({
        "number": 41,
        "body": trusted_projection_body("Existing wait", None, "waiting", first_event, 9001)?,
        "labels": [
            {"name": "work-tracker:item"},
            {"name": "work-tracker:status:waiting"}
        ],
        "updated_at": "2026-09-22T15:00:00Z"
    });
    let first_comment = json!({
        "id": 9001,
        "created_at": "2026-09-22T14:00:00Z",
        "user": {"login": "octocat"},
        "body": genesis_body("Existing wait", None, "waiting", first_event)
    });
    gh.respond(1, 0, &json!([[first_issue.clone()]]).to_string(), "")?;
    gh.respond(2, 0, &json!([[first_comment.clone()]]).to_string(), "")?;
    assert_success(&cli.run_with_fake_gh(&gh, ["--json", "list", "--all"])?)?;

    let second_issue = json!({
        "number": 42,
        "body": trusted_projection_body(
            "New blocker",
            Some("Needs a decision"),
            "blocked",
            second_event,
            9002
        )?,
        "labels": [
            {"name": "work-tracker:item"},
            {"name": "work-tracker:status:blocked"}
        ],
        "updated_at": "2026-09-22T16:00:00Z"
    });
    let second_comment = json!({
        "id": 9002,
        "created_at": "2026-09-22T15:30:00Z",
        "user": {"login": "octocat"},
        "body": genesis_body(
            "New blocker",
            Some("Needs a decision"),
            "blocked",
            second_event
        )
    });
    gh.respond(3, 0, &json!([[first_issue, second_issue]]).to_string(), "")?;
    gh.respond(4, 0, &json!([[first_comment]]).to_string(), "")?;
    gh.respond(5, 0, &json!([[second_comment]]).to_string(), "")?;

    let refreshed = cli.run_with_fake_gh(&gh, ["--json", "list", "--all"])?;
    assert_success(&refreshed)?;
    let items = json_stdout(&refreshed)?;
    let items = items.as_array().context("list was not an array")?;
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["id"], 42);
    assert_eq!(items[1]["id"], 41);

    let calls = gh.calls()?;
    assert!(calls.contains(
        "issues?state=all&sort=updated&direction=asc&per_page=100&since=2026-09-22T15:00:00.000Z"
    ));
    let connection = Connection::open(cli.github_cache_path("octocat", "work-tracker-data"))?;
    let cursor: String = connection.query_row(
        "SELECT sync_cursor FROM github_cache_state WHERE repository = ?1",
        ["octocat/work-tracker-data"],
        |row| row.get(0),
    )?;
    assert_eq!(cursor, "2026-09-22T16:00:00.000Z");
    Ok(())
}

#[test]
fn unchanged_incremental_query_reuses_etag_and_accepts_not_modified() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    let gh = FakeGh::new()?;
    let event_id = "11111111-1111-4111-8111-111111111111";
    gh.respond(
        1,
        0,
        &json!([[{
            "number": 41,
            "body": trusted_projection_body("Stable item", None, "active", event_id, 9001)?,
            "labels": [
                {"name": "work-tracker:item"},
                {"name": "work-tracker:status:active"}
            ],
            "updated_at": "2026-09-22T15:00:00Z"
        }]])
        .to_string(),
        "",
    )?;
    gh.respond(
        2,
        0,
        &json!([[{
            "id": 9001,
            "created_at": "2026-09-22T14:00:00Z",
            "user": {"login": "octocat"},
            "body": genesis_body("Stable item", None, "active", event_id)
        }]])
        .to_string(),
        "",
    )?;
    assert_success(&cli.run_with_fake_gh(&gh, ["--json", "list"])?)?;

    gh.respond(
        3,
        0,
        "HTTP/2.0 200 OK\r\netag: \"stable-etag\"\r\n\r\n[]",
        "",
    )?;
    assert_success(&cli.run_with_fake_gh(&gh, ["--json", "list"])?)?;

    gh.respond(
        4,
        0,
        "HTTP/2.0 304 Not Modified\r\netag: \"stable-etag\"\r\n\r\n",
        "",
    )?;
    let unchanged = cli.run_with_fake_gh(&gh, ["--json", "list"])?;
    assert_success(&unchanged)?;
    assert_eq!(json_stdout(&unchanged)?[0]["id"], 41);

    let calls = gh.calls()?;
    assert!(calls.contains("\t--include"));
    assert!(calls.contains("\t--header\tIf-None-Match: \"stable-etag\""));
    Ok(())
}

#[test]
fn paginated_sync_ignores_foreign_issues_and_reads_paginated_comments() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    let gh = FakeGh::new()?;
    let event_id = "11111111-1111-4111-8111-111111111111";
    let foreign = json!({
        "number": 7,
        "body": "ordinary issue",
        "labels": [{"name": "documentation"}],
        "updated_at": "2026-09-22T14:00:00Z"
    });
    let first_page = format!(
        "HTTP/2.0 200 OK\r\netag: \"page-etag\"\r\nlink: <https://api.github.test/issues?page=2>; rel=\"next\"\r\n\r\n{}",
        json!([foreign])
    );
    gh.respond(1, 0, &first_page, "")?;
    gh.respond(
        2,
        0,
        &format!(
            "HTTP/2.0 200 OK\r\n\r\n{}",
            json!([{
                "number": 41,
                "body": trusted_projection_body("Tracked item", None, "pending", event_id, 9001)?,
                "labels": [
                    {"name": "work-tracker:item"},
                    {"name": "work-tracker:status:pending"}
                ],
                "updated_at": "2026-09-22T15:00:00Z"
            }])
        ),
        "",
    )?;
    gh.respond(
        3,
        0,
        &json!([
            [{
                "id": 8000,
                "created_at": "2026-09-22T13:00:00Z",
                "user": {"login": "reader"},
                "body": "unstructured discussion"
            }],
            [{
                "id": 9001,
                "created_at": "2026-09-22T14:30:00Z",
                "user": {"login": "octocat"},
                "body": genesis_body("Tracked item", None, "pending", event_id)
            }]
        ])
        .to_string(),
        "",
    )?;

    let listed = cli.run_with_fake_gh(&gh, ["list"])?;
    assert_success(&listed)?;
    let output = String::from_utf8(listed.stdout)?;
    assert!(output.contains("Tracked item"));
    assert!(!output.contains("ordinary issue"));
    let calls = gh.calls()?;
    assert!(calls.contains("&page=1\t--include"));
    assert!(calls.contains("&page=2\t--include"));
    assert!(calls.contains("issues/41/comments?per_page=100\t--paginate\t--slurp"));
    Ok(())
}

#[test]
fn incompatible_work_tracker_metadata_is_reported_without_adoption() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    let gh = FakeGh::new()?;
    gh.respond(
        1,
        0,
        &json!([[{
            "number": 41,
            "body": "marker is missing",
            "labels": [
                {"name": "work-tracker:item"},
                {"name": "work-tracker:status:active"}
            ],
            "updated_at": "2026-09-22T15:00:00Z"
        }]])
        .to_string(),
        "",
    )?;

    let failed = cli.run_with_fake_gh(&gh, ["--json", "list"])?;
    assert!(!failed.status.success());
    assert!(failed.stdout.is_empty());
    let diagnostic: Value = serde_json::from_slice(&failed.stderr)?;
    assert_eq!(diagnostic["error"]["code"], "github_metadata_collision");
    let connection = Connection::open(cli.github_cache_path("octocat", "work-tracker-data"))?;
    let count: i64 =
        connection.query_row("SELECT count(*) FROM work_items", [], |row| row.get(0))?;
    assert_eq!(count, 0);
    Ok(())
}

#[test]
fn item_that_becomes_foreign_is_removed_with_the_incremental_cursor() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    let gh = FakeGh::new()?;
    let event_id = "11111111-1111-4111-8111-111111111111";
    gh.respond(
        1,
        0,
        &json!([[{
            "number": 41,
            "body": trusted_projection_body("Was tracked", None, "active", event_id, 9001)?,
            "labels": [
                {"name": "work-tracker:item"},
                {"name": "work-tracker:status:active"}
            ],
            "updated_at": "2026-09-22T15:00:00Z"
        }]])
        .to_string(),
        "",
    )?;
    gh.respond(
        2,
        0,
        &json!([[{
            "id": 9001,
            "created_at": "2026-09-22T14:00:00Z",
            "user": {"login": "octocat"},
            "body": genesis_body("Was tracked", None, "active", event_id)
        }]])
        .to_string(),
        "",
    )?;
    let initial = cli.run_with_fake_gh(&gh, ["--json", "list"])?;
    assert_success(&initial)?;
    assert_eq!(
        json_stdout(&initial)?
            .as_array()
            .context("list was not an array")?
            .len(),
        1
    );

    gh.respond(
        3,
        0,
        &json!([[{
            "number": 41,
            "body": "ordinary issue now",
            "labels": [{"name": "documentation"}],
            "updated_at": "2026-09-22T16:00:00Z"
        }]])
        .to_string(),
        "",
    )?;
    let refreshed = cli.run_with_fake_gh(&gh, ["--json", "list"])?;
    assert_success(&refreshed)?;
    assert!(
        json_stdout(&refreshed)?
            .as_array()
            .context("list was not an array")?
            .is_empty()
    );

    let connection = Connection::open(cli.github_cache_path("octocat", "work-tracker-data"))?;
    let count: i64 =
        connection.query_row("SELECT count(*) FROM work_items", [], |row| row.get(0))?;
    assert_eq!(count, 0);
    let cursor: String = connection.query_row(
        "SELECT sync_cursor FROM github_cache_state WHERE repository = ?1",
        ["octocat/work-tracker-data"],
        |row| row.get(0),
    )?;
    assert_eq!(cursor, "2026-09-22T16:00:00.000Z");
    Ok(())
}

#[test]
fn interrupted_cache_commit_rolls_back_items_history_and_cursor_then_retries() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    let gh = FakeGh::new()?;
    let first_event = "11111111-1111-4111-8111-111111111111";
    let second_event = "22222222-2222-4222-8222-222222222222";
    gh.respond(
        1,
        0,
        &json!([[{
            "number": 41,
            "body": trusted_projection_body(
                "Existing item",
                None,
                "active",
                first_event,
                9001
            )?,
            "labels": [
                {"name": "work-tracker:item"},
                {"name": "work-tracker:status:active"}
            ],
            "updated_at": "2026-09-22T15:00:00Z"
        }]])
        .to_string(),
        "",
    )?;
    gh.respond(
        2,
        0,
        &json!([[{
            "id": 9001,
            "created_at": "2026-09-22T14:00:00Z",
            "user": {"login": "octocat"},
            "body": genesis_body("Existing item", None, "active", first_event)
        }]])
        .to_string(),
        "",
    )?;
    assert_success(&cli.run_with_fake_gh(&gh, ["--json", "list"])?)?;

    let cache = cli.github_cache_path("octocat", "work-tracker-data");
    let connection = Connection::open(&cache)?;
    connection.execute_batch(
        "CREATE TRIGGER interrupt_second_history
         BEFORE INSERT ON history_entries WHEN NEW.id = 9002
         BEGIN SELECT RAISE(ABORT, 'injected cache interruption'); END;",
    )?;
    drop(connection);

    let second_issue = json!({
        "number": 42,
        "body": trusted_projection_body("New item", None, "blocked", second_event, 9002)?,
        "labels": [
            {"name": "work-tracker:item"},
            {"name": "work-tracker:status:blocked"}
        ],
        "updated_at": "2026-09-22T16:00:00Z"
    });
    let second_comment = json!({
        "id": 9002,
        "created_at": "2026-09-22T15:30:00Z",
        "user": {"login": "octocat"},
        "body": genesis_body("New item", None, "blocked", second_event)
    });
    gh.respond(3, 0, &json!([[second_issue.clone()]]).to_string(), "")?;
    gh.respond(4, 0, &json!([[second_comment.clone()]]).to_string(), "")?;
    let interrupted = cli.run_with_fake_gh(&gh, ["--json", "list"])?;
    assert!(!interrupted.status.success());

    let connection = Connection::open(&cache)?;
    let count: i64 =
        connection.query_row("SELECT count(*) FROM work_items", [], |row| row.get(0))?;
    assert_eq!(count, 1);
    let cursor: String = connection.query_row(
        "SELECT sync_cursor FROM github_cache_state WHERE repository = ?1",
        ["octocat/work-tracker-data"],
        |row| row.get(0),
    )?;
    assert_eq!(cursor, "2026-09-22T15:00:00.000Z");
    connection.execute_batch("DROP TRIGGER interrupt_second_history;")?;
    drop(connection);

    gh.respond(5, 0, &json!([[second_issue]]).to_string(), "")?;
    gh.respond(6, 0, &json!([[second_comment]]).to_string(), "")?;
    let retried = cli.run_with_fake_gh(&gh, ["--json", "list"])?;
    assert_success(&retried)?;
    assert_eq!(
        json_stdout(&retried)?
            .as_array()
            .context("list was not an array")?
            .len(),
        2
    );
    Ok(())
}

#[test]
fn github_lists_preserve_filters_limits_attention_order_and_viewer_local_day() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    let gh = FakeGh::new()?;
    let seoul = FixedOffset::east_opt(9 * 60 * 60).context("invalid Seoul offset")?;
    let local_date = Utc::now().with_timezone(&seoul).date_naive();
    let local_midnight = seoul
        .from_local_datetime(
            &local_date
                .and_hms_opt(0, 0, 0)
                .context("invalid local midnight")?,
        )
        .single()
        .context("ambiguous local midnight")?
        .with_timezone(&Utc);
    let today = local_midnight + Duration::hours(1);
    let stale = local_midnight - Duration::hours(1);
    let statuses = [
        (41, "blocked", "Stale blocker", stale, 9001),
        (42, "done", "Done today", today, 9002),
        (43, "done", "Done yesterday", stale, 9003),
        (44, "archived", "Archived evidence", stale, 9004),
    ];
    let issues = statuses
        .iter()
        .map(|(number, status, title, occurred_at, comment_id)| {
            let event_id = format!("{number:08}-1111-4111-8111-111111111111");
            Ok(json!({
                "number": number,
                "body": trusted_projection_body(title, None, status, &event_id, *comment_id)?,
                "labels": [
                    {"name": "work-tracker:item"},
                    {"name": format!("work-tracker:status:{status}")}
                ],
                "locked": *status == "archived",
                "updated_at": occurred_at.to_rfc3339()
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    gh.respond(1, 0, &json!([issues]).to_string(), "")?;
    for (index, (number, status, title, occurred_at, comment_id)) in statuses.iter().enumerate() {
        let event_id = format!("{number:08}-1111-4111-8111-111111111111");
        gh.respond(
            index + 2,
            0,
            &json!([[{
                "id": comment_id,
                "created_at": occurred_at.to_rfc3339(),
                "user": {"login": "octocat"},
                "body": genesis_body(title, None, status, &event_id)
            }]])
            .to_string(),
            "",
        )?;
    }
    for call in 6..=11 {
        gh.respond(call, 0, "[[]]", "")?;
    }

    let daily = cli.run_with_fake_gh_and_tz(&gh, "Asia/Seoul", ["--json", "today"])?;
    assert_success(&daily)?;
    let ids = json_stdout(&daily)?
        .as_array()
        .context("today was not an array")?
        .iter()
        .map(|item| item["id"].as_i64().context("item omitted id"))
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(ids, vec![41, 42]);

    let limited = cli.run_with_fake_gh_and_tz(
        &gh,
        "Asia/Seoul",
        ["--json", "list", "--all", "--limit", "1"],
    )?;
    assert_success(&limited)?;
    assert_eq!(json_stdout(&limited)?[0]["id"], 41);

    let done =
        cli.run_with_fake_gh_and_tz(&gh, "Asia/Seoul", ["--json", "list", "--status", "done"])?;
    assert_success(&done)?;
    assert_eq!(
        json_stdout(&done)?
            .as_array()
            .context("done list was not an array")?
            .len(),
        2
    );

    for status in ["archived", "deleted"] {
        let archived =
            cli.run_with_fake_gh_and_tz(&gh, "Asia/Seoul", ["--json", "list", "--status", status])?;
        assert_success(&archived)?;
        let item = &json_stdout(&archived)?[0];
        assert_eq!(item["id"], 44);
        assert_eq!(item["status"], "archived");
        assert_eq!(item["deleted_at"], item["archived_at"]);
    }

    for include in ["--include-archived", "--include-deleted"] {
        let included =
            cli.run_with_fake_gh_and_tz(&gh, "Asia/Seoul", ["--json", "list", "--all", include])?;
        assert_success(&included)?;
        assert_eq!(
            json_stdout(&included)?
                .as_array()
                .context("included list was not an array")?
                .len(),
            4
        );
    }
    Ok(())
}
