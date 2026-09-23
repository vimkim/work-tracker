mod support;

use anyhow::{Context, Result, ensure};
use serde_json::Value;
use support::{CliHarness, FakeGh, assert_success, stderr, stdout};

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
    assert_success(
        &cli.run_with_fake_gh(gh, ["--json", "add", "Watch CI", "--actor", "agent-a"])?,
    )?;
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

fn note_comment(event_id: &str, actor: &str, github_actor: &str, note: &str) -> String {
    format!(
        "Work Tracker History Entry: noted by {actor}\n\n<!-- work-tracker:event\n{}\n-->",
        serde_json::json!({
            "schema_version": 1,
            "event_id": event_id,
            "kind": "noted",
            "actor": actor,
            "github_actor": github_actor,
            "note": note,
            "changes": {}
        })
    )
}

fn without_projection_head(body: &str) -> Result<String> {
    let marker = "\n\n<!-- work-tracker:projection\n";
    let start = body
        .rfind(marker)
        .context("projection fixture omitted metadata")?;
    let encoded_start = start + marker.len();
    let encoded_end = body[encoded_start..]
        .find("\n-->")
        .map(|offset| encoded_start + offset)
        .context("projection fixture metadata was unterminated")?;
    let mut metadata: Value = serde_json::from_str(&body[encoded_start..encoded_end])?;
    let object = metadata
        .as_object_mut()
        .context("projection fixture metadata was not an object")?;
    object.remove("head_event_id");
    object.remove("head_comment_id");
    object.remove("history_hash");
    Ok(format!(
        "{}{}{}\n-->",
        &body[..start],
        marker,
        serde_json::to_string(&metadata)?
    ))
}

#[test]
fn note_publishes_one_canonical_event_and_returns_trusted_attribution() -> Result<()> {
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
    gh.respond(
        21,
        0,
        &serde_json::json!([[{
            "id": 9001,
            "created_at": "2026-09-23T01:02:04Z",
            "user": {"login": "octocat"},
            "body": genesis
        }, {
            "id": 9002,
            "created_at": "2026-09-23T01:03:04Z",
            "user": {"login": "octocat"},
            "body": note_comment("note-fixed-1", "agent-b", "octocat", "deployment queued")
        }]])
        .to_string(),
        "",
    )?;
    gh.respond(22, 0, "{}", "")?;

    let noted = cli.run_with_fake_gh(
        &gh,
        [
            "--json",
            "note",
            "41",
            "deployment queued",
            "--actor",
            "agent-b",
            "--event-id",
            "note-fixed-1",
        ],
    )?;
    assert_success(&noted)?;
    assert_eq!(stderr(&noted)?, "");
    let entry = json(&noted)?;
    assert_eq!(entry["id"], 9002);
    assert_eq!(entry["work_item_id"], 41);
    assert_eq!(entry["event_id"], "note-fixed-1");
    assert_eq!(entry["kind"], "noted");
    assert_eq!(entry["actor"], "agent-b");
    assert_eq!(entry["github_actor"], "octocat");
    assert_eq!(entry["occurred_at"], "2026-09-23T01:03:04Z");
    assert_eq!(entry["note"], "deployment queued");
    assert_eq!(entry["changes"], serde_json::json!({}));
    assert_eq!(entry["state_revision"], 1);
    ensure!(entry["history_hash"].as_str().is_some());

    let calls = gh.calls()?;
    assert_eq!(
        calls
            .matches("\tPOST\trepos/octocat/work-tracker-data/issues/41/comments")
            .count(),
        2,
        "genesis and one note comment were expected"
    );
    assert_eq!(
        calls
            .matches("\tGET\trepos/octocat/work-tracker-data/issues/41/comments?per_page=100")
            .count(),
        2,
        "note publication must be confirmed from GitHub before projection and cache updates"
    );
    ensure!(calls.contains("Work Tracker History Entry: noted by agent-b"));
    ensure!(calls.contains("\"schema_version\":1"));
    ensure!(calls.contains("\"event_id\":\"note-fixed-1\""));
    ensure!(calls.contains("\"kind\":\"noted\""));
    ensure!(calls.contains("\"actor\":\"agent-b\""));
    ensure!(calls.contains("\"github_actor\":\"octocat\""));
    ensure!(calls.contains("\"note\":\"deployment queued\""));
    ensure!(calls.contains("\"changes\":{}"));
    ensure!(calls.contains("\"state_revision\":1"));
    ensure!(calls.contains("\"head_comment_id\":9002"));
    ensure!(calls.contains("\"history_hash\":"));
    Ok(())
}

