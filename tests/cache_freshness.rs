mod support;

use std::fs;

use anyhow::{Context, Result, ensure};
use rusqlite::Connection;
use serde_json::{Value, json};
use support::{CliHarness, FakeGh, assert_success, stderr};

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

fn projection_body() -> String {
    format!(
        "Cached description\n\n<!-- work-tracker:projection\n{}\n-->",
        json!({
            "schema_version": 1,
            "kind": "work_item",
            "event_id": "11111111-1111-4111-8111-111111111111",
            "creation_fingerprint": "fingerprint-41",
            "pending_genesis_event_id": null,
            "genesis_comment_id": 9001,
            "state_revision": 1
        })
    )
}

fn genesis_body() -> String {
    format!(
        "Work Tracker History Entry: created by agent-a\n\n<!-- work-tracker:event\n{}\n-->",
        json!({
            "schema_version": 1,
            "event_id": "11111111-1111-4111-8111-111111111111",
            "kind": "created",
            "actor": "agent-a",
            "github_actor": "octocat",
            "note": "created remotely",
            "initial_values": {
                "title": "Cached item",
                "description": "Cached description",
                "status": "active"
            },
            "occurred_at_source": "github_comment.created_at"
        })
    )
}

fn populate_cache(cli: &CliHarness) -> Result<()> {
    let gh = FakeGh::new()?;
    gh.respond(
        1,
        0,
        &json!([[{
            "number": 41,
            "body": projection_body(),
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
            "body": genesis_body()
        }]])
        .to_string(),
        "",
    )?;
    assert_success(&cli.run_with_fake_gh(&gh, ["--json", "list", "--all"])?).map(|_| ())
}

#[test]
fn json_show_falls_back_to_cache_with_a_structured_stale_warning() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    populate_cache(&cli)?;

    let gh = FakeGh::new()?;
    gh.respond(1, 1, "", "temporary service outage")?;
    let shown = cli.run_with_fake_gh(&gh, ["--json", "show", "41"])?;

    assert_success(&shown)?;
    let item: Value = serde_json::from_slice(&shown.stdout)?;
    assert_eq!(item["id"], 41);
    assert_eq!(item["title"], "Cached item");
    let warning: Value = serde_json::from_str(stderr(&shown)?)?;
    assert_eq!(warning["warning"]["code"], "stale_cache");
    ensure!(warning["warning"]["last_successful_sync_at"].is_string());
    ensure!(
        warning["warning"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("temporary service outage"))
    );
    Ok(())
}

#[test]
fn every_normal_read_attempts_sync_and_human_output_warns_when_stale() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    populate_cache(&cli)?;

    for args in [
        vec!["show", "41"],
        vec!["list", "--all"],
        vec!["today"],
        vec!["history", "41"],
    ] {
        let gh = FakeGh::new()?;
        gh.respond(1, 1, "", "GitHub is unavailable")?;
        let output = cli.run_with_fake_gh(&gh, args)?;
        assert_success(&output)?;
        ensure!(stderr(&output)?.contains("warning: STALE CACHE"));
        ensure!(stderr(&output)?.contains("last successful synchronization"));
        ensure!(gh.calls()?.contains("issues?state=all"));
    }
    Ok(())
}

#[test]
fn fresh_read_fails_instead_of_using_cached_data() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    populate_cache(&cli)?;
    let gh = FakeGh::new()?;
    gh.respond(1, 1, "", "temporary service outage")?;

    let output = cli.run_with_fake_gh(&gh, ["--json", "--fresh", "show", "41"])?;

    ensure!(!output.status.success());
    ensure!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr)?;
    assert_eq!(error["error"]["code"], "github_api_failure");
    Ok(())
}

