mod support;

use anyhow::{Context, Result, ensure};
use serde_json::Value;
use support::{CliHarness, FakeGh, assert_success};

const PRIVATE_REPOSITORY: &str = r#"{"full_name":"octocat/work-tracker-data","private":true,"has_issues":true,"permissions":{"admin":true,"push":true}}"#;

fn initialize(cli: &CliHarness, gh: &FakeGh) -> Result<()> {
    gh.respond(1, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(2, 1, "", "gh: Not Found (HTTP 404)\n")?;
    gh.respond(3, 0, PRIVATE_REPOSITORY, "")?;
    gh.respond(4, 0, "[[]]", "")?;
    assert_success(&cli.run_with_fake_gh(gh, ["init", "github"])?)
}

fn field_value<'a>(
    calls: &'a str,
    endpoint: &str,
    field: &str,
    next: &str,
    last: bool,
) -> Result<&'a str> {
    let call = (if last {
        calls.rfind(endpoint)
    } else {
        calls.find(endpoint)
    })
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

fn archive_comment() -> String {
    format!(
        "Work Tracker Mutation Proposal: archived by agent-b\n\nNote: superseded\n\n<!-- work-tracker:event\n{}\n-->",
        serde_json::json!({
            "schema_version": 1,
            "event_id": "{{LAST_EVENT_ID}}",
            "kind": "archived",
            "actor": "agent-b",
            "github_actor": "octocat",
            "note": "superseded",
            "changes": {"status": {"from": "pending", "to": "archived"}},
            "expected_state_revision": 1
        })
    )
}

struct ArchiveScenario {
    cli: CliHarness,
    gh: FakeGh,
    projection: String,
    genesis_body: String,
}

impl ArchiveScenario {
    fn new() -> Result<Self> {
        let cli = CliHarness::new()?;
        let gh = FakeGh::new()?;
        initialize(&cli, &gh)?;
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
            &gh,
            ["--json", "add", "Retain evidence", "--actor", "agent-a"],
        )?)?;
        let calls = gh.calls()?;
        let projection = field_value(
            &calls,
            "\tPATCH\trepos/octocat/work-tracker-data/issues/41",
            "body",
            "\t--field\tlabels[]=",
            false,
        )?
        .to_owned();
        let genesis_body = field_value(
            &calls,
            "\tPOST\trepos/octocat/work-tracker-data/issues/41/comments",
            "body",
            "\nCALL\tapi\t--method\tPATCH\t",
            false,
        )?
        .to_owned();
        Ok(Self {
            cli,
            gh,
            projection,
            genesis_body,
        })
    }

    fn run_archive(&self, lock_succeeds: bool) -> Result<(std::process::Output, Value, Value)> {
        let issue = serde_json::json!({
            "number":41,
            "title":"Retain evidence",
            "body":self.projection,
            "labels":[
                {"name":"work-tracker:item"},
                {"name":"work-tracker:status:pending"}
            ],
            "state":"open",
            "locked":false
        });
        let genesis = serde_json::json!({
            "id":9001,
            "created_at":"2026-09-23T01:02:04Z",
            "user":{"login":"octocat"},
            "body":self.genesis_body
        });
        let archived = serde_json::json!({
            "id":9002,
            "created_at":"2026-09-23T01:03:04Z",
            "user":{"login":"octocat"},
            "body":archive_comment()
        });
        self.gh.respond(17, 0, r#"{"login":"octocat"}"#, "")?;
        self.gh.respond(18, 0, &issue.to_string(), "")?;
        self.gh.respond(
            19,
            0,
            &serde_json::json!([[genesis.clone()]]).to_string(),
            "",
        )?;
        self.gh.respond(
            20,
            0,
            r#"{"id":9002,"created_at":"2026-09-23T01:03:04Z","user":{"login":"octocat"}}"#,
            "",
        )?;
        self.gh.respond(
            21,
            0,
            &serde_json::json!([[genesis.clone(), archived.clone()]]).to_string(),
            "",
        )?;
        self.gh.respond(22, 0, "{}", "")?;
        self.gh.respond(
            23,
            i32::from(!lock_succeeds),
            if lock_succeeds { "{}" } else { "" },
            if lock_succeeds {
                ""
            } else {
                "gh: service unavailable (HTTP 503)\n"
            },
        )?;
        let output = self.cli.run_with_fake_gh(
            &self.gh,
            [
                "--json",
                "archive",
                "41",
                "--note",
                "superseded",
                "--actor",
                "agent-b",
            ],
        )?;
        Ok((output, genesis, archived))
    }
}