#[test]
fn history_replays_github_comment_order_ignores_discussion_and_populates_the_cache() -> Result<()> {
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
    gh.respond(
        21,
        0,
        &serde_json::json!([[{
            "id": 9001,
            "created_at": "2026-09-23T01:02:04Z",
            "user": {"login": "octocat"},
            "body": genesis
        }, {
            "id": 9002,
            "created_at": "2026-09-23T01:03:04Z",
            "user": {"login": "octocat"},
            "body": note_comment("note-fixed-1", "agent-b", "octocat", "deployment queued")
        }]])
        .to_string(),
        "",
    )?;
    gh.respond(22, 0, "{}", "")?;
    assert_success(&cli.run_with_fake_gh(
        &gh,
        [
            "--json",
            "note",
            "41",
            "deployment queued",
            "--actor",
            "agent-b",
            "--event-id",
            "note-fixed-1",
        ],
    )?)?;

    let calls = gh.calls()?;
    let completed_projection = format!(
        "{}\n-->",
        last_field_value(
            &calls,
            "\tPATCH\trepos/octocat/work-tracker-data/issues/41",
            "body",
            "\n-->\n",
        )?
    );
    let note_event = last_field_value(
        &calls,
        "\tPOST\trepos/octocat/work-tracker-data/issues/41/comments",
        "body",
        "\nCALL\tapi\t--method\tGET\trepos/octocat/work-tracker-data/issues/41/comments",
    )?
    .to_owned();
    let issue = serde_json::json!([[{
        "number": 41,
        "body": completed_projection,
        "labels": [
            {"name": "work-tracker:item"},
            {"name": "work-tracker:status:pending"}
        ],
        "updated_at": "2026-09-23T01:03:04Z"
    }]])
    .to_string();
    let comments = serde_json::json!([[
        {
            "id": 9010,
            "created_at": "2026-09-23T01:04:00Z",
            "user": {"login": "reviewer"},
            "body": "Does the work-tracker:event marker need documentation? This is ordinary discussion."
        },
        {
            "id": 9002,
            "created_at": "2026-09-23T01:03:04Z",
            "user": {"login": "octocat"},
            "body": note_event
        },
        {
            "id": 9001,
            "created_at": "2026-09-23T01:02:04Z",
            "user": {"login": "octocat"},
            "body": genesis
        }
    ]])
    .to_string();
    gh.respond(23, 0, &issue, "")?;
    gh.respond(24, 0, &comments, "")?;

    let replayed = cli.run_with_fake_gh(&gh, ["--json", "history", "41"])?;
    assert_success(&replayed)?;
    let entries = json(&replayed)?;
    let entries = entries.as_array().context("history was not an array")?;
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["id"], 9001);
    assert_eq!(entries[0]["kind"], "created");
    assert_eq!(entries[0]["github_actor"], "octocat");
    assert_eq!(entries[0]["changes"]["title"], "Watch CI");
    assert_eq!(entries[0]["state_revision"], 1);
    assert_eq!(entries[1]["id"], 9002);
    assert_eq!(entries[1]["kind"], "noted");
    assert_eq!(entries[1]["state_revision"], 1);
    assert_eq!(
        entries[1]["previous_history_hash"],
        entries[0]["history_hash"]
    );
    ensure!(entries[0]["history_hash"] != entries[1]["history_hash"]);
    let expected = entries.clone();

    let cache = cli.github_cache_path("octocat", "work-tracker-data");
    let cached = cli.run([
        "--json",
        "--database",
        cache.to_str().context("cache path was not UTF-8")?,
        "history",
        "41",
    ])?;
    assert_success(&cached)?;
    assert_eq!(json(&cached)?, Value::Array(expected.clone()));

    let offline_gh = FakeGh::new()?;
    let offline = cli.run_with_fake_gh(&offline_gh, ["--json", "--offline", "history", "41"])?;
    assert_success(&offline)?;
    assert_eq!(json(&offline)?, Value::Array(expected));
    let warning: Value =
        serde_json::from_slice(&offline.stderr).context("offline warning was not valid JSON")?;
    assert_eq!(warning["warning"]["code"], "offline_cache");
    ensure!(
        offline_gh.calls().is_err(),
        "offline trusted history contacted GitHub"
    );

    let human = cli.run([
        "--database",
        cache.to_str().context("cache path was not UTF-8")?,
        "history",
        "41",
    ])?;
    assert_success(&human)?;
    let rendered = stdout(&human)?;
    ensure!(rendered.contains("noted"));
    ensure!(rendered.contains("by agent-b (GitHub: octocat)"));
    ensure!(rendered.contains("Event: note-fixed-1"));
    ensure!(rendered.contains("State revision: 1"));
    ensure!(rendered.contains("History hash:"));
    Ok(())
}

