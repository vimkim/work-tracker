mod support;

use anyhow::{Context, Result, ensure};
use serde_json::Value;
use support::{CliHarness, FakeGh, assert_success, stderr};

const PRIVATE_REPOSITORY: &str = r#"{"full_name":"octocat/work-tracker-data","private":true,"has_issues":true,"permissions":{"admin":true,"push":true}}"#;

fn json(output: &std::process::Output) -> Result<Value> {
    serde_json::from_slice(&output.stdout).context("stdout was not valid JSON")
}

fn initialize(cli: &CliHarness, gh: &FakeGh) -> Result<()> {
    gh.respond(1, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(2, 1, "", "gh: Not Found (HTTP 404)\n")?;
    gh.respond(3, 0, PRIVATE_REPOSITORY, "")?;
    gh.respond(4, 0, "[[]]", "")?;
    assert_success(&cli.run_with_fake_gh(gh, ["init", "github"])?)
}

fn field_value<'a>(calls: &'a str, endpoint: &str, field: &str, next: &str) -> Result<&'a str> {
    let call = calls
        .find(endpoint)
        .with_context(|| format!("missing call to {endpoint}"))?;
    let marker = format!("\t--field\t{field}=");
    let start = calls[call..]
        .find(&marker)
        .map(|offset| call + offset + marker.len())
        .with_context(|| format!("missing {field} field for {endpoint}"))?;
    let end = calls[start..]
        .find(next)
        .map(|offset| start + offset)
        .with_context(|| format!("missing terminator for {field} field"))?;
    Ok(&calls[start..end])
}

fn last_field_value<'a>(
    calls: &'a str,
    endpoint: &str,
    field: &str,
    next: &str,
) -> Result<&'a str> {
    let call = calls
        .rfind(endpoint)
        .with_context(|| format!("missing call to {endpoint}"))?;
    let marker = format!("\t--field\t{field}=");
    let start = calls[call..]
        .find(&marker)
        .map(|offset| call + offset + marker.len())
        .with_context(|| format!("missing {field} field for {endpoint}"))?;
    let end = calls[start..]
        .find(next)
        .map(|offset| start + offset)
        .with_context(|| format!("missing terminator for {field} field"))?;
    Ok(&calls[start..end])
}