#[test]
fn archive_records_one_event_closes_not_planned_and_locks() -> Result<()> {
    let scenario = ArchiveScenario::new()?;
    let cli = &scenario.cli;
    let gh = &scenario.gh;
    let (output, genesis, _) = scenario.run_archive(true)?;
    assert_success(&output)?;
    let item: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(item["status"], "archived");
    assert_eq!(item["archived_at"], "2026-09-23T01:03:04Z");
    assert_eq!(item["deleted_at"], item["archived_at"]);
    assert_eq!(item["purge_after"], Value::Null);

    let cache = cli.github_cache_path("octocat", "work-tracker-data");
    let database = cache.to_str().context("cache path was not UTF-8")?;
    let shown = cli.run(["--json", "--database", database, "show", "41"])?;
    assert_success(&shown)?;
    assert_eq!(
        serde_json::from_slice::<Value>(&shown.stdout)?["status"],
        "archived"
    );
    let history = cli.run(["--json", "--database", database, "history", "41"])?;
    assert_success(&history)?;
    let history: Value = serde_json::from_slice(&history.stdout)?;
    assert_eq!(history[1]["kind"], "archived");
    assert_eq!(history[1]["changes"]["status"]["to"], "archived");
    let filtered = cli.run([
        "--json",
        "--database",
        database,
        "list",
        "--status",
        "archived",
    ])?;
    assert_success(&filtered)?;
    assert_eq!(
        serde_json::from_slice::<Value>(&filtered.stdout)?[0]["id"],
        41
    );

    let calls = gh.calls()?;
    assert_eq!(
        calls
            .matches("\tPOST\trepos/octocat/work-tracker-data/issues/41/comments")
            .count(),
        2
    );
    ensure!(calls.contains("\"kind\":\"archived\""));
    let projection_call = calls
        .rfind("\tPATCH\trepos/octocat/work-tracker-data/issues/41")
        .map(|index| &calls[index..])
        .context("missing archive projection")?;
    ensure!(projection_call.contains("\t--field\tlabels[]=work-tracker:status:archived"));
    ensure!(projection_call.contains("\t--field\tstate=closed"));
    ensure!(projection_call.contains("\t--field\tstate_reason=not_planned"));
    ensure!(
        projection_call
            .contains("\nCALL\tapi\t--method\tPUT\trepos/octocat/work-tracker-data/issues/41/lock")
    );

    let archive_projection = field_value(
        &calls,
        "\tPATCH\trepos/octocat/work-tracker-data/issues/41",
        "body",
        "\nCALL\tapi\t--method\tPUT",
        true,
    )?
    .to_owned();
    let event_marker = "\"event_id\":\"archive-";
    let event_start = calls
        .rfind(event_marker)
        .map(|index| index + "\"event_id\":\"".len())
        .context("missing archive event ID")?;
    let event_end = calls[event_start..]
        .find('"')
        .map(|offset| event_start + offset)
        .context("unterminated archive event ID")?;
    let archive_event =
        archive_comment().replace("{{LAST_EVENT_ID}}", &calls[event_start..event_end]);
    let archived_issue = serde_json::json!({
        "number":41,
        "title":"Retain evidence",
        "body":archive_projection,
        "labels":[
            {"name":"work-tracker:item"},
            {"name":"work-tracker:status:archived"}
        ],
        "state":"closed",
        "state_reason":"not_planned",
        "locked":true,
        "updated_at":"2026-09-23T01:03:04Z"
    });
    let archived_comments = serde_json::json!([[genesis, {
        "id":9002,
        "created_at":"2026-09-23T01:03:04Z",
        "user":{"login":"octocat"},
        "body":archive_event
    }]]);
    gh.respond(
        24,
        0,
        &serde_json::json!([[archived_issue.clone()]]).to_string(),
        "",
    )?;
    gh.respond(25, 0, &archived_comments.to_string(), "")?;
    let synchronized_show = cli.run_with_fake_gh(gh, ["--json", "show", "41"])?;
    assert_success(&synchronized_show)?;
    assert_eq!(
        serde_json::from_slice::<Value>(&synchronized_show.stdout)?["status"],
        "archived"
    );

    for (start, args) in [
        (26, vec!["archive", "41", "--actor", "agent-c"]),
        (29, vec!["update", "41", "--title", "Rewritten"]),
        (32, vec!["status", "41", "active"]),
    ] {
        gh.respond(start, 0, r#"{"login":"octocat"}"#, "")?;
        gh.respond(start + 1, 0, &archived_issue.to_string(), "")?;
        gh.respond(start + 2, 0, &archived_comments.to_string(), "")?;
        let rejected = cli.run_with_fake_gh(gh, args)?;
        ensure!(!rejected.status.success());
        ensure!(
            std::str::from_utf8(&rejected.stderr)?.contains("is archived and cannot be modified")
        );
    }
    gh.respond(35, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(36, 0, &archived_issue.to_string(), "")?;
    gh.respond(37, 0, &archived_comments.to_string(), "")?;
    let rejected_note = cli.run_with_fake_gh(gh, ["note", "41", "late context"])?;
    ensure!(!rejected_note.status.success());
    ensure!(
        std::str::from_utf8(&rejected_note.stderr)?.contains("is archived and cannot be modified")
    );
    assert_eq!(
        gh.calls()?
            .matches("\tPOST\trepos/octocat/work-tracker-data/issues/41/comments")
            .count(),
        2,
        "later commands must not append history"
    );
    Ok(())
}

fn archive_lock_failure_is_recovered_by_sync(lock_applied: bool) -> Result<()> {
    let scenario = ArchiveScenario::new()?;
    let cli = &scenario.cli;
    let gh = &scenario.gh;
    let (interrupted, genesis, archived) = scenario.run_archive(false)?;
    ensure!(!interrupted.status.success());
    let error: Value = serde_json::from_slice(&interrupted.stderr)?;
    assert_eq!(error["error"]["code"], "github_projection_pending");
    assert_eq!(error["error"]["accepted"], true);
    assert_eq!(error["error"]["current_values"]["status"], "archived");

    let calls = gh.calls()?;
    let archived_projection = field_value(
        &calls,
        "\tPATCH\trepos/octocat/work-tracker-data/issues/41",
        "body",
        "\nCALL\tapi\t--method\tPUT",
        true,
    )?
    .to_owned();
    let archived_issue = serde_json::json!({
        "number":41,
        "title":"Retain evidence",
        "body":archived_projection,
        "labels":[
            {"name":"work-tracker:item"},
            {"name":"work-tracker:status:archived"}
        ],
        "state":"closed",
        "state_reason":"not_planned",
        "locked":lock_applied,
        "updated_at":"2026-09-23T01:03:04Z"
    });
    let archived_comments = serde_json::json!([[genesis.clone(), archived.clone()]]);
    gh.respond(
        24,
        0,
        &serde_json::json!([[archived_issue.clone()]]).to_string(),
        "",
    )?;
    gh.respond(25, 0, &archived_comments.to_string(), "")?;
    let repeated_start = if lock_applied {
        26
    } else {
        gh.respond(26, 0, "{}", "")?;
        gh.respond(27, 0, "{}", "")?;
        28
    };
    let recovered = cli.run_with_fake_gh(gh, ["--json", "show", "41"])?;
    assert_success(&recovered)?;
    assert_eq!(
        serde_json::from_slice::<Value>(&recovered.stdout)?["status"],
        "archived"
    );

    let mut drifted_issue = archived_issue;
    drifted_issue["locked"] = Value::Bool(false);
    gh.respond(repeated_start, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(repeated_start + 1, 0, &drifted_issue.to_string(), "")?;
    gh.respond(
        repeated_start + 2,
        0,
        &serde_json::json!([[genesis, archived]]).to_string(),
        "",
    )?;
    let lock_calls_before = gh
        .calls()?
        .matches("\tPUT\trepos/octocat/work-tracker-data/issues/41/lock")
        .count();
    let repeated = cli.run_with_fake_gh(
        gh,
        [
            "--json",
            "delete",
            "41",
            "--note",
            "superseded",
            "--actor",
            "agent-b",
        ],
    )?;
    ensure!(!repeated.status.success());
    ensure!(std::str::from_utf8(&repeated.stderr)?.contains("is archived and cannot be modified"));
    let calls = gh.calls()?;
    assert_eq!(
        calls
            .matches("\tPOST\trepos/octocat/work-tracker-data/issues/41/comments")
            .count(),
        2,
        "recovery must not publish another archive event"
    );
    assert_eq!(
        calls
            .matches("\tPUT\trepos/octocat/work-tracker-data/issues/41/lock")
            .count(),
        lock_calls_before,
        "a repeated archive must not repair projection drift"
    );
    Ok(())
}

#[test]
fn sync_recovers_when_archive_lock_was_not_applied() -> Result<()> {
    archive_lock_failure_is_recovered_by_sync(false)
}

#[test]
fn sync_recovers_when_archive_lock_response_was_lost() -> Result<()> {
    archive_lock_failure_is_recovered_by_sync(true)
}