#[test]
fn concurrent_notes_are_both_accepted_in_comment_order_without_advancing_state_revision()
-> Result<()> {
    let cli = CliHarness::new()?;
    let gh = FakeGh::new()?;
    initialize(&cli, &gh)?;
    let (projection, genesis) = create_item(&cli, &gh)?;
    gh.respond(
        17,
        0,
        &serde_json::json!([[{
            "number": 41,
            "body": projection,
            "labels": [
                {"name": "work-tracker:item"},
                {"name": "work-tracker:status:pending"}
            ],
            "updated_at": "2026-09-23T01:04:04Z"
        }]])
        .to_string(),
        "",
    )?;
    gh.respond(
        18,
        0,
        &serde_json::json!([[{
            "id": 9003,
            "created_at": "2026-09-23T01:04:04Z",
            "user": {"login": "octocat"},
            "body": note_comment("note-b", "agent-b", "octocat", "second by GitHub order")
        }, {
            "id": 9001,
            "created_at": "2026-09-23T01:02:04Z",
            "user": {"login": "octocat"},
            "body": genesis
        }, {
            "id": 9002,
            "created_at": "2026-09-23T01:03:04Z",
            "user": {"login": "other-user"},
            "body": note_comment("note-a", "agent-a", "other-user", "first by GitHub order")
        }]])
        .to_string(),
        "",
    )?;
    gh.respond(19, 0, "{}", "")?;
    let output = cli.run_with_fake_gh(&gh, ["--json", "history", "41"])?;
    assert_success(&output)?;
    let history = json(&output)?;
    assert_eq!(
        history
            .as_array()
            .context("history was not an array")?
            .len(),
        3
    );
    assert_eq!(history[1]["event_id"], "note-a");
    assert_eq!(history[1]["github_actor"], "other-user");
    assert_eq!(history[1]["state_revision"], 1);
    assert_eq!(history[2]["event_id"], "note-b");
    assert_eq!(history[2]["state_revision"], 1);
    assert_eq!(
        history[2]["previous_history_hash"],
        history[1]["history_hash"]
    );
    let calls = gh.calls()?;
    ensure!(calls.contains("\tPATCH\trepos/octocat/work-tracker-data/issues/41"));
    ensure!(calls.contains("\"head_event_id\":\"note-b\""));
    ensure!(calls.contains("\"head_comment_id\":9003"));
    ensure!(calls.contains("\"state_revision\":1"));
    Ok(())
}