fn create_item(cli: &CliHarness, gh: &FakeGh) -> Result<(String, String)> {
    gh.respond(13, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(14, 0, r#"{"number":41}"#, "")?;
    gh.respond(
        15,
        0,
        r#"{"id":9001,"created_at":"2026-09-23T01:02:04Z","user":{"login":"octocat"}}"#,
        "",
    )?;
    gh.respond(16, 0, "{}", "")?;
    assert_success(&cli.run_with_fake_gh(
        gh,
        [
            "--json",
            "add",
            "Watch CI",
            "--description",
            "Initial details",
            "--actor",
            "agent-a",
        ],
    )?)?;
    let calls = gh.calls()?;
    let projection = field_value(
        &calls,
        "\tPATCH\trepos/octocat/work-tracker-data/issues/41",
        "body",
        "\t--field\tlabels[]=",
    )?
    .to_owned();
    let genesis = field_value(
        &calls,
        "\tPOST\trepos/octocat/work-tracker-data/issues/41/comments",
        "body",
        "\nCALL\tapi\t--method\tPATCH\t",
    )?
    .to_owned();
    Ok((projection, genesis))
}

fn updated_comment(
    event_id: &str,
    actor: &str,
    github_actor: &str,
    expected_revision: u64,
    note: Option<&str>,
    changes: Value,
) -> String {
    let note_text = note
        .map(|note| format!("\n\nNote: {note}"))
        .unwrap_or_default();
    format!(
        "Work Tracker Mutation Proposal: updated by {actor}{note_text}\n\n<!-- work-tracker:event\n{}\n-->",
        serde_json::json!({
            "schema_version": 1,
            "event_id": event_id,
            "kind": "updated",
            "actor": actor,
            "github_actor": github_actor,
            "note": note,
            "changes": changes,
            "expected_state_revision": expected_revision
        })
    )
}

#[test]
fn accepted_field_update_advances_revision_once_and_records_exact_changes() -> Result<()> {
    let cli = CliHarness::new()?;
    let gh = FakeGh::new()?;
    initialize(&cli, &gh)?;
    let (projection, genesis) = create_item(&cli, &gh)?;

    gh.respond(17, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(
        18,
        0,
        &serde_json::json!({
            "number": 41,
            "title": "Watch CI",
            "body": projection,
            "labels": [
                {"name": "work-tracker:item"},
                {"name": "work-tracker:status:pending"}
            ]
        })
        .to_string(),
        "",
    )?;
    gh.respond(
        19,
        0,
        &serde_json::json!([[{
            "id": 9001,
            "created_at": "2026-09-23T01:02:04Z",
            "user": {"login": "octocat"},
            "body": genesis
        }]])
        .to_string(),
        "",
    )?;
    gh.respond(
        20,
        0,
        r#"{"id":9002,"created_at":"2026-09-23T01:03:04Z","user":{"login":"octocat"}}"#,
        "",
    )?;
    let changes = serde_json::json!({
        "title": {"from": "Watch CI", "to": "Watch release CI"},
        "description": {"from": "Initial details", "to": "Final details"}
    });
    let update = updated_comment(
        "update-fixed-1",
        "agent-b",
        "octocat",
        1,
        Some("clarify outcome"),
        changes.clone(),
    );
    gh.respond(
        21,
        0,
        &serde_json::json!([[
            {
                "id": 9001,
                "created_at": "2026-09-23T01:02:04Z",
                "user": {"login": "octocat"},
                "body": genesis
            },
            {
                "id": 9002,
                "created_at": "2026-09-23T01:03:04Z",
                "user": {"login": "octocat"},
                "body": update
            }
        ]])
        .to_string(),
        "",
    )?;
    gh.respond(22, 0, "{}", "")?;

    let failed = cli.run_with_fake_gh(
        &gh,
        [
            "--json",
            "update",
            "41",
            "--title",
            "Watch release CI",
            "--description",
            "Final details",
            "--note",
            "clarify outcome",
            "--actor",
            "agent-b",
            "--event-id",
            "update-fixed-1",
        ],
    )?;
    ensure!(
        failed.status.success(),
        "update failed before confirmation fixture could be completed: {}",
        stderr(&failed)?
    );

    let item = json(&failed)?;
    assert_eq!(item["title"], "Watch release CI");
    assert_eq!(item["description"], "Final details");
    let calls = gh.calls()?;
    ensure!(calls.contains("\"kind\":\"updated\""));
    ensure!(calls.contains("\"expected_state_revision\":1"));
    ensure!(calls.contains("\"title\":{\"from\":\"Watch CI\",\"to\":\"Watch release CI\"}"));
    ensure!(
        calls.contains("\"description\":{\"from\":\"Initial details\",\"to\":\"Final details\"}")
    );
    ensure!(calls.contains("\t--field\ttitle=Watch release CI"));

    let update_body = last_field_value(
        &calls,
        "\tPOST\trepos/octocat/work-tracker-data/issues/41/comments",
        "body",
        "\nCALL\tapi\t--method\tGET\t",
    )?;
    ensure!(update_body.contains("Work Tracker Mutation Proposal: updated by agent-b"));
    ensure!(update_body.contains("Note: clarify outcome"));

    let cache = cli.github_cache_path("octocat", "work-tracker-data");
    let cached = cli.run([
        "--json",
        "--database",
        cache.to_str().context("cache path was not UTF-8")?,
        "history",
        "41",
    ])?;
    assert_success(&cached)?;
    let history = json(&cached)?;
    assert_eq!(
        history
            .as_array()
            .context("history was not an array")?
            .len(),
        2
    );
    assert_eq!(history[1]["kind"], "updated");
    assert_eq!(history[1]["state_revision"], 2);
    assert_eq!(history[1]["changes"], changes);
    assert_eq!(
        history[1]["previous_history_hash"],
        history[0]["history_hash"]
    );
    ensure!(history[1]["history_hash"] != history[0]["history_hash"]);
    Ok(())
}

#[test]
fn no_op_field_update_publishes_no_proposal_and_appends_no_history() -> Result<()> {
    let cli = CliHarness::new()?;
    let gh = FakeGh::new()?;
    initialize(&cli, &gh)?;
    let (projection, genesis) = create_item(&cli, &gh)?;
    gh.respond(17, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(
        18,
        0,
        &serde_json::json!({
            "number": 41,
            "title": "Watch CI",
            "body": projection,
            "labels": [
                {"name": "work-tracker:item"},
                {"name": "work-tracker:status:pending"}
            ]
        })
        .to_string(),
        "",
    )?;
    gh.respond(
        19,
        0,
        &serde_json::json!([[{
            "id": 9001,
            "created_at": "2026-09-23T01:02:04Z",
            "user": {"login": "octocat"},
            "body": genesis
        }]])
        .to_string(),
        "",
    )?;

    let unchanged = cli.run_with_fake_gh(
        &gh,
        [
            "update",
            "41",
            "--title",
            " Watch CI ",
            "--description",
            " Initial details ",
            "--actor",
            "agent-b",
        ],
    )?;
    assert_success(&unchanged)?;
    assert_eq!(stderr(&unchanged)?, "");
    let rendered = std::str::from_utf8(&unchanged.stdout)?;
    ensure!(rendered.contains("Watch CI"));
    ensure!(rendered.contains("Initial details"));

    let calls = gh.calls()?;
    assert_eq!(
        calls
            .matches("\tPOST\trepos/octocat/work-tracker-data/issues/41/comments")
            .count(),
        1,
        "only the genesis comment should exist"
    );
    let cache = cli.github_cache_path("octocat", "work-tracker-data");
    let cached = cli.run([
        "--json",
        "--database",
        cache.to_str().context("cache path was not UTF-8")?,
        "history",
        "41",
    ])?;
    assert_success(&cached)?;
    assert_eq!(
        json(&cached)?
            .as_array()
            .context("history was not an array")?
            .len(),
        1
    );
    Ok(())
}

#[test]
fn first_proposal_in_comment_order_wins_and_stale_loser_gets_current_state() -> Result<()> {
    let cli = CliHarness::new()?;
    let gh = FakeGh::new()?;
    initialize(&cli, &gh)?;
    let (projection, genesis) = create_item(&cli, &gh)?;
    let winner_changes = serde_json::json!({
        "title": {"from": "Watch CI", "to": "Winner title"}
    });
    let loser_changes = serde_json::json!({
        "title": {"from": "Watch CI", "to": "Loser title"}
    });
    let winner = updated_comment(
        "update-winner",
        "agent-a",
        "other-user",
        1,
        None,
        winner_changes.clone(),
    );
    let loser = updated_comment(
        "update-loser",
        "agent-b",
        "octocat",
        1,
        Some("my attempt"),
        loser_changes,
    );
    gh.respond(17, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(
        18,
        0,
        &serde_json::json!({
            "number": 41,
            "title": "Watch CI",
            "body": projection,
            "labels": [
                {"name": "work-tracker:item"},
                {"name": "work-tracker:status:pending"}
            ]
        })
        .to_string(),
        "",
    )?;
    gh.respond(
        19,
        0,
        &serde_json::json!([[{
            "id": 9001,
            "created_at": "2026-09-23T01:02:04Z",
            "user": {"login": "octocat"},
            "body": genesis
        }]])
        .to_string(),
        "",
    )?;
    gh.respond(
        20,
        0,
        r#"{"id":9003,"created_at":"2026-09-23T01:04:04Z","user":{"login":"octocat"}}"#,
        "",
    )?;
    gh.respond(
        21,
        0,
        &serde_json::json!([[
            {
                "id": 9003,
                "created_at": "2026-09-23T01:04:04Z",
                "user": {"login": "octocat"},
                "body": loser
            },
            {
                "id": 9001,
                "created_at": "2026-09-23T01:02:04Z",
                "user": {"login": "octocat"},
                "body": genesis
            },
            {
                "id": 9002,
                "created_at": "2026-09-23T01:03:04Z",
                "user": {"login": "other-user"},
                "body": winner
            }
        ]])
        .to_string(),
        "",
    )?;
    gh.respond(22, 0, "{}", "")?;

    let conflicted = cli.run_with_fake_gh(
        &gh,
        [
            "--json",
            "update",
            "41",
            "--title",
            "Loser title",
            "--note",
            "my attempt",
            "--actor",
            "agent-b",
            "--event-id",
            "update-loser",
        ],
    )?;
    ensure!(!conflicted.status.success());
    assert_eq!(conflicted.stdout, b"");
    let error: Value = serde_json::from_slice(&conflicted.stderr)?;
    assert_eq!(error["error"]["code"], "github_state_conflict");
    assert_eq!(error["error"]["kind"], "rejected_mutation");
    assert_eq!(error["error"]["event_id"], "update-loser");
    assert_eq!(error["error"]["expected_state_revision"], 1);
    assert_eq!(error["error"]["current_state_revision"], 2);
    assert_eq!(error["error"]["current_values"]["title"], "Winner title");
    assert_eq!(
        error["error"]["current_values"]["description"],
        "Initial details"
    );
    assert_eq!(error["error"]["current_values"]["status"], "pending");
    ensure!(
        error["error"]["instruction"]
            .as_str()
            .context("missing retry instruction")?
            .contains("refresh")
    );
    ensure!(
        error["error"]["instruction"]
            .as_str()
            .context("missing retry instruction")?
            .contains("new proposal")
    );

    let cache = cli.github_cache_path("octocat", "work-tracker-data");
    let cached = cli.run([
        "--json",
        "--database",
        cache.to_str().context("cache path was not UTF-8")?,
        "history",
        "41",
    ])?;
    assert_success(&cached)?;
    let history = json(&cached)?;
    assert_eq!(
        history
            .as_array()
            .context("history was not an array")?
            .len(),
        2
    );
    assert_eq!(history[1]["event_id"], "update-winner");
    assert_eq!(history[1]["state_revision"], 2);
    assert_eq!(history[1]["changes"], winner_changes);
    let calls = gh.calls()?;
    ensure!(calls.contains("\"event_id\":\"update-loser\""));
    ensure!(calls.contains("\"head_event_id\":\"update-winner\""));
    ensure!(calls.contains("\t--field\ttitle=Winner title"));

    let repaired_projection = format!(
        "{}\n-->",
        last_field_value(
            &calls,
            "\tPATCH\trepos/octocat/work-tracker-data/issues/41",
            "body",
            "\n-->\n",
        )?
    );
    let retry_changes = serde_json::json!({
        "title": {"from": "Winner title", "to": "Retried title"}
    });
    let retry = updated_comment(
        "update-after-refresh",
        "agent-b",
        "octocat",
        2,
        Some("still needed after refresh"),
        retry_changes.clone(),
    );
    gh.respond(23, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(
        24,
        0,
        &serde_json::json!({
            "number": 41,
            "title": "Winner title",
            "body": repaired_projection,
            "labels": [
                {"name": "work-tracker:item"},
                {"name": "work-tracker:status:pending"}
            ]
        })
        .to_string(),
        "",
    )?;
    let prior_comments = serde_json::json!([
        {
            "id": 9001,
            "created_at": "2026-09-23T01:02:04Z",
            "user": {"login": "octocat"},
            "body": genesis
        },
        {
            "id": 9002,
            "created_at": "2026-09-23T01:03:04Z",
            "user": {"login": "other-user"},
            "body": winner
        },
        {
            "id": 9003,
            "created_at": "2026-09-23T01:04:04Z",
            "user": {"login": "octocat"},
            "body": loser
        }
    ]);
    gh.respond(25, 0, &serde_json::json!([prior_comments]).to_string(), "")?;
    gh.respond(
        26,
        0,
        r#"{"id":9004,"created_at":"2026-09-23T01:05:04Z","user":{"login":"octocat"}}"#,
        "",
    )?;
    let mut confirmed = prior_comments
        .as_array()
        .context("comments fixture was not an array")?
        .clone();
    confirmed.push(serde_json::json!({
        "id": 9004,
        "created_at": "2026-09-23T01:05:04Z",
        "user": {"login": "octocat"},
        "body": retry
    }));
    gh.respond(27, 0, &serde_json::json!([confirmed]).to_string(), "")?;
    gh.respond(28, 0, "{}", "")?;

    let retried = cli.run_with_fake_gh(
        &gh,
        [
            "--json",
            "update",
            "41",
            "--title",
            "Retried title",
            "--note",
            "still needed after refresh",
            "--actor",
            "agent-b",
            "--event-id",
            "update-after-refresh",
        ],
    )?;
    assert_success(&retried)?;
    assert_eq!(json(&retried)?["title"], "Retried title");
    let history = cli.run([
        "--json",
        "--database",
        cache.to_str().context("cache path was not UTF-8")?,
        "history",
        "41",
    ])?;
    assert_success(&history)?;
    let history = json(&history)?;
    assert_eq!(
        history
            .as_array()
            .context("history was not an array")?
            .len(),
        3
    );
    assert_eq!(history[2]["event_id"], "update-after-refresh");
    assert_eq!(history[2]["state_revision"], 3);
    assert_eq!(history[2]["changes"], retry_changes);
    assert_eq!(
        history[2]["previous_history_hash"],
        history[1]["history_hash"]
    );
    let calls = gh.calls()?;
    ensure!(calls.contains("\"event_id\":\"update-after-refresh\""));
    ensure!(calls.contains("\"expected_state_revision\":2"));
    Ok(())
}

#[test]
fn accepted_event_survives_projection_failure_and_sync_repairs_it() -> Result<()> {
    let cli = CliHarness::new()?;
    let gh = FakeGh::new()?;
    initialize(&cli, &gh)?;
    let (projection, genesis) = create_item(&cli, &gh)?;
    let changes = serde_json::json!({
        "description": {"from": "Initial details", "to": "Recovered details"}
    });
    let update = updated_comment(
        "update-before-patch-failure",
        "agent-b",
        "octocat",
        1,
        None,
        changes.clone(),
    );
    let issue = serde_json::json!({
        "number": 41,
        "title": "Watch CI",
        "body": projection,
        "labels": [
            {"name": "work-tracker:item"},
            {"name": "work-tracker:status:pending"}
        ]
    });
    let genesis_comment = serde_json::json!({
        "id": 9001,
        "created_at": "2026-09-23T01:02:04Z",
        "user": {"login": "octocat"},
        "body": genesis
    });
    let update_comment = serde_json::json!({
        "id": 9002,
        "created_at": "2026-09-23T01:03:04Z",
        "user": {"login": "octocat"},
        "body": update
    });
    gh.respond(17, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(18, 0, &issue.to_string(), "")?;
    gh.respond(
        19,
        0,
        &serde_json::json!([[genesis_comment]]).to_string(),
        "",
    )?;
    gh.respond(
        20,
        0,
        r#"{"id":9002,"created_at":"2026-09-23T01:03:04Z","user":{"login":"octocat"}}"#,
        "",
    )?;
    gh.respond(
        21,
        0,
        &serde_json::json!([[genesis_comment, update_comment]]).to_string(),
        "",
    )?;
    gh.respond(22, 1, "", "gh: service unavailable (HTTP 503)\n")?;

    let failed = cli.run_with_fake_gh(
        &gh,
        [
            "--json",
            "update",
            "41",
            "--description",
            "Recovered details",
            "--actor",
            "agent-b",
            "--event-id",
            "update-before-patch-failure",
        ],
    )?;
    ensure!(!failed.status.success());
    assert_eq!(failed.stdout, b"");
    let error: Value = serde_json::from_slice(&failed.stderr)?;
    assert_eq!(error["error"]["code"], "github_api_failure");

    let stale_issue = serde_json::json!([[
        {
            "number": 41,
            "title": "Watch CI",
            "body": issue["body"],
            "labels": issue["labels"],
            "updated_at": "2026-09-23T01:03:04Z"
        }
    ]]);
    gh.respond(23, 0, &stale_issue.to_string(), "")?;
    gh.respond(
        24,
        0,
        &serde_json::json!([[genesis_comment, update_comment]]).to_string(),
        "",
    )?;
    gh.respond(25, 0, "{}", "")?;

    let recovered = cli.run_with_fake_gh(&gh, ["--json", "history", "41"])?;
    assert_success(&recovered)?;
    let history = json(&recovered)?;
    assert_eq!(
        history
            .as_array()
            .context("history was not an array")?
            .len(),
        2
    );
    assert_eq!(history[1]["event_id"], "update-before-patch-failure");
    assert_eq!(history[1]["state_revision"], 2);
    assert_eq!(history[1]["changes"], changes);
    let calls = gh.calls()?;
    assert_eq!(
        calls
            .matches("\tPOST\trepos/octocat/work-tracker-data/issues/41/comments")
            .count(),
        2,
        "recovery must not publish a duplicate proposal"
    );
    ensure!(calls.contains("\t--field\tbody=Recovered details"));
    ensure!(calls.contains("\"state_revision\":2"));
    ensure!(calls.contains("\"head_event_id\":\"update-before-patch-failure\""));
    Ok(())
}

#[test]
fn update_requires_successful_online_preflight_before_reading_or_recording_state() -> Result<()> {
    let cli = CliHarness::new()?;
    let gh = FakeGh::new()?;
    initialize(&cli, &gh)?;
    create_item(&cli, &gh)?;
    gh.respond(17, 1, "", "temporary service outage")?;

    let failed = cli.run_with_fake_gh(
        &gh,
        [
            "--json",
            "update",
            "41",
            "--title",
            "Must not be proposed",
            "--actor",
            "agent-b",
        ],
    )?;
    ensure!(!failed.status.success());
    assert_eq!(failed.stdout, b"");
    let error: Value = serde_json::from_slice(&failed.stderr)?;
    assert_eq!(error["error"]["code"], "github_api_failure");

    let calls = gh.calls()?;
    assert_eq!(
        calls
            .matches("\tGET\trepos/octocat/work-tracker-data/issues/41")
            .count(),
        0,
        "preflight failure must happen before loading remote state"
    );
    assert_eq!(
        calls
            .matches("\tPOST\trepos/octocat/work-tracker-data/issues/41/comments")
            .count(),
        1,
        "only the genesis comment should exist"
    );
    let cache = cli.github_cache_path("octocat", "work-tracker-data");
    let history = cli.run([
        "--json",
        "--database",
        cache.to_str().context("cache path was not UTF-8")?,
        "history",
        "41",
    ])?;
    assert_success(&history)?;
    assert_eq!(
        json(&history)?
            .as_array()
            .context("history was not an array")?
            .len(),
        1
    );
    Ok(())
}
