mod support;

use anyhow::{Context, Result, ensure};
use serde_json::Value;
use support::{CliHarness, FakeGh, assert_success, stderr, stdout};

const PRIVATE_REPOSITORY: &str = r#"{"full_name":"octocat/work-tracker-data","private":true,"has_issues":true,"permissions":{"admin":true,"push":true}}"#;

fn json(output: &std::process::Output) -> Result<Value> {
    serde_json::from_slice(&output.stdout).context("stdout was not valid JSON")
}

fn json_from_stderr(output: &std::process::Output) -> Result<Value> {
    serde_json::from_slice(&output.stderr).context("stderr was not valid JSON")
}

fn initialize(cli: &CliHarness, gh: &FakeGh) -> Result<()> {
    gh.respond(1, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(2, 1, "", "gh: Not Found (HTTP 404)\n")?;
    gh.respond(3, 0, PRIVATE_REPOSITORY, "")?;
    gh.respond(4, 0, "[[]]", "")?;
    assert_success(&cli.run_with_fake_gh(gh, ["init", "github"])?)
}

fn initialize_existing(cli: &CliHarness, gh: &FakeGh) -> Result<()> {
    gh.respond(1, 0, PRIVATE_REPOSITORY, "")?;
    gh.respond(2, 0, "[[]]", "")?;
    gh.respond(3, 0, "[[]]", "")?;
    gh.respond(4, 0, "[[]]", "")?;
    assert_success(&cli.run_with_fake_gh(gh, ["init", "github", "octocat/work-tracker-data"])?)
}

fn field_value<'a>(calls: &'a str, endpoint: &str, field: &str, next: &str) -> Result<&'a str> {
    let call = calls
        .find(endpoint)
        .with_context(|| format!("missing call to {endpoint}"))?;
    let field_marker = format!("\t--field\t{field}=");
    let value_start = calls[call..]
        .find(&field_marker)
        .map(|offset| call + offset + field_marker.len())
        .with_context(|| format!("missing {field} field for {endpoint}"))?;
    let value_end = calls[value_start..]
        .find(next)
        .map(|offset| value_start + offset)
        .with_context(|| format!("missing terminator for {field} field"))?;
    Ok(&calls[value_start..value_end])
}