#[test]
fn unknown_event_schema_is_reported_and_never_cached_as_history() -> Result<()> {
    let cli = CliHarness::new()?;
    let gh = FakeGh::new()?;
    initialize(&cli, &gh)?;
    let (projection, genesis) = create_item(&cli, &gh)?;
    let future = format!(
        "Work Tracker future event\n\n<!-- work-tracker:event\n{}\n-->",
        serde_json::json!({
            "schema_version": 99,
            "event_id": "future-1",
            "kind": "noted",
            "actor": "future-agent",
            "github_actor": "octocat",
            "note": "do not guess",
            "changes": {}
        })
    );
    gh.respond(
        17,
        0,
        &serde_json::json!([[{
            "number": 41,
            "body": projection,
            "labels": [
                {"name": "work-tracker:item"},
                {"name": "work-tracker:status:pending"}
            ],
            "updated_at": "2026-09-23T01:03:04Z"
        }]])
        .to_string(),
        "",
    )?;
    gh.respond(
        18,
        0,
        &serde_json::json!([[{
            "id": 9001,
            "created_at": "2026-09-23T01:02:04Z",
            "user": {"login": "octocat"},
            "body": genesis
        }, {
            "id": 9002,
            "created_at": "2026-09-23T01:03:04Z",
            "user": {"login": "octocat"},
            "body": future
        }]])
        .to_string(),
        "",
    )?;

    let failed = cli.run_with_fake_gh(&gh, ["--json", "history", "41"])?;
    ensure!(!failed.status.success());
    assert_eq!(failed.stdout, b"");
    let diagnostic: Value = serde_json::from_slice(&failed.stderr)?;
    assert_eq!(diagnostic["error"]["code"], "github_unknown_event_schema");
    ensure!(
        diagnostic["error"]["message"]
            .as_str()
            .context("missing error message")?
            .contains("99")
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
fn retrying_a_known_event_id_returns_the_prior_entry_without_another_comment() -> Result<()> {
    let cli = CliHarness::new()?;
    let gh = FakeGh::new()?;
    initialize(&cli, &gh)?;
    let (projection, genesis) = create_item(&cli, &gh)?;
    let issue = serde_json::json!({
        "number": 41,
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
    let note = serde_json::json!({
            "id": 9002,
            "created_at": "2026-09-23T01:03:04Z",
            "user": {"login": "octocat"},
            "body": note_comment("note-fixed-1", "agent-b", "octocat", "deployment queued")
    });
    gh.respond(17, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(18, 0, &issue.to_string(), "")?;
    gh.respond(
        19,
        0,
        &serde_json::json!([[genesis_comment]]).to_string(),
        "",
    )?;
    gh.respond(20, 1, "", "gh: connection reset after upload\n")?;
    let args = [
        "--json",
        "note",
        "41",
        "deployment queued",
        "--actor",
        "agent-b",
        "--event-id",
        "note-fixed-1",
    ];
    ensure!(!cli.run_with_fake_gh(&gh, args)?.status.success());

    gh.respond(21, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(22, 0, &issue.to_string(), "")?;
    gh.respond(
        23,
        0,
        &serde_json::json!([[genesis_comment, note]]).to_string(),
        "",
    )?;
    gh.respond(24, 0, "{}", "")?;

    let retried = cli.run_with_fake_gh(&gh, args)?;
    assert_success(&retried)?;
    let entry = json(&retried)?;
    assert_eq!(entry["id"], 9002);
    assert_eq!(entry["event_id"], "note-fixed-1");
    let calls = gh.calls()?;
    assert_eq!(
        calls
            .matches("\tPOST\trepos/octocat/work-tracker-data/issues/41/comments")
            .count(),
        2,
        "the retry must not publish after the uncertain note POST"
    );
    Ok(())
}

#[test]
fn edited_history_raises_ledger_integrity_error_before_note_mutation_or_repair() -> Result<()> {
    let cli = CliHarness::new()?;
    let gh = FakeGh::new()?;
    initialize(&cli, &gh)?;
    let (projection, genesis) = create_item(&cli, &gh)?;
    let edited_genesis = genesis.replace("\"actor\":\"agent-a\"", "\"actor\":\"intruder\"");
    ensure!(
        edited_genesis != genesis,
        "fixture did not edit the genesis event"
    );

    gh.respond(17, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(
        18,
        0,
        &serde_json::json!({
            "number": 41,
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
            "body": edited_genesis
        }]])
        .to_string(),
        "",
    )?;

    let failed = cli.run_with_fake_gh(
        &gh,
        [
            "--json",
            "note",
            "41",
            "must not be published",
            "--actor",
            "agent-b",
            "--event-id",
            "note-after-corruption",
        ],
    )?;
    ensure!(!failed.status.success());
    assert_eq!(failed.stdout, b"");
    let diagnostic: Value = serde_json::from_slice(&failed.stderr)?;
    assert_eq!(diagnostic["error"]["code"], "github_ledger_integrity");
    ensure!(
        diagnostic["error"]["message"]
            .as_str()
            .context("missing integrity error message")?
            .contains("Ledger Integrity Error")
    );

    let calls = gh.calls()?;
    assert_eq!(
        calls
            .matches("\tPOST\trepos/octocat/work-tracker-data/issues/41/comments")
            .count(),
        1,
        "only the original genesis comment should exist"
    );
    assert_eq!(
        calls
            .matches("\tPATCH\trepos/octocat/work-tracker-data/issues/41")
            .count(),
        1,
        "the corrupted projection must not be repaired automatically"
    );
    Ok(())
}

#[test]
fn deleting_a_v5_projection_head_cannot_forge_legacy_bootstrap_permission() -> Result<()> {
    let cli = CliHarness::new()?;
    let gh = FakeGh::new()?;
    initialize(&cli, &gh)?;
    let (projection, genesis) = create_item(&cli, &gh)?;
    let headless_projection = without_projection_head(&projection)?;

    gh.respond(17, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(
        18,
        0,
        &serde_json::json!({
            "number": 41,
            "body": headless_projection,
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

    let failed = cli.run_with_fake_gh(
        &gh,
        [
            "--json",
            "note",
            "41",
            "must not be published",
            "--actor",
            "agent-b",
            "--event-id",
            "note-after-head-removal",
        ],
    )?;
    ensure!(!failed.status.success());
    let diagnostic: Value = serde_json::from_slice(&failed.stderr)?;
    assert_eq!(diagnostic["error"]["code"], "github_ledger_integrity");
    let calls = gh.calls()?;
    assert_eq!(
        calls
            .matches("\tPOST\trepos/octocat/work-tracker-data/issues/41/comments")
            .count(),
        1,
        "only the original genesis comment should exist"
    );
    assert_eq!(
        calls
            .matches("\tPATCH\trepos/octocat/work-tracker-data/issues/41")
            .count(),
        1,
        "removing the head must not trigger automatic repair"
    );
    Ok(())
}
