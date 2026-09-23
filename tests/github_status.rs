mod support;

use anyhow::{Context, Result, ensure};
use serde_json::Value;
use support::{CliHarness, FakeGh, assert_success};

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

fn field_value<'a>(
    calls: &'a str,
    endpoint: &str,
    field: &str,
    next: &str,
    last: bool,
) -> Result<&'a str> {
    let call = if last {
        calls.rfind(endpoint)
    } else {
        calls.find(endpoint)
    }
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
        false,
    )?
    .to_owned();
    let genesis = field_value(
        &calls,
        "\tPOST\trepos/octocat/work-tracker-data/issues/41/comments",
        "body",
        "\nCALL\tapi\t--method\tPATCH\t",
        false,
    )?
    .to_owned();
    Ok((projection, genesis))
}

struct StatusScenario {
    cli: CliHarness,
    gh: FakeGh,
    projection: String,
    genesis: String,
}

impl StatusScenario {
    fn new() -> Result<Self> {
        let cli = CliHarness::new()?;
        let gh = FakeGh::new()?;
        initialize(&cli, &gh)?;
        let (projection, genesis) = create_item(&cli, &gh)?;
        Ok(Self {
            cli,
            gh,
            projection,
            genesis,
        })
    }

    fn genesis_comment(&self) -> Value {
        serde_json::json!({
            "id":9001,
            "created_at":"2026-09-23T01:02:04Z",
            "user":{"login":"octocat"},
            "body":self.genesis
        })
    }

    fn issue(&self, extra_labels: &[&str]) -> Value {
        let mut labels = vec![
            serde_json::json!({"name":"work-tracker:item"}),
            serde_json::json!({"name":"work-tracker:status:pending"}),
        ];
        labels.extend(
            extra_labels
                .iter()
                .map(|label| serde_json::json!({"name":label})),
        );
        serde_json::json!({
            "number":41,
            "title":"Watch CI",
            "body":self.projection,
            "labels":labels
        })
    }