#[test]
fn add_creates_a_github_work_item_and_show_reads_the_synchronized_cache() -> Result<()> {
    let cli = CliHarness::new()?;
    let gh = FakeGh::new()?;
    initialize(&cli, &gh)?;
    gh.respond(13, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(
        14,
        0,
        r#"{"number":41,"created_at":"2026-09-23T01:02:03Z","updated_at":"2026-09-23T01:02:03Z"}"#,
        "",
    )?;
    gh.respond(
        15,
        0,
        r#"{"id":9001,"created_at":"2026-09-23T01:02:04Z","user":{"login":"octocat"}}"#,
        "",
    )?;
    gh.respond(16, 0, "{}", "")?;

    let added = cli.run_with_fake_gh(
        &gh,
        [
            "--json",
            "add",
            "Watch company CI",
            "--description",
            "Wait for all required checks",
            "--status",
            "waiting",
            "--actor",
            "agent-a",
            "--note",
            "queued by release manager",
        ],
    )?;
    assert_success(&added)?;
    assert_eq!(stderr(&added)?, "");
    let item = json(&added)?;
    assert_eq!(item["id"], 41);
    assert_eq!(item["title"], "Watch company CI");
    assert_eq!(item["description"], "Wait for all required checks");
    assert_eq!(item["status"], "waiting");
    assert_eq!(item["created_at"], "2026-09-23T01:02:04Z");
    assert_eq!(item["updated_at"], "2026-09-23T01:02:04Z");

    let calls = gh.calls()?;
    let issue_start = calls
        .find("\tPOST\trepos/octocat/work-tracker-data/issues")
        .context("missing issue creation call")?;
    let issue_end = calls[issue_start..]
        .find("CALL\tapi\t--method\tPOST\trepos/octocat/work-tracker-data/issues/41/comments")
        .map(|offset| issue_start + offset)
        .context("missing genesis comment call")?;
    let issue_call = &calls[issue_start..issue_end];
    ensure!(issue_call.contains("title=Watch company CI"));
    ensure!(issue_call.contains("Wait for all required checks"));
    ensure!(issue_call.contains("work-tracker:item"));
    assert_eq!(
        issue_call.matches("work-tracker:status:").count(),
        1,
        "issue must carry exactly one initial Status label: {issue_call}"
    );
    ensure!(issue_call.contains("pending_genesis_event_id"));

    ensure!(calls.contains("\tPOST\trepos/octocat/work-tracker-data/issues/41/comments"));
    ensure!(calls.contains("work-tracker:event"));
    ensure!(calls.contains("\"schema_version\":1"));
    ensure!(calls.contains("\"kind\":\"created\""));
    ensure!(calls.contains("\"actor\":\"agent-a\""));
    ensure!(calls.contains("\"github_actor\":\"octocat\""));
    ensure!(calls.contains("queued by release manager"));
    ensure!(calls.contains("\tPATCH\trepos/octocat/work-tracker-data/issues/41"));
    ensure!(calls.contains("\"pending_genesis_event_id\":null"));
    ensure!(calls.contains("\"genesis_comment_id\":9001"));
    ensure!(calls.contains("\"state_revision\":1"));

    let shown_human = cli.run(["show", "41"])?;
    assert_success(&shown_human)?;
    ensure!(stdout(&shown_human)?.contains("ID:          41"));
    ensure!(stdout(&shown_human)?.contains("Status:      waiting"));
    ensure!(stdout(&shown_human)?.contains("Title:       Watch company CI"));

    let shown_json = cli.run(["--json", "show", "41"])?;
    assert_success(&shown_json)?;
    assert_eq!(json(&shown_json)?, item);

    Ok(())
}

#[test]
fn human_add_renders_the_github_id_and_projects_a_finished_status_as_closed() -> Result<()> {
    let cli = CliHarness::new()?;
    let gh = FakeGh::new()?;
    initialize(&cli, &gh)?;
    gh.respond(13, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(14, 0, r#"{"number":42}"#, "")?;
    gh.respond(
        15,
        0,
        r#"{"id":9002,"created_at":"2026-09-23T01:03:04Z","user":{"login":"octocat"}}"#,
        "",
    )?;
    gh.respond(16, 0, "{}", "")?;

    let added = cli.run_with_fake_gh(
        &gh,
        [
            "add",
            "Already complete",
            "--status",
            "done",
            "--actor",
            "agent-a",
        ],
    )?;
    assert_success(&added)?;
    ensure!(stdout(&added)?.contains("ID:          42"));
    ensure!(stdout(&added)?.contains("Status:      done"));
    ensure!(stdout(&added)?.contains("Title:       Already complete"));
    let calls = gh.calls()?;
    let patch_start = calls
        .find("\tPATCH\trepos/octocat/work-tracker-data/issues/42")
        .context("missing completed projection call")?;
    let patch = &calls[patch_start..];
    ensure!(patch.contains("\t--field\tstate=closed"));
    ensure!(patch.contains("\t--field\tstate_reason=completed"));
    Ok(())
}

#[test]
fn github_backend_still_rejects_mutations_outside_the_completed_tracer_slices() -> Result<()> {
    let cli = CliHarness::new()?;
    let gh = FakeGh::new()?;
    initialize(&cli, &gh)?;

    let unsupported = cli.run(["--json", "update", "41", "--title", "Changed"])?;
    ensure!(!unsupported.status.success());
    assert_eq!(stdout(&unsupported)?, "");
    let diagnostic = json_from_stderr(&unsupported)?;
    ensure!(
        diagnostic["error"]
            .as_str()
            .context("missing unsupported-command diagnostic")?
            .contains("does not support this mutation yet")
    );
    Ok(())
}

#[test]
fn existing_repository_starts_with_a_recovery_capable_cache() -> Result<()> {
    let cli = CliHarness::new()?;
    let gh = FakeGh::new()?;
    initialize_existing(&cli, &gh)?;
    gh.respond(13, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(14, 0, "[[]]", "")?;
    gh.respond(15, 0, r#"{"number":41}"#, "")?;
    gh.respond(
        16,
        0,
        r#"{"id":9001,"created_at":"2026-09-23T01:02:04Z","user":{"login":"octocat"}}"#,
        "",
    )?;
    gh.respond(17, 0, "{}", "")?;

    let added = cli.run_with_fake_gh(
        &gh,
        ["--json", "add", "Existing repository", "--actor", "agent-a"],
    )?;
    assert_success(&added)?;
    assert_eq!(json(&added)?["id"], 41);
    ensure!(gh.calls()?.contains(
        "\tGET\trepos/octocat/work-tracker-data/issues?state=all&per_page=100\t--paginate\t--slurp"
    ));
    Ok(())
}

#[test]
fn retry_after_genesis_publication_failure_completes_the_existing_issue() -> Result<()> {
    let cli = CliHarness::new()?;
    let gh = FakeGh::new()?;
    initialize(&cli, &gh)?;
    gh.respond(13, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(
        14,
        0,
        r#"{"number":41,"created_at":"2026-09-23T01:02:03Z"}"#,
        "",
    )?;
    gh.respond(15, 1, "", "gh: temporary service failure (HTTP 503)\n")?;

    let args = [
        "--json",
        "add",
        "Resume creation",
        "--description",
        "Do not duplicate this issue",
        "--actor",
        "agent-a",
    ];
    let interrupted = cli.run_with_fake_gh(&gh, args)?;
    ensure!(!interrupted.status.success());
    let first_calls = gh.calls()?;
    let pending_body = field_value(
        &first_calls,
        "\tPOST\trepos/octocat/work-tracker-data/issues",
        "body",
        "\t--field\tlabels[]=",
    )?;
    ensure!(pending_body.contains("pending_genesis_event_id"));

    gh.respond(16, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(
        17,
        0,
        &serde_json::json!({
            "number": 41,
            "body": pending_body,
            "labels": [{"name": "work-tracker:item"}, {"name": "work-tracker:status:pending"}]
        })
        .to_string(),
        "",
    )?;
    gh.respond(18, 0, "[[]]", "")?;
    gh.respond(
        19,
        0,
        r#"{"id":9001,"created_at":"2026-09-23T01:02:04Z","user":{"login":"octocat"}}"#,
        "",
    )?;
    gh.respond(20, 0, "{}", "")?;

    let resumed = cli.run_with_fake_gh(&gh, args)?;
    assert_success(&resumed)?;
    assert_eq!(json(&resumed)?["id"], 41);
    let all_calls = gh.calls()?;
    assert_eq!(
        all_calls
            .matches("\tPOST\trepos/octocat/work-tracker-data/issues\t")
            .count(),
        1,
        "retry created a duplicate issue:\n{all_calls}"
    );
    assert_eq!(
        all_calls
            .matches("\tPOST\trepos/octocat/work-tracker-data/issues/41/comments")
            .count(),
        2,
        "one failed and one successful genesis publication were expected"
    );
    Ok(())
}

#[test]
fn retry_after_projection_failure_reuses_the_published_genesis_event() -> Result<()> {
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
    gh.respond(16, 1, "", "gh: projection update failed (HTTP 503)\n")?;

    let args = ["--json", "add", "Finish projection", "--actor", "agent-a"];
    let interrupted = cli.run_with_fake_gh(&gh, args)?;
    ensure!(!interrupted.status.success());
    let first_calls = gh.calls()?;
    let completed_body = field_value(
        &first_calls,
        "\tPATCH\trepos/octocat/work-tracker-data/issues/41",
        "body",
        "\t--field\tlabels[]=",
    )?;
    let event_body = field_value(
        &first_calls,
        "\tPOST\trepos/octocat/work-tracker-data/issues/41/comments",
        "body",
        "\nCALL\tapi\t--method\tPATCH\t",
    )?;

    gh.respond(17, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(
        18,
        0,
        &serde_json::json!({
            "number": 41,
            "body": completed_body,
            "labels": [{"name": "work-tracker:item"}, {"name": "work-tracker:status:pending"}]
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
            "body": event_body
        }]])
        .to_string(),
        "",
    )?;
    gh.respond(20, 0, "{}", "")?;

    let resumed = cli.run_with_fake_gh(&gh, args)?;
    assert_success(&resumed)?;
    assert_eq!(json(&resumed)?["id"], 41);
    let all_calls = gh.calls()?;
    assert_eq!(
        all_calls
            .matches("\tPOST\trepos/octocat/work-tracker-data/issues\t")
            .count(),
        1
    );
    assert_eq!(
        all_calls
            .matches("\tPOST\trepos/octocat/work-tracker-data/issues/41/comments")
            .count(),
        1,
        "retry duplicated the accepted genesis event"
    );
    assert_eq!(
        all_calls
            .matches("\tPATCH\trepos/octocat/work-tracker-data/issues/41")
            .count(),
        2
    );
    Ok(())
}

#[test]
fn retry_rejects_a_matching_event_id_with_tampered_genesis_values() -> Result<()> {
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
    gh.respond(16, 1, "", "gh: projection update failed (HTTP 503)\n")?;

    let args = ["--json", "add", "Reject tampering", "--actor", "agent-a"];
    ensure!(!cli.run_with_fake_gh(&gh, args)?.status.success());
    let first_calls = gh.calls()?;
    let pending_body = field_value(
        &first_calls,
        "\tPOST\trepos/octocat/work-tracker-data/issues",
        "body",
        "\t--field\tlabels[]=",
    )?;
    let event_body = field_value(
        &first_calls,
        "\tPOST\trepos/octocat/work-tracker-data/issues/41/comments",
        "body",
        "\nCALL\tapi\t--method\tPATCH\t",
    )?
    .replace("\"actor\":\"agent-a\"", "\"actor\":\"intruder\"");

    gh.respond(17, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(
        18,
        0,
        &serde_json::json!({
            "number": 41,
            "body": pending_body,
            "labels": [{"name": "work-tracker:item"}, {"name": "work-tracker:status:pending"}]
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
            "body": event_body
        }]])
        .to_string(),
        "",
    )?;

    let collision = cli.run_with_fake_gh(&gh, args)?;
    ensure!(!collision.status.success());
    let diagnostic: Value = serde_json::from_slice(&collision.stderr)?;
    assert_eq!(diagnostic["error"]["code"], "github_metadata_collision");
    ensure!(
        diagnostic["error"]["message"]
            .as_str()
            .context("missing collision message")?
            .contains("genesis")
    );
    Ok(())
}

#[test]
fn retry_after_lost_issue_response_ignores_foreign_issues_and_finds_the_pending_marker()
-> Result<()> {
    let cli = CliHarness::new()?;
    let gh = FakeGh::new()?;
    initialize(&cli, &gh)?;
    gh.respond(13, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(14, 1, "", "gh: connection closed before response\n")?;

    let args = [
        "--json",
        "add",
        "Recover lost response",
        "--description",
        "The issue exists remotely",
        "--actor",
        "agent-a",
    ];
    let interrupted = cli.run_with_fake_gh(&gh, args)?;
    ensure!(!interrupted.status.success());
    let first_calls = gh.calls()?;
    let pending_body = field_value(
        &first_calls,
        "\tPOST\trepos/octocat/work-tracker-data/issues",
        "body",
        "\t--field\tlabels[]=",
    )?;

    gh.respond(15, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(
        16,
        0,
        &serde_json::json!([[
            {
                "number": 7,
                "body": "A normal GitHub issue",
                "labels": [{"name": "question"}]
            },
            {
                "number": 41,
                "body": pending_body,
                "labels": [
                    {"name": "work-tracker:item"},
                    {"name": "work-tracker:status:pending"}
                ]
            }
        ]])
        .to_string(),
        "",
    )?;
    gh.respond(17, 0, "[[]]", "")?;
    gh.respond(
        18,
        0,
        r#"{"id":9001,"created_at":"2026-09-23T01:02:04Z","user":{"login":"octocat"}}"#,
        "",
    )?;
    gh.respond(19, 0, "{}", "")?;

    let resumed = cli.run_with_fake_gh(&gh, args)?;
    assert_success(&resumed)?;
    assert_eq!(json(&resumed)?["id"], 41);
    let all_calls = gh.calls()?;
    assert_eq!(
        all_calls
            .matches("\tPOST\trepos/octocat/work-tracker-data/issues\t")
            .count(),
        1,
        "the retry must not create a second issue"
    );
    ensure!(all_calls.contains(
        "\tGET\trepos/octocat/work-tracker-data/issues?state=all&per_page=100\t--paginate\t--slurp"
    ));
    Ok(())
}

#[test]
fn pending_creation_is_recoverable_after_the_disposable_cache_is_lost() -> Result<()> {
    let cli = CliHarness::new()?;
    let gh = FakeGh::new()?;
    initialize(&cli, &gh)?;
    gh.respond(13, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(14, 1, "", "gh: connection closed before response\n")?;

    let args = [
        "--json",
        "add",
        "Recover without cache",
        "--actor",
        "agent-a",
    ];
    ensure!(!cli.run_with_fake_gh(&gh, args)?.status.success());
    let first_calls = gh.calls()?;
    let pending_body = field_value(
        &first_calls,
        "\tPOST\trepos/octocat/work-tracker-data/issues",
        "body",
        "\t--field\tlabels[]=",
    )?;
    ensure!(pending_body.contains("creation_fingerprint"));

    let cache = cli.github_cache_path("octocat", "work-tracker-data");
    let wal = cache.with_extension("db-wal");
    let shm = cache.with_extension("db-shm");
    for path in [&cache, &wal, &shm] {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    let empty_show = cli.run(["--json", "show", "41"])?;
    ensure!(!empty_show.status.success());

    gh.respond(15, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(
        16,
        0,
        &serde_json::json!([[{
            "number": 41,
            "body": pending_body,
            "labels": [
                {"name": "work-tracker:item"},
                {"name": "work-tracker:status:pending"}
            ]
        }]])
        .to_string(),
        "",
    )?;
    gh.respond(17, 0, "[[]]", "")?;
    gh.respond(
        18,
        0,
        r#"{"id":9001,"created_at":"2026-09-23T01:02:04Z","user":{"login":"octocat"}}"#,
        "",
    )?;
    gh.respond(19, 0, "{}", "")?;

    let resumed = cli.run_with_fake_gh(&gh, args)?;
    assert_success(&resumed)?;
    assert_eq!(json(&resumed)?["id"], 41);
    assert_eq!(
        gh.calls()?
            .matches("\tPOST\trepos/octocat/work-tracker-data/issues\t")
            .count(),
        1
    );
    Ok(())
}

#[test]
fn retry_creates_the_issue_when_the_failed_request_created_nothing() -> Result<()> {
    let cli = CliHarness::new()?;
    let gh = FakeGh::new()?;
    initialize(&cli, &gh)?;
    gh.respond(13, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(14, 1, "", "gh: service unavailable before creation\n")?;

    let args = ["--json", "add", "Retry real failure", "--actor", "agent-a"];
    ensure!(!cli.run_with_fake_gh(&gh, args)?.status.success());

    gh.respond(15, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(16, 0, "[[]]", "")?;
    gh.respond(17, 0, r#"{"number":41}"#, "")?;
    gh.respond(18, 0, "[[]]", "")?;
    gh.respond(
        19,
        0,
        r#"{"id":9001,"created_at":"2026-09-23T01:02:04Z","user":{"login":"octocat"}}"#,
        "",
    )?;
    gh.respond(20, 0, "{}", "")?;

    let resumed = cli.run_with_fake_gh(&gh, args)?;
    assert_success(&resumed)?;
    assert_eq!(json(&resumed)?["id"], 41);
    assert_eq!(
        gh.calls()?
            .matches("\tPOST\trepos/octocat/work-tracker-data/issues\t")
            .count(),
        2,
        "one failed attempt and one successful retry were expected"
    );
    Ok(())
}

#[test]
fn retry_rejects_a_work_tracker_label_without_valid_metadata() -> Result<()> {
    let cli = CliHarness::new()?;
    let gh = FakeGh::new()?;
    initialize(&cli, &gh)?;
    gh.respond(13, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(14, 1, "", "gh: connection closed before response\n")?;

    let args = ["--json", "add", "Collision", "--actor", "agent-a"];
    let interrupted = cli.run_with_fake_gh(&gh, args)?;
    ensure!(!interrupted.status.success());

    gh.respond(15, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(
        16,
        0,
        r#"[[{"number":41,"body":"unrelated body","labels":[{"name":"work-tracker:item"},{"name":"work-tracker:status:pending"}]}]]"#,
        "",
    )?;

    let collision = cli.run_with_fake_gh(&gh, args)?;
    ensure!(!collision.status.success());
    assert_eq!(stdout(&collision)?, "");
    let diagnostic: Value = serde_json::from_slice(&collision.stderr)?;
    assert_eq!(diagnostic["error"]["code"], "github_metadata_collision");
    ensure!(
        diagnostic["error"]["message"]
            .as_str()
            .context("missing collision message")?
            .contains("issue #41")
    );
    let calls = gh.calls()?;
    assert_eq!(
        calls
            .matches("\tPOST\trepos/octocat/work-tracker-data/issues\t")
            .count(),
        1
    );
    Ok(())
}

#[test]
fn retry_rejects_metadata_that_conflicts_with_the_cached_creation() -> Result<()> {
    let cli = CliHarness::new()?;
    let gh = FakeGh::new()?;
    initialize(&cli, &gh)?;
    gh.respond(13, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(14, 0, r#"{"number":41}"#, "")?;
    gh.respond(15, 1, "", "gh: temporary service failure (HTTP 503)\n")?;

    let args = [
        "--json",
        "add",
        "Fingerprint collision",
        "--actor",
        "agent-a",
    ];
    ensure!(!cli.run_with_fake_gh(&gh, args)?.status.success());
    let calls = gh.calls()?;
    let pending_body = field_value(
        &calls,
        "\tPOST\trepos/octocat/work-tracker-data/issues",
        "body",
        "\t--field\tlabels[]=",
    )?
    .replacen("creation-", "conflicting-", 1);

    gh.respond(16, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(
        17,
        0,
        &serde_json::json!({
            "number": 41,
            "body": pending_body,
            "labels": [{"name": "work-tracker:item"}, {"name": "work-tracker:status:pending"}]
        })
        .to_string(),
        "",
    )?;

    let collision = cli.run_with_fake_gh(&gh, args)?;
    ensure!(!collision.status.success());
    let diagnostic = json_from_stderr(&collision)?;
    assert_eq!(diagnostic["error"]["code"], "github_metadata_collision");
    Ok(())
}