#[test]
fn offline_read_uses_cache_without_invoking_github() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    populate_cache(&cli)?;
    let gh = FakeGh::new()?;

    let output = cli.run_with_fake_gh(&gh, ["--json", "--offline", "history", "41"])?;

    assert_success(&output)?;
    let history: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(
        history
            .as_array()
            .context("history was not an array")?
            .len(),
        1
    );
    let warning: Value = serde_json::from_slice(&output.stderr)?;
    assert_eq!(warning["warning"]["code"], "offline_cache");
    ensure!(gh.calls().is_err(), "offline read unexpectedly invoked gh");
    Ok(())
}

#[test]
fn cache_is_unavailable_until_one_sync_has_succeeded() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    let gh = FakeGh::new()?;
    gh.respond(1, 1, "", "temporary service outage")?;

    let output = cli.run_with_fake_gh(&gh, ["--json", "list"])?;

    ensure!(!output.status.success());
    ensure!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr)?;
    assert_eq!(error["error"]["code"], "cache_unavailable");
    ensure!(
        error["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("no synchronized cache"))
    );
    Ok(())
}

#[test]
fn offline_write_fails_before_invoking_github() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    let gh = FakeGh::new()?;

    let output = cli.run_with_fake_gh(&gh, ["--json", "--offline", "add", "Nope"])?;

    ensure!(!output.status.success());
    ensure!(output.stdout.is_empty());
    ensure!(stderr(&output)?.contains("writes are unavailable in --offline mode"));
    ensure!(gh.calls().is_err(), "offline write unexpectedly invoked gh");
    Ok(())
}

#[test]
fn fresh_and_offline_cannot_be_combined() -> Result<()> {
    let cli = CliHarness::new()?;
    let output = cli.run(["--json", "--fresh", "list", "--offline"])?;
    ensure!(!output.status.success());
    ensure!(stderr(&output)?.contains("cannot be used together"));
    Ok(())
}

#[test]
fn online_write_preflight_fails_before_recording_local_pending_state() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    let gh = FakeGh::new()?;
    gh.respond(1, 1, "", "temporary service outage")?;

    let output = cli.run_with_fake_gh(&gh, ["--json", "add", "Not recorded"])?;

    ensure!(!output.status.success());
    ensure!(output.stdout.is_empty());
    let connection = Connection::open(cli.github_cache_path("octocat", "work-tracker-data"))?;
    let pending: i64 =
        connection.query_row("SELECT count(*) FROM pending_github_creations", [], |row| {
            row.get(0)
        })?;
    let items: i64 =
        connection.query_row("SELECT count(*) FROM work_items", [], |row| row.get(0))?;
    assert_eq!(pending, 0);
    assert_eq!(items, 0);
    Ok(())
}

#[test]
fn offline_initialization_fails_without_invoking_github() -> Result<()> {
    let cli = CliHarness::new()?;
    let gh = FakeGh::new()?;

    let output = cli.run_with_fake_gh(&gh, ["--offline", "init", "github"])?;

    ensure!(!output.status.success());
    ensure!(stderr(&output)?.contains("unavailable in --offline mode"));
    ensure!(
        gh.calls().is_err(),
        "offline initialization unexpectedly invoked gh"
    );
    Ok(())
}

#[test]
fn offline_path_override_does_not_validate_through_github() -> Result<()> {
    let cli = CliHarness::new()?;
    let gh = FakeGh::new()?;

    let output = cli.run_with_fake_gh(
        &gh,
        [
            "--json",
            "--offline",
            "--repository",
            "octocat/work-tracker-data",
            "path",
        ],
    )?;

    assert_success(&output)?;
    let path: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(path["repository"], "octocat/work-tracker-data");
    ensure!(gh.calls().is_err(), "offline path unexpectedly invoked gh");
    Ok(())
}

#[test]
fn repository_not_found_does_not_fall_back_to_cached_data() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    populate_cache(&cli)?;
    let gh = FakeGh::new()?;
    gh.respond(1, 1, "", "HTTP 404 Not Found")?;

    let output = cli.run_with_fake_gh(&gh, ["--json", "show", "41"])?;

    ensure!(!output.status.success());
    ensure!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr)?;
    assert_eq!(error["error"]["code"], "github_not_found");
    Ok(())
}