    fn script_initial_load(&self, extra_labels: &[&str]) -> Result<Value> {
        let genesis = self.genesis_comment();
        self.gh.respond(17, 0, r#"{"login":"octocat"}"#, "")?;
        self.gh
            .respond(18, 0, &self.issue(extra_labels).to_string(), "")?;
        self.gh
            .respond(19, 0, &serde_json::json!([[genesis]]).to_string(), "")?;
        Ok(genesis)
    }
}

fn status_comment(
    event_id: &str,
    from: &str,
    to: &str,
    actor: &str,
    github_actor: &str,
    expected_revision: u64,
    note: Option<&str>,
) -> String {
    let note_text = note
        .map(|note| format!("\n\nNote: {note}"))
        .unwrap_or_default();
    format!(
        "Work Tracker Mutation Proposal: status_changed by {actor}{note_text}\n\n<!-- work-tracker:event\n{}\n-->",
        serde_json::json!({
            "schema_version": 1,
            "event_id": event_id,
            "kind": "status_changed",
            "actor": actor,
            "github_actor": github_actor,
            "note": note,
            "changes": {"status": {"from": from, "to": to}},
            "expected_state_revision": expected_revision
        })
    )
}

fn last_status_event_id(calls: &str) -> Result<&str> {
    let marker = "\"event_id\":\"status-";
    let start = calls
        .rfind(marker)
        .map(|index| index + "\"event_id\":\"".len())
        .context("missing Status event ID")?;
    let end = calls[start..]
        .find('"')
        .map(|offset| start + offset)
        .context("unterminated Status event ID")?;
    Ok(&calls[start..end])
}

fn note_comment(event_id: &str, actor: &str, github_actor: &str, note: &str) -> String {
    format!(
        "Work Tracker History Entry: noted by {actor}\n\nNote: {note}\n\n<!-- work-tracker:event\n{}\n-->",
        serde_json::json!({
            "schema_version":1,
            "event_id":event_id,
            "kind":"noted",
            "actor":actor,
            "github_actor":github_actor,
            "note":note,
            "changes":{}
        })
    )
}

#[test]
fn actionable_status_transition_advances_revision_and_replaces_projection_label() -> Result<()> {
    let scenario = StatusScenario::new()?;
    let cli = &scenario.cli;
    let gh = &scenario.gh;
    let status = status_comment(
        "{{LAST_EVENT_ID}}",
        "pending",
        "active",
        "agent-b",
        "octocat",
        1,
        Some("starting"),
    );
    let genesis_comment = scenario.script_initial_load(&["team:release"])?;
    let status_comment = serde_json::json!({
        "id": 9002,
        "created_at": "2026-09-23T01:03:04Z",
        "user": {"login": "octocat"},
        "body": status
    });
    gh.respond(
        20,
        0,
        r#"{"id":9002,"created_at":"2026-09-23T01:03:04Z","user":{"login":"octocat"}}"#,
        "",
    )?;
    gh.respond(
        21,
        0,
        &serde_json::json!([[genesis_comment, status_comment]]).to_string(),
        "",
    )?;
    gh.respond(22, 0, "{}", "")?;

    let transitioned = cli.run_with_fake_gh(
        gh,
        [
            "--json", "status", "41", "active", "--note", "starting", "--actor", "agent-b",
        ],
    )?;
    assert_success(&transitioned)?;
    assert_eq!(json(&transitioned)?["status"], "active");

    let calls = gh.calls()?;
    ensure!(calls.contains("\"kind\":\"status_changed\""));
    ensure!(calls.contains("\"status\":{\"from\":\"pending\",\"to\":\"active\"}"));
    let projection_call = calls
        .rfind("\tPATCH\trepos/octocat/work-tracker-data/issues/41")
        .map(|index| &calls[index..])
        .context("missing projection update")?;
    assert_eq!(projection_call.matches("\t--field\tlabels[]=").count(), 3);
    ensure!(projection_call.contains("\t--field\tlabels[]=work-tracker:item"));
    ensure!(projection_call.contains("\t--field\tlabels[]=work-tracker:status:active"));
    ensure!(projection_call.contains("\t--field\tlabels[]=team:release"));
    ensure!(projection_call.contains("\t--field\tstate=open"));

    let cache = cli.github_cache_path("octocat", "work-tracker-data");
    let history = cli.run([
        "--json",
        "--database",
        cache.to_str().context("cache path was not UTF-8")?,
        "history",
        "41",
    ])?;
    assert_success(&history)?;
    let history = json(&history)?;
    assert_eq!(history[1]["kind"], "status_changed");
    assert_eq!(history[1]["state_revision"], 2);
    assert_eq!(
        history[1]["changes"],
        serde_json::json!({"status": {"from": "pending", "to": "active"}})
    );
    Ok(())
}

#[test]
fn done_closes_as_completed_and_can_reopen_to_an_actionable_status() -> Result<()> {
    let scenario = StatusScenario::new()?;
    let cli = &scenario.cli;
    let gh = &scenario.gh;
    let done = status_comment(
        "{{LAST_EVENT_ID}}",
        "pending",
        "done",
        "agent-b",
        "octocat",
        1,
        None,
    );
    let genesis_comment = scenario.script_initial_load(&[])?;
    gh.respond(
        20,
        0,
        r#"{"id":9002,"created_at":"2026-09-23T01:03:04Z","user":{"login":"octocat"}}"#,
        "",
    )?;
    gh.respond(
        21,
        0,
        &serde_json::json!([[
            genesis_comment,
            {"id":9002,"created_at":"2026-09-23T01:03:04Z","user":{"login":"octocat"},"body":done}
        ]])
        .to_string(),
        "",
    )?;
    gh.respond(22, 0, "{}", "")?;

    let completed =
        cli.run_with_fake_gh(gh, ["--json", "status", "41", "done", "--actor", "agent-b"])?;
    assert_success(&completed)?;
    assert_eq!(json(&completed)?["status"], "done");
    let calls = gh.calls()?;
    let done_event_id = last_status_event_id(&calls)?.to_owned();
    let done_projection = field_value(
        &calls,
        "\tPATCH\trepos/octocat/work-tracker-data/issues/41",
        "body",
        "\n-->\n",
        true,
    )?;
    let done_projection = format!("{done_projection}\n-->");
    let completed_call = calls
        .rfind("\tPATCH\trepos/octocat/work-tracker-data/issues/41")
        .map(|index| &calls[index..])
        .context("missing completed projection")?;
    ensure!(completed_call.contains("\t--field\tlabels[]=work-tracker:status:done"));
    ensure!(completed_call.contains("\t--field\tstate=closed"));
    ensure!(completed_call.contains("\t--field\tstate_reason=completed"));

    let done_body = status_comment(
        &done_event_id,
        "pending",
        "done",
        "agent-b",
        "octocat",
        1,
        None,
    );
    let active = status_comment(
        "{{LAST_EVENT_ID}}",
        "done",
        "active",
        "agent-c",
        "octocat",
        2,
        Some("more work"),
    );
    let prior_comments = serde_json::json!([
        genesis_comment,
        {"id":9002,"created_at":"2026-09-23T01:03:04Z","user":{"login":"octocat"},"body":done_body}
    ]);
    gh.respond(23, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(
        24,
        0,
        &serde_json::json!({
            "number":41,
            "title":"Watch CI",
            "body":done_projection,
            "labels":[{"name":"work-tracker:item"},{"name":"work-tracker:status:done"}]
        })
        .to_string(),
        "",
    )?;
    gh.respond(25, 0, &serde_json::json!([prior_comments]).to_string(), "")?;
    gh.respond(
        26,
        0,
        r#"{"id":9003,"created_at":"2026-09-23T01:04:04Z","user":{"login":"octocat"}}"#,
        "",
    )?;
    let mut confirmed = prior_comments
        .as_array()
        .context("comments were not an array")?
        .clone();
    confirmed.push(serde_json::json!({
        "id":9003,"created_at":"2026-09-23T01:04:04Z","user":{"login":"octocat"},"body":active
    }));
    gh.respond(27, 0, &serde_json::json!([confirmed]).to_string(), "")?;
    gh.respond(28, 0, "{}", "")?;

    let reopened = cli.run_with_fake_gh(
        gh,
        [
            "status",
            "41",
            "active",
            "--note",
            "more work",
            "--actor",
            "agent-c",
        ],
    )?;
    assert_success(&reopened)?;
    ensure!(std::str::from_utf8(&reopened.stdout)?.contains("Status:      active"));
    let calls = gh.calls()?;
    let reopened_call = calls
        .rfind("\tPATCH\trepos/octocat/work-tracker-data/issues/41")
        .map(|index| &calls[index..])
        .context("missing reopened projection")?;
    ensure!(reopened_call.contains("\t--field\tlabels[]=work-tracker:status:active"));
    ensure!(reopened_call.contains("\t--field\tstate=open"));
    ensure!(!reopened_call.contains("\t--field\tstate_reason="));
    Ok(())
}

#[test]
fn repeated_status_is_idempotent_and_publishes_no_proposal() -> Result<()> {
    let scenario = StatusScenario::new()?;
    let cli = &scenario.cli;
    let gh = &scenario.gh;
    scenario.script_initial_load(&[])?;

    let unchanged = cli.run_with_fake_gh(
        gh,
        ["--json", "status", "41", "pending", "--actor", "agent-b"],
    )?;
    assert_success(&unchanged)?;
    assert_eq!(json(&unchanged)?["status"], "pending");
    assert_eq!(
        gh.calls()?
            .matches("\tPOST\trepos/octocat/work-tracker-data/issues/41/comments")
            .count(),
        1,
        "only the genesis comment should exist"
    );
    Ok(())
}

#[test]
fn accepted_status_survives_projection_failure_and_sync_repairs_it() -> Result<()> {
    let scenario = StatusScenario::new()?;
    let cli = &scenario.cli;
    let gh = &scenario.gh;
    let status = status_comment(
        "{{LAST_EVENT_ID}}",
        "pending",
        "active",
        "agent-b",
        "octocat",
        1,
        None,
    );
    let issue = scenario.issue(&[]);
    let genesis_comment = scenario.script_initial_load(&[])?;
    let status_entry = serde_json::json!({
        "id":9002,"created_at":"2026-09-23T01:03:04Z","user":{"login":"octocat"},"body":status
    });
    gh.respond(
        20,
        0,
        r#"{"id":9002,"created_at":"2026-09-23T01:03:04Z","user":{"login":"octocat"}}"#,
        "",
    )?;
    gh.respond(
        21,
        0,
        &serde_json::json!([[genesis_comment, status_entry]]).to_string(),
        "",
    )?;
    gh.respond(22, 1, "", "gh: service unavailable (HTTP 503)\n")?;

    let failed = cli.run_with_fake_gh(
        gh,
        ["--json", "status", "41", "active", "--actor", "agent-b"],
    )?;
    ensure!(!failed.status.success());
    assert_eq!(failed.stdout, b"");
    let error: Value = serde_json::from_slice(&failed.stderr)?;
    assert_eq!(error["error"]["code"], "github_projection_pending");
    assert_eq!(error["error"]["accepted"], true);
    assert_eq!(error["error"]["effective"], true);
    assert_eq!(error["error"]["current_values"]["status"], "active");
    let event_id = error["error"]["event_id"]
        .as_str()
        .context("missing event ID")?;

    let status = status_comment(event_id, "pending", "active", "agent-b", "octocat", 1, None);
    let status_entry = serde_json::json!({
        "id":9002,"created_at":"2026-09-23T01:03:04Z","user":{"login":"octocat"},"body":status
    });
    gh.respond(
        23,
        0,
        &serde_json::json!([[{
            "number":41,
            "title":"Watch CI",
            "body":issue["body"],
            "labels":issue["labels"],
            "updated_at":"2026-09-23T01:03:04Z"
        }]])
        .to_string(),
        "",
    )?;
    gh.respond(
        24,
        0,
        &serde_json::json!([[genesis_comment, status_entry]]).to_string(),
        "",
    )?;
    gh.respond(25, 0, "{}", "")?;

    let recovered = cli.run_with_fake_gh(gh, ["--json", "history", "41"])?;
    assert_success(&recovered)?;
    let history = json(&recovered)?;
    assert_eq!(history[1]["event_id"], event_id);
    assert_eq!(history[1]["state_revision"], 2);
    let calls = gh.calls()?;
    assert_eq!(
        calls
            .matches("\tPOST\trepos/octocat/work-tracker-data/issues/41/comments")
            .count(),
        2,
        "recovery must not publish a duplicate Status proposal"
    );
    let repair = calls
        .rfind("\tPATCH\trepos/octocat/work-tracker-data/issues/41")
        .map(|index| &calls[index..])
        .context("missing projection repair")?;
    ensure!(repair.contains("\t--field\tlabels[]=work-tracker:status:active"));
    ensure!(repair.contains("\t--field\tstate=open"));
    Ok(())
}

#[test]
fn first_valid_status_wins_and_loser_gets_the_structured_conflict() -> Result<()> {
    let scenario = StatusScenario::new()?;
    let cli = &scenario.cli;
    let gh = &scenario.gh;
    let unsupported_archive = status_comment(
        "status-archive",
        "pending",
        "archived",
        "agent-x",
        "other-user",
        1,
        None,
    );
    let winner = status_comment(
        "status-winner",
        "pending",
        "waiting",
        "agent-a",
        "other-user",
        1,
        None,
    );
    let loser = status_comment(
        "{{LAST_EVENT_ID}}",
        "pending",
        "blocked",
        "agent-b",
        "octocat",
        1,
        Some("blocked here"),
    );
    let genesis_comment = scenario.script_initial_load(&[])?;
    gh.respond(
        20,
        0,
        r#"{"id":9004,"created_at":"2026-09-23T01:04:04Z","user":{"login":"octocat"}}"#,
        "",
    )?;
    gh.respond(21, 0, &serde_json::json!([[
        genesis_comment,
        {"id":9004,"created_at":"2026-09-23T01:04:04Z","user":{"login":"octocat"},"body":loser},
        {"id":9002,"created_at":"2026-09-23T01:02:30Z","user":{"login":"other-user"},"body":unsupported_archive},
        {"id":9003,"created_at":"2026-09-23T01:03:04Z","user":{"login":"other-user"},"body":winner}
    ]]).to_string(), "")?;
    gh.respond(22, 0, "{}", "")?;

    let rejected = cli.run_with_fake_gh(
        gh,
        [
            "--json",
            "status",
            "41",
            "blocked",
            "--note",
            "blocked here",
            "--actor",
            "agent-b",
        ],
    )?;
    ensure!(!rejected.status.success());
    assert_eq!(rejected.stdout, b"");
    let error: Value = serde_json::from_slice(&rejected.stderr)?;
    assert_eq!(error["error"]["code"], "github_rejected_mutation");
    assert_eq!(error["error"]["expected_state_revision"], 1);
    assert_eq!(error["error"]["current_state_revision"], 2);
    assert_eq!(error["error"]["current_values"]["status"], "waiting");

    let cache = cli.github_cache_path("octocat", "work-tracker-data");
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
        2
    );
    assert_eq!(history[1]["event_id"], "status-winner");
    assert_eq!(
        history[1]["changes"],
        serde_json::json!({"status":{"from":"pending","to":"waiting"}})
    );
    let rejected_mutations = cli.run([
        "--json",
        "--database",
        cache.to_str().context("cache path was not UTF-8")?,
        "rejected",
        "41",
    ])?;
    assert_success(&rejected_mutations)?;
    let rejected_mutations = json(&rejected_mutations)?;
    assert_eq!(
        rejected_mutations
            .as_array()
            .context("Rejected Mutations was not an array")?
            .len(),
        1
    );
    assert_eq!(
        rejected_mutations[0]["changes"],
        serde_json::json!({"status":{"from":"pending","to":"blocked"}})
    );
    Ok(())
}

#[test]
fn note_racing_status_is_retained_without_a_false_revision_conflict() -> Result<()> {
    let scenario = StatusScenario::new()?;
    let cli = &scenario.cli;
    let gh = &scenario.gh;
    let note = note_comment("note-racer", "agent-a", "other-user", "CI queued");
    let status = status_comment(
        "{{LAST_EVENT_ID}}",
        "pending",
        "waiting",
        "agent-b",
        "octocat",
        1,
        None,
    );
    let genesis_comment = scenario.script_initial_load(&[])?;
    gh.respond(
        20,
        0,
        r#"{"id":9003,"created_at":"2026-09-23T01:04:04Z","user":{"login":"octocat"}}"#,
        "",
    )?;
    gh.respond(21, 0, &serde_json::json!([[
        genesis_comment,
        {"id":9003,"created_at":"2026-09-23T01:04:04Z","user":{"login":"octocat"},"body":status},
        {"id":9002,"created_at":"2026-09-23T01:03:04Z","user":{"login":"other-user"},"body":note}
    ]]).to_string(), "")?;
    gh.respond(22, 0, "{}", "")?;

    let transitioned = cli.run_with_fake_gh(
        gh,
        ["--json", "status", "41", "waiting", "--actor", "agent-b"],
    )?;
    assert_success(&transitioned)?;
    assert_eq!(json(&transitioned)?["status"], "waiting");
    let cache = cli.github_cache_path("octocat", "work-tracker-data");
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
    assert_eq!(history[1]["event_id"], "note-racer");
    assert_eq!(history[1]["state_revision"], 1);
    assert_eq!(history[2]["kind"], "status_changed");
    assert_eq!(history[2]["state_revision"], 2);
    Ok(())
}

#[test]
fn cancelled_closes_as_not_planned_and_can_reopen() -> Result<()> {
    let scenario = StatusScenario::new()?;
    let cli = &scenario.cli;
    let gh = &scenario.gh;
    let cancelled = status_comment(
        "{{LAST_EVENT_ID}}",
        "pending",
        "cancelled",
        "agent-b",
        "octocat",
        1,
        None,
    );
    let genesis_comment = scenario.script_initial_load(&[])?;
    gh.respond(
        20,
        0,
        r#"{"id":9002,"created_at":"2026-09-23T01:03:04Z","user":{"login":"octocat"}}"#,
        "",
    )?;
    gh.respond(21, 0, &serde_json::json!([[
        genesis_comment,
        {"id":9002,"created_at":"2026-09-23T01:03:04Z","user":{"login":"octocat"},"body":cancelled}
    ]]).to_string(), "")?;
    gh.respond(22, 0, "{}", "")?;
    let result = cli.run_with_fake_gh(
        gh,
        ["--json", "status", "41", "cancelled", "--actor", "agent-b"],
    )?;
    assert_success(&result)?;
    assert_eq!(json(&result)?["status"], "cancelled");
    let calls = gh.calls()?;
    let projection_call = calls
        .rfind("\tPATCH\trepos/octocat/work-tracker-data/issues/41")
        .map(|index| &calls[index..])
        .context("missing cancelled projection")?;
    ensure!(projection_call.contains("\t--field\tlabels[]=work-tracker:status:cancelled"));
    ensure!(projection_call.contains("\t--field\tstate=closed"));
    ensure!(projection_call.contains("\t--field\tstate_reason=not_planned"));

    let cancelled_event_id = last_status_event_id(&calls)?.to_owned();
    let projected_body = field_value(
        &calls,
        "\tPATCH\trepos/octocat/work-tracker-data/issues/41",
        "body",
        "\n-->\n",
        true,
    )?;
    let projected_body = format!("{projected_body}\n-->");
    let cancelled_body = status_comment(
        &cancelled_event_id,
        "pending",
        "cancelled",
        "agent-b",
        "octocat",
        1,
        None,
    );
    let active = status_comment(
        "{{LAST_EVENT_ID}}",
        "cancelled",
        "active",
        "agent-c",
        "octocat",
        2,
        None,
    );
    let prior_comments = serde_json::json!([
        scenario.genesis_comment(),
        {"id":9002,"created_at":"2026-09-23T01:03:04Z","user":{"login":"octocat"},"body":cancelled_body}
    ]);
    gh.respond(23, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(
        24,
        0,
        &serde_json::json!({
            "number":41,
            "title":"Watch CI",
            "body":projected_body,
            "labels":[{"name":"work-tracker:item"},{"name":"work-tracker:status:cancelled"}],
            "state":"closed",
            "state_reason":"not_planned"
        })
        .to_string(),
        "",
    )?;
    gh.respond(25, 0, &serde_json::json!([prior_comments]).to_string(), "")?;
    gh.respond(
        26,
        0,
        r#"{"id":9003,"created_at":"2026-09-23T01:04:04Z","user":{"login":"octocat"}}"#,
        "",
    )?;
    let mut confirmed = prior_comments
        .as_array()
        .context("comments were not an array")?
        .clone();
    confirmed.push(serde_json::json!({
        "id":9003,"created_at":"2026-09-23T01:04:04Z","user":{"login":"octocat"},"body":active
    }));
    gh.respond(27, 0, &serde_json::json!([confirmed]).to_string(), "")?;
    gh.respond(28, 0, "{}", "")?;
    let reopened = cli.run_with_fake_gh(
        gh,
        ["--json", "status", "41", "active", "--actor", "agent-c"],
    )?;
    assert_success(&reopened)?;
    assert_eq!(json(&reopened)?["status"], "active");
    Ok(())
}
