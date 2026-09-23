mod support;

use std::fs;

use anyhow::{Context, Result, ensure};
use rusqlite::Connection;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
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

#[test]
fn exact_recovery_restores_verified_copy_revalidates_and_rebuilds_projection() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    let gh = FakeGh::new()?;
    let scenario = seed_edited_integrity(&cli, &gh)?;

    gh.respond(5, 0, &scenario.corrupt_issue.to_string(), "")?;
    gh.respond(
        6,
        0,
        &json!([[remote_comment(&scenario.edited_comment)]]).to_string(),
        "",
    )?;
    gh.respond(7, 0, "", "")?;
    gh.respond(8, 0, &scenario.healthy_issue.to_string(), "")?;
    gh.respond(
        9,
        0,
        &json!([[remote_comment(&scenario.original_comment)]]).to_string(),
        "",
    )?;
    gh.respond(10, 0, "", "")?;
    let recovered = cli.run_with_fake_gh(
        &gh,
        ["--json", "recover", "41", "--mode", "restore-exact-copy"],
    )?;
    assert_success(&recovered)?;
    let report: Value = serde_json::from_slice(&recovered.stdout)?;
    assert_eq!(report["outcome"], "exact_restoration");
    assert_eq!(report["work_item_id"], 41);
    assert_eq!(report["full_history_revalidated"], true);
    assert_eq!(report["projection_rebuilt"], true);

    let shown = cli.run_with_fake_gh(&gh, ["--offline", "--json", "show", "41"])?;
    assert_success(&shown)?;
    ensure!(serde_json::from_slice::<Value>(&shown.stdout)?["ledger_integrity_error"].is_null());
    let calls = gh.calls()?;
    ensure!(
        calls.contains("\tPATCH\trepos/octocat/work-tracker-data/issues/comments/9001\t--field")
    );
    ensure!(!calls.contains("\tPOST\trepos/octocat/work-tracker-data/issues/41/comments"));
    Ok(())
}

#[test]
fn exact_recovery_failure_stays_latched_and_reports_failed_validation() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    let gh = FakeGh::new()?;
    let scenario = seed_edited_integrity(&cli, &gh)?;

    gh.respond(5, 0, &scenario.corrupt_issue.to_string(), "")?;
    gh.respond(
        6,
        0,
        &json!([[remote_comment(&scenario.edited_comment)]]).to_string(),
        "",
    )?;
    gh.respond(7, 0, "", "")?;
    gh.respond(8, 0, &scenario.corrupt_issue.to_string(), "")?;
    gh.respond(
        9,
        0,
        &json!([[remote_comment(&scenario.edited_comment)]]).to_string(),
        "",
    )?;
    let recovered = cli.run_with_fake_gh(
        &gh,
        ["--json", "recover", "41", "--mode", "restore-exact-copy"],
    )?;
    ensure!(!recovered.status.success());
    let error: Value = serde_json::from_slice(&recovered.stderr)?;
    assert_eq!(error["error"]["code"], "github_recovery_validation_failed");
    assert_eq!(error["error"]["outcome"], "failed_validation");

    let shown = cli.run_with_fake_gh(&gh, ["--offline", "--json", "show", "41"])?;
    assert_success(&shown)?;
    assert_eq!(
        serde_json::from_slice::<Value>(&shown.stdout)?["ledger_integrity_error"],
        true
    );
    Ok(())
}

#[test]
fn exact_recovery_refuses_to_unlock_a_live_locked_work_item() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    let gh = FakeGh::new()?;
    let scenario = seed_edited_integrity(&cli, &gh)?;
    let mut locked_issue = scenario.corrupt_issue.clone();
    locked_issue["locked"] = json!(true);
    gh.respond(5, 0, &locked_issue.to_string(), "")?;
    gh.respond(
        6,
        0,
        &json!([[remote_comment(&scenario.edited_comment)]]).to_string(),
        "",
    )?;
    let recovered = cli.run_with_fake_gh(
        &gh,
        ["--json", "recover", "41", "--mode", "restore-exact-copy"],
    )?;
    ensure!(!recovered.status.success());
    let error: Value = serde_json::from_slice(&recovered.stderr)?;
    assert_eq!(error["error"]["code"], "github_recovery_still_blocked");
    let calls = gh.calls()?;
    ensure!(!calls.contains("\tPATCH\trepos/octocat/work-tracker-data/issues/comments/9001"));
    ensure!(!calls.contains("\tDELETE\trepos/octocat/work-tracker-data/issues/41/lock"));
    Ok(())
}

#[test]
fn doctor_never_offers_exact_recovery_for_unverified_cached_evidence() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    let gh = FakeGh::new()?;
    let original_event = event("Original title");
    let edited_event = event("Edited after untrusted caching");
    let original_comment = comment(&original_event)?;
    let edited_comment = comment(&edited_event)?;
    let invalid_projection = projection("not-the-event-hash");
    let corrupt_issue = issue(&invalid_projection, "2026-09-23T01:02:04Z", false);
    respond_sync(&gh, 1, &corrupt_issue, &remote_comment(&original_comment))?;
    let shown = cli.run_with_fake_gh(&gh, ["--json", "show", "41"])?;
    assert_success(&shown)?;
    assert_eq!(
        serde_json::from_slice::<Value>(&shown.stdout)?["ledger_integrity_error"],
        true
    );

    gh.respond(3, 0, &corrupt_issue.to_string(), "")?;
    gh.respond(
        4,
        0,
        &json!([[remote_comment(&edited_comment)]]).to_string(),
        "",
    )?;
    gh.respond(5, 0, "[[]]", "")?;
    let diagnosed = cli.run_with_fake_gh(&gh, ["--json", "doctor", "41"])?;
    assert_success(&diagnosed)?;
    let report: Value = serde_json::from_slice(&diagnosed.stdout)?;
    assert_eq!(report["eligible_repair_modes"], json!(["rebaseline"]));

    gh.respond(6, 0, &corrupt_issue.to_string(), "")?;
    gh.respond(
        7,
        0,
        &json!([[remote_comment(&edited_comment)]]).to_string(),
        "",
    )?;
    let recovered = cli.run_with_fake_gh(
        &gh,
        ["--json", "recover", "41", "--mode", "restore-exact-copy"],
    )?;
    ensure!(!recovered.status.success());
    let error: Value = serde_json::from_slice(&recovered.stderr)?;
    assert_eq!(error["error"]["code"], "github_recovery_still_blocked");
    assert_eq!(error["error"]["outcome"], "still_blocked");
    Ok(())
}

#[test]
fn explicit_rebaseline_retains_untrusted_evidence_and_starts_a_new_hash_root() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    let gh = FakeGh::new()?;
    let original_event = event("Original title");
    let original_comment = comment(&original_event)?;
    let invalid_projection = projection("disconnected-projection-head");
    let corrupt_issue = issue(&invalid_projection, "2026-09-23T01:02:04Z", false);
    respond_sync(&gh, 1, &corrupt_issue, &remote_comment(&original_comment))?;
    assert_success(&cli.run_with_fake_gh(&gh, ["--json", "show", "41"])?)?;

    let missing_actor = cli.run_with_fake_gh(
        &gh,
        [
            "--json",
            "recover",
            "41",
            "--mode",
            "rebaseline",
            "--reason",
            "reviewed projection",
        ],
    )?;
    ensure!(!missing_actor.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&missing_actor.stderr)?["error"]["outcome"],
        "still_blocked"
    );
    let missing_reason = cli.run_with_fake_gh(
        &gh,
        [
            "--json",
            "recover",
            "41",
            "--mode",
            "rebaseline",
            "--actor",
            "reviewer-a",
        ],
    )?;
    ensure!(!missing_reason.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&missing_reason.stderr)?["error"]["outcome"],
        "still_blocked"
    );
    let empty_reason = cli.run_with_fake_gh(
        &gh,
        [
            "--json",
            "recover",
            "41",
            "--mode",
            "rebaseline",
            "--actor",
            "reviewer-a",
            "--reason",
            "   ",
        ],
    )?;
    ensure!(!empty_reason.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&empty_reason.stderr)?["error"]["outcome"],
        "still_blocked"
    );

    respond_rebaseline(
        &gh,
        3,
        &corrupt_issue,
        &[remote_comment(&original_comment)],
        &json!([{
            "event": "commented",
            "comment_id": 9001,
            "actor": {"login": "octocat"}
        }]),
        "2026-09-23T04:02:03Z",
    )?;
    let recovered = cli.run_with_fake_gh(
        &gh,
        [
            "--json",
            "recover",
            "41",
            "--mode",
            "rebaseline",
            "--actor",
            "reviewer-a",
            "--reason",
            "reviewed projection",
        ],
    )?;
    assert_success(&recovered)?;
    let report: Value = serde_json::from_slice(&recovered.stdout)?;
    assert_eq!(report["outcome"], "rebaseline");
    assert_eq!(report["full_history_revalidated"], false);
    assert_eq!(report["projection_rebuilt"], true);
    assert_eq!(report["trusted_event_count"], 1);
    assert_eq!(report["untrusted_event_count"], 1);

    let connection = Connection::open(cli.github_cache_path("octocat", "work-tracker-data"))?;
    let mut statement = connection.prepare(
        "SELECT event_id, kind, actor, note, changes_json, previous_history_hash, evidence_trust
         FROM history_entries WHERE work_item_id = 41 ORDER BY id",
    )?;
    let entries = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, String>(6)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].0.as_deref(), Some("genesis-integrity-41"));
    assert_eq!(entries[0].6, "untrusted");
    assert_eq!(entries[1].1, "rebaseline");
    assert_eq!(entries[1].2, "reviewer-a");
    assert_eq!(entries[1].3.as_deref(), Some("reviewed projection"));
    assert!(entries[1].5.is_none());
    assert_eq!(entries[1].6, "trusted");
    let rebaseline_changes: Value = serde_json::from_str(&entries[1].4)?;
    assert_eq!(rebaseline_changes["title"], "Original title");
    assert_eq!(rebaseline_changes["status"], "active");
    assert_eq!(
        rebaseline_changes["integrity_context"]["integrity_health"],
        "ledger_integrity_error"
    );
    ensure!(
        rebaseline_changes["prior_evidence"][0]["body"]
            .as_str()
            .is_some_and(|body| body.contains("genesis-integrity-41"))
    );
    Ok(())
}

#[test]
fn doctor_diagnosis_can_be_rebaselined_without_a_prior_cached_latch() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    let gh = FakeGh::new()?;
    let original_event = event("Original title");
    let original_comment = comment(&original_event)?;
    let invalid_projection = projection("disconnected-projection-head");
    let corrupt_issue = issue(&invalid_projection, "2026-09-23T01:02:04Z", false);

    gh.respond(1, 0, &corrupt_issue.to_string(), "")?;
    gh.respond(
        2,
        0,
        &json!([[remote_comment(&original_comment)]]).to_string(),
        "",
    )?;
    gh.respond(3, 0, "[[]]", "")?;
    let diagnosed = cli.run_with_fake_gh(&gh, ["--json", "doctor", "41"])?;
    assert_success(&diagnosed)?;
    assert_eq!(
        serde_json::from_slice::<Value>(&diagnosed.stdout)?["eligible_repair_modes"],
        json!(["rebaseline"])
    );

    respond_rebaseline(
        &gh,
        4,
        &corrupt_issue,
        &[remote_comment(&original_comment)],
        &json!([]),
        "2026-09-23T04:02:03Z",
    )?;
    let recovered = cli.run_with_fake_gh(
        &gh,
        [
            "--json",
            "recover",
            "41",
            "--mode",
            "rebaseline",
            "--actor",
            "reviewer-a",
            "--reason",
            "doctor-reviewed projection",
        ],
    )?;
    assert_success(&recovered)?;
    assert_eq!(
        serde_json::from_slice::<Value>(&recovered.stdout)?["outcome"],
        "rebaseline"
    );
    Ok(())
}

#[test]
fn rebaseline_without_cached_evidence_retains_a_projected_markerless_comment() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    let gh = FakeGh::new()?;
    let original_event = event("Original title");
    let projection = projection(&hash(&original_event, 9001)?);
    let corrupt_issue = issue(&projection, "2026-09-23T01:02:04Z", false);
    let damaged_body = "damaged genesis body with no event marker";
    respond_rebaseline(
        &gh,
        1,
        &corrupt_issue,
        &[remote_comment(damaged_body)],
        &json!([]),
        "2026-09-23T04:02:03Z",
    )?;

    let recovered = cli.run_with_fake_gh(
        &gh,
        [
            "--json",
            "recover",
            "41",
            "--mode",
            "rebaseline",
            "--actor",
            "reviewer-a",
            "--reason",
            "reviewed projected markerless evidence",
        ],
    )?;
    assert_success(&recovered)?;
    assert_eq!(
        serde_json::from_slice::<Value>(&recovered.stdout)?["untrusted_event_count"],
        1
    );
    let connection = Connection::open(cli.github_cache_path("octocat", "work-tracker-data"))?;
    let retained: String = connection.query_row(
        "SELECT changes_json FROM history_entries
         WHERE work_item_id = 41 AND evidence_trust = 'untrusted'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        serde_json::from_str::<Value>(&retained)?["retained_body"],
        damaged_body
    );
    Ok(())
}

#[test]
fn rebaseline_blocks_an_unlocked_issue_with_no_live_status_label() -> Result<()> {
    assert_rebaseline_blocks_ambiguous_status(json!([{"name": "work-tracker:item"}]))
}

#[test]
fn rebaseline_blocks_an_unlocked_issue_with_multiple_live_status_labels() -> Result<()> {
    assert_rebaseline_blocks_ambiguous_status(json!([
        {"name": "work-tracker:item"},
        {"name": "work-tracker:status:active"},
        {"name": "work-tracker:status:blocked"}
    ]))
}

fn assert_rebaseline_blocks_ambiguous_status(labels: Value) -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    let gh = FakeGh::new()?;
    let original_event = event("Original title");
    let original_comment = comment(&original_event)?;
    let invalid_projection = projection("disconnected-projection-head");
    let mut corrupt_issue = issue(&invalid_projection, "2026-09-23T01:02:04Z", false);
    corrupt_issue["labels"] = labels;
    respond_rebaseline(
        &gh,
        1,
        &corrupt_issue,
        &[remote_comment(&original_comment)],
        &json!([]),
        "2026-09-23T04:02:03Z",
    )?;

    let recovered = cli.run_with_fake_gh(
        &gh,
        [
            "--json",
            "recover",
            "41",
            "--mode",
            "rebaseline",
            "--actor",
            "reviewer-a",
            "--reason",
            "reviewed ambiguous status",
        ],
    )?;
    ensure!(!recovered.status.success());
    let error: Value = serde_json::from_slice(&recovered.stderr)?;
    assert_eq!(error["error"]["code"], "github_recovery_still_blocked");
    ensure!(
        !gh.calls()?
            .contains("\tPOST\trepos/octocat/work-tracker-data/issues/41/comments")
    );
    Ok(())
}

#[test]
fn rebaseline_retains_cached_and_observed_edited_variants_as_untrusted() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    let gh = FakeGh::new()?;
    let scenario = seed_edited_integrity(&cli, &gh)?;
    let mut reviewed_live_issue = scenario.corrupt_issue.clone();
    reviewed_live_issue["title"] = json!("Reviewed live title");
    respond_rebaseline(
        &gh,
        5,
        &reviewed_live_issue,
        &[remote_comment(&scenario.edited_comment)],
        &json!([]),
        "2026-09-23T04:02:03Z",
    )?;
    let recovered = cli.run_with_fake_gh(
        &gh,
        [
            "--json",
            "recover",
            "41",
            "--mode",
            "rebaseline",
            "--actor",
            "reviewer-a",
            "--reason",
            "reviewed both variants",
        ],
    )?;
    assert_success(&recovered)?;
    assert_eq!(
        serde_json::from_slice::<Value>(&recovered.stdout)?["untrusted_event_count"],
        2
    );

    let history = cli.run_with_fake_gh(&gh, ["--offline", "--json", "history", "41"])?;
    assert_success(&history)?;
    let entries: Value = serde_json::from_slice(&history.stdout)?;
    assert_eq!(entries.as_array().map(Vec::len), Some(3));
    assert_eq!(entries[0]["trust"], "untrusted");
    assert_eq!(entries[0]["changes"]["title"], "Edited behind Work Tracker");
    assert_eq!(entries[1]["trust"], "untrusted");
    assert_eq!(entries[1]["changes"]["title"], "Original title");
    assert_eq!(entries[2]["kind"], "rebaseline");
    assert_eq!(entries[2]["changes"]["title"], "Reviewed live title");
    assert_eq!(
        entries[2]["changes"]["prior_evidence"][0]["variant"],
        "observed_damaged_copy"
    );
    assert_eq!(
        entries[2]["changes"]["prior_evidence"][1]["variant"],
        "cached_exact_copy"
    );
    Ok(())
}

#[test]
fn rebaseline_retains_markerless_current_and_latched_damaged_variants() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    let gh = FakeGh::new()?;
    let original_event = event("Original title");
    let original_comment = comment(&original_event)?;
    let projection = projection(&hash(&original_event, 9001)?);
    let healthy_issue = issue(&projection, "2026-09-23T01:02:04Z", false);
    let corrupt_issue = issue(&projection, "2026-09-23T02:02:04Z", false);
    let latched_damaged_body = "first damaged body with no structured marker";
    let current_damaged_body = "second damaged body with no structured marker";
    respond_sync(&gh, 1, &healthy_issue, &remote_comment(&original_comment))?;
    assert_success(&cli.run_with_fake_gh(&gh, ["--json", "show", "41"])?)?;
    respond_sync(
        &gh,
        3,
        &corrupt_issue,
        &remote_comment(latched_damaged_body),
    )?;
    assert_success(&cli.run_with_fake_gh(&gh, ["--json", "show", "41"])?)?;

    respond_rebaseline(
        &gh,
        5,
        &corrupt_issue,
        &[remote_comment(current_damaged_body)],
        &json!([]),
        "2026-09-23T04:02:03Z",
    )?;
    let recovered = cli.run_with_fake_gh(
        &gh,
        [
            "--json",
            "recover",
            "41",
            "--mode",
            "rebaseline",
            "--actor",
            "reviewer-a",
            "--reason",
            "reviewed damaged unstructured copy",
        ],
    )?;
    assert_success(&recovered)?;
    assert_eq!(
        serde_json::from_slice::<Value>(&recovered.stdout)?["untrusted_event_count"],
        3
    );
    let history = cli.run_with_fake_gh(&gh, ["--offline", "--json", "history", "41"])?;
    assert_success(&history)?;
    let entries: Value = serde_json::from_slice(&history.stdout)?;
    assert_eq!(entries.as_array().map(Vec::len), Some(4));
    let retained_bodies = entries
        .as_array()
        .context("history should be an array")?
        .iter()
        .filter_map(|entry| entry["changes"]["retained_body"].as_str())
        .collect::<Vec<_>>();
    assert!(retained_bodies.contains(&latched_damaged_body));
    assert!(retained_bodies.contains(&current_damaged_body));
    assert!(
        entries.as_array().unwrap()[..3]
            .iter()
            .all(|entry| entry["trust"] == "untrusted")
    );
    assert_eq!(entries[3]["kind"], "rebaseline");
    Ok(())
}

#[derive(Clone, Serialize)]
struct Event<'a> {
    schema_version: u32,
    event_id: &'a str,
    kind: &'a str,
    actor: &'a str,
    github_actor: &'a str,
    note: Option<&'a str>,
    changes: Value,
}

fn event(title: &str) -> Event<'_> {
    Event {
        schema_version: 1,
        event_id: "genesis-integrity-41",
        kind: "created",
        actor: "agent-a",
        github_actor: "octocat",
        note: Some("original evidence"),
        changes: json!({
            "title": title,
            "description": "Evidence body",
            "status": "active"
        }),
    }
}

fn comment(event: &Event<'_>) -> Result<String> {
    Ok(format!(
        "Work Tracker History Entry: created by agent-a\n\n<!-- work-tracker:event\n{}\n-->",
        serde_json::to_string(event)?
    ))
}

fn hash(event: &Event<'_>, comment_id: i64) -> Result<String> {
    let canonical = serde_json::to_vec(event)?;
    let mut hasher = Sha256::new();
    hasher.update(b"work-tracker-history-v1");
    for part in [&[][..], canonical.as_slice(), &comment_id.to_be_bytes()] {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn projection(expected_hash: &str) -> String {
    projection_for(expected_hash, "genesis-integrity-41", 9001)
}

fn projection_for(expected_hash: &str, event_id: &str, comment_id: i64) -> String {
    format!(
        "Evidence body\n\n<!-- work-tracker:projection\n{}\n-->",
        json!({
            "schema_version": 1,
            "kind": "work_item",
            "event_id": event_id,
            "creation_fingerprint": "creation-integrity-41",
            "creation_event_id_supplied": true,
            "pending_genesis_event_id": null,
            "genesis_comment_id": comment_id,
            "state_revision": 1,
            "head_event_id": event_id,
            "head_comment_id": comment_id,
            "history_hash": expected_hash
        })
    )
}

fn issue(body: &str, updated_at: &str, locked: bool) -> Value {
    json!({
        "number": 41,
        "title": "Original title",
        "body": body,
        "labels": [
            {"name": "work-tracker:item"},
            {"name": "work-tracker:status:active"}
        ],
        "state": "open",
        "locked": locked,
        "updated_at": updated_at
    })
}

fn remote_comment(body: &str) -> Value {
    json!({
        "id": 9001,
        "created_at": "2026-09-23T01:02:03Z",
        "user": {"login": "octocat"},
        "body": body
    })
}

fn respond_sync(gh: &FakeGh, start: usize, issue: &Value, comment: &Value) -> Result<()> {
    gh.respond(start, 0, &json!([[issue]]).to_string(), "")?;
    gh.respond(start + 1, 0, &json!([[comment]]).to_string(), "")?;
    Ok(())
}

struct EditedIntegrityScenario {
    original_comment: String,
    edited_comment: String,
    healthy_issue: Value,
    corrupt_issue: Value,
}

fn seed_edited_integrity(cli: &CliHarness, gh: &FakeGh) -> Result<EditedIntegrityScenario> {
    let original_event = event("Original title");
    let edited_event = event("Edited behind Work Tracker");
    let original_comment = comment(&original_event)?;
    let edited_comment = comment(&edited_event)?;
    let projection = projection(&hash(&original_event, 9001)?);
    let healthy_issue = issue(&projection, "2026-09-23T01:02:04Z", false);
    let corrupt_issue = issue(&projection, "2026-09-23T02:02:04Z", false);
    respond_sync(gh, 1, &healthy_issue, &remote_comment(&original_comment))?;
    assert_success(&cli.run_with_fake_gh(gh, ["--json", "show", "41"])?)?;
    respond_sync(gh, 3, &corrupt_issue, &remote_comment(&edited_comment))?;
    assert_success(&cli.run_with_fake_gh(gh, ["--json", "show", "41"])?)?;
    Ok(EditedIntegrityScenario {
        original_comment,
        edited_comment,
        healthy_issue,
        corrupt_issue,
    })
}

fn respond_rebaseline(
    gh: &FakeGh,
    start: usize,
    issue: &Value,
    comments: &[Value],
    timeline: &Value,
    created_at: &str,
) -> Result<()> {
    gh.respond(start, 0, &issue.to_string(), "")?;
    gh.respond(start + 1, 0, &json!([comments]).to_string(), "")?;
    gh.respond(start + 2, 0, &json!([timeline]).to_string(), "")?;
    gh.respond(start + 3, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(
        start + 4,
        0,
        &json!({
            "id": 9100,
            "created_at": created_at,
            "user": {"login": "octocat"}
        })
        .to_string(),
        "",
    )?;
    gh.respond(start + 5, 0, "", "")?;
    Ok(())
}

#[test]
fn edited_event_stays_inspectable_and_doctor_reports_exact_recovery_evidence() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    let gh = FakeGh::new()?;
    let original_event = event("Original title");
    let edited_event = event("Edited behind Work Tracker");
    let original_comment = comment(&original_event)?;
    let edited_comment = comment(&edited_event)?;
    let expected_hash = hash(&original_event, 9001)?;
    let observed_hash = hash(&edited_event, 9001)?;
    let projection = projection(&expected_hash);
    let healthy_issue = issue(&projection, "2026-09-23T01:02:04Z", false);
    let corrupt_issue = issue(&projection, "2026-09-23T02:02:04Z", false);
    let healthy_comment = remote_comment(&original_comment);
    let corrupt_comment = remote_comment(&edited_comment);

    respond_sync(&gh, 1, &healthy_issue, &healthy_comment)?;
    assert_success(&cli.run_with_fake_gh(&gh, ["--json", "show", "41"])?)?;

    respond_sync(&gh, 3, &corrupt_issue, &corrupt_comment)?;
    let shown = cli.run_with_fake_gh(&gh, ["--json", "show", "41"])?;
    assert_success(&shown)?;
    let shown_json: Value = serde_json::from_slice(&shown.stdout)?;
    assert_eq!(shown_json["title"], "Original title");
    assert_eq!(shown_json["ledger_integrity_error"], true);
    let warning: Value = serde_json::from_slice(&shown.stderr)?;
    assert_eq!(warning["warning"]["code"], "ledger_integrity");

    respond_sync(&gh, 5, &corrupt_issue, &corrupt_comment)?;
    let history = cli.run_with_fake_gh(&gh, ["--json", "history", "41"])?;
    assert_success(&history)?;
    let history_json: Value = serde_json::from_slice(&history.stdout)?;
    assert_eq!(history_json[0]["event_id"], "genesis-integrity-41");
    assert_eq!(history_json[0]["trust"], "trusted");

    let calls_before_offline = gh.calls()?.lines().count();
    let human = cli.run_with_fake_gh(&gh, ["--offline", "show", "41"])?;
    assert_success(&human)?;
    ensure!(std::str::from_utf8(&human.stdout)?.contains("Integrity:   Ledger Integrity Error"));
    ensure!(stderr(&human)?.contains("LEDGER INTEGRITY ERROR"));
    assert_eq!(gh.calls()?.lines().count(), calls_before_offline);

    gh.respond(7, 0, &corrupt_issue.to_string(), "")?;
    gh.respond(8, 0, &json!([[corrupt_comment]]).to_string(), "")?;
    gh.respond(
        9,
        0,
        &json!([[{
            "event": "commented",
            "comment_id": 9001,
            "actor": {"login": "octocat"},
            "created_at": "2026-09-23T01:02:03Z"
        }]])
        .to_string(),
        "",
    )?;
    let diagnosed = cli.run_with_fake_gh(&gh, ["--json", "doctor", "41"])?;
    assert_success(&diagnosed)?;
    let report: Value = serde_json::from_slice(&diagnosed.stdout)?;
    assert_eq!(report["work_item_id"], 41);
    assert_eq!(report["integrity_health"], "ledger_integrity_error");
    assert_eq!(report["first_break"]["kind"], "edited_event");
    assert_eq!(report["first_break"]["github_comment_id"], 9001);
    assert_eq!(report["first_break"]["event_id"], "genesis-integrity-41");
    assert_eq!(report["first_break"]["github_actor"], "octocat");
    assert_eq!(report["first_break"]["expected_hash"], expected_hash);
    assert_eq!(report["first_break"]["observed_hash"], observed_hash);
    assert_eq!(report["first_break"]["cached_exact_copy"], original_comment);
    assert_eq!(report["first_break"]["observed_copy"], edited_comment);
    assert_eq!(
        report["eligible_repair_modes"],
        json!(["restore_exact_copy", "rebaseline"])
    );
    ensure!(
        report["timeline_evidence"]
            .as_array()
            .is_some_and(|v| v.len() == 1)
    );

    let calls_before = gh.calls()?.lines().count();
    let mutation = cli.run_with_fake_gh(
        &gh,
        [
            "--json",
            "note",
            "41",
            "must not publish",
            "--actor",
            "agent-b",
        ],
    )?;
    ensure!(!mutation.status.success());
    let error: Value = serde_json::from_slice(mutation.stderr.as_slice())?;
    assert_eq!(error["error"]["code"], "github_ledger_integrity");
    assert_eq!(gh.calls()?.lines().count(), calls_before);
    ensure!(stderr(&mutation)?.contains("Ledger Integrity Error"));
    Ok(())
}

#[test]
fn unchanged_issue_audit_detects_a_comment_edit_hidden_from_incremental_issue_sync() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    let gh = FakeGh::new()?;
    let original_event = event("Original title");
    let edited_event = event("Edited without issue timestamp change");
    let original_comment = comment(&original_event)?;
    let edited_comment = comment(&edited_event)?;
    let projection = projection(&hash(&original_event, 9001)?);
    let issue = issue(&projection, "2026-09-23T01:02:04Z", false);
    respond_sync(&gh, 1, &issue, &remote_comment(&original_comment))?;
    assert_success(&cli.run_with_fake_gh(&gh, ["--json", "show", "41"])?)?;

    gh.respond(3, 0, "HTTP/2 304\n\n", "")?;
    gh.respond(
        4,
        0,
        &json!([[remote_comment(&edited_comment)]]).to_string(),
        "",
    )?;
    let shown = cli.run_with_fake_gh(&gh, ["--json", "show", "41"])?;
    assert_success(&shown)?;
    let item: Value = serde_json::from_slice(&shown.stdout)?;
    assert_eq!(item["title"], "Original title");
    assert_eq!(item["ledger_integrity_error"], true);
    let warning: Value = serde_json::from_slice(&shown.stderr)?;
    assert_eq!(warning["warning"]["code"], "ledger_integrity");
    let calls = gh.calls()?;
    ensure!(calls.contains("&since=2026-09-23T01:02:04.000Z"));
    assert_eq!(
        calls
            .matches("\tGET\trepos/octocat/work-tracker-data/issues/41/comments?per_page=100")
            .count(),
        2
    );
    Ok(())
}

#[test]
fn unchanged_issue_audit_runs_when_an_unrelated_issue_changed() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    let gh = FakeGh::new()?;
    let original_event = event("Original title");
    let edited_event = event("Edited while another issue changed");
    let original_comment = comment(&original_event)?;
    let edited_comment = comment(&edited_event)?;
    let projection = projection(&hash(&original_event, 9001)?);
    let issue = issue(&projection, "2026-09-23T01:02:04Z", false);
    respond_sync(&gh, 1, &issue, &remote_comment(&original_comment))?;
    assert_success(&cli.run_with_fake_gh(&gh, ["--json", "show", "41"])?)?;

    gh.respond(
        3,
        0,
        &json!([[{
            "number": 99,
            "title": "Unrelated foreign issue",
            "body": "ordinary issue",
            "labels": [{"name": "documentation"}],
            "updated_at": "2026-09-23T02:02:04Z"
        }]])
        .to_string(),
        "",
    )?;
    gh.respond(
        4,
        0,
        &json!([[remote_comment(&edited_comment)]]).to_string(),
        "",
    )?;
    let shown = cli.run_with_fake_gh(&gh, ["--json", "show", "41"])?;
    assert_success(&shown)?;
    let item: Value = serde_json::from_slice(&shown.stdout)?;
    assert_eq!(item["ledger_integrity_error"], true);
    let warning: Value = serde_json::from_slice(&shown.stderr)?;
    assert_eq!(warning["warning"]["code"], "ledger_integrity");
    Ok(())
}

#[test]
fn unchanged_issue_audit_records_every_corrupt_omitted_item() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    let gh = FakeGh::new()?;
    let first_original = event("First original");
    let first_edited = event("First edited");
    let second_original = Event {
        schema_version: 1,
        event_id: "genesis-integrity-42",
        kind: "created",
        actor: "agent-a",
        github_actor: "octocat",
        note: Some("original evidence"),
        changes: json!({
            "title": "Second original",
            "description": "Evidence body",
            "status": "active"
        }),
    };
    let second_edited = Event {
        changes: json!({
            "title": "Second edited",
            "description": "Evidence body",
            "status": "active"
        }),
        ..second_original.clone()
    };
    let first_projection = projection(&hash(&first_original, 9001)?);
    let second_projection =
        projection_for(&hash(&second_original, 9002)?, "genesis-integrity-42", 9002);
    let mut first_issue = issue(&first_projection, "2026-09-23T01:02:04Z", false);
    first_issue["title"] = Value::String("First original".to_owned());
    let second_issue = json!({
        "number": 42,
        "title": "Second original",
        "body": second_projection,
        "labels": [
            {"name": "work-tracker:item"},
            {"name": "work-tracker:status:active"}
        ],
        "state": "open",
        "locked": false,
        "updated_at": "2026-09-23T01:02:05Z"
    });
    let second_original_comment = json!({
        "id": 9002,
        "created_at": "2026-09-23T01:02:04Z",
        "user": {"login": "octocat"},
        "body": comment(&second_original)?
    });
    gh.respond(1, 0, &json!([[first_issue, second_issue]]).to_string(), "")?;
    gh.respond(
        2,
        0,
        &json!([[remote_comment(&comment(&first_original)?)]]).to_string(),
        "",
    )?;
    gh.respond(3, 0, &json!([[second_original_comment]]).to_string(), "")?;
    assert_success(&cli.run_with_fake_gh(&gh, ["--json", "list", "--all"])?)?;

    gh.respond(
        4,
        0,
        &json!([[{
            "number": 99,
            "title": "Unrelated foreign issue",
            "body": "ordinary issue",
            "labels": [{"name": "documentation"}],
            "updated_at": "2026-09-23T02:02:04Z"
        }]])
        .to_string(),
        "",
    )?;
    gh.respond(
        5,
        0,
        &json!([[remote_comment(&comment(&first_edited)?)]]).to_string(),
        "",
    )?;
    gh.respond(
        6,
        0,
        &json!([[{
            "id": 9002,
            "created_at": "2026-09-23T01:02:04Z",
            "user": {"login": "octocat"},
            "body": comment(&second_edited)?
        }]])
        .to_string(),
        "",
    )?;
    let listed = cli.run_with_fake_gh(&gh, ["--json", "list", "--all"])?;
    assert_success(&listed)?;
    let items: Value = serde_json::from_slice(&listed.stdout)?;
    assert_eq!(items[0]["ledger_integrity_error"], true);
    assert_eq!(items[1]["ledger_integrity_error"], true);
    Ok(())
}

#[test]
fn restored_remote_evidence_does_not_clear_latched_integrity() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    let gh = FakeGh::new()?;
    let original_event = event("Original title");
    let edited_event = event("Edited behind Work Tracker");
    let original_comment = comment(&original_event)?;
    let edited_comment = comment(&edited_event)?;
    let projection = projection(&hash(&original_event, 9001)?);
    let healthy_issue = issue(&projection, "2026-09-23T01:02:04Z", false);
    let corrupt_issue = issue(&projection, "2026-09-23T02:02:04Z", false);
    let restored_issue = issue(&projection, "2026-09-23T03:02:04Z", false);

    respond_sync(&gh, 1, &healthy_issue, &remote_comment(&original_comment))?;
    assert_success(&cli.run_with_fake_gh(&gh, ["--json", "show", "41"])?)?;
    respond_sync(&gh, 3, &corrupt_issue, &remote_comment(&edited_comment))?;
    assert_success(&cli.run_with_fake_gh(&gh, ["--json", "show", "41"])?)?;
    respond_sync(&gh, 5, &restored_issue, &remote_comment(&original_comment))?;
    let restored = cli.run_with_fake_gh(&gh, ["--json", "show", "41"])?;
    assert_success(&restored)?;
    assert_eq!(
        serde_json::from_slice::<Value>(&restored.stdout)?["ledger_integrity_error"],
        true
    );

    gh.respond(7, 0, &restored_issue.to_string(), "")?;
    gh.respond(
        8,
        0,
        &json!([[remote_comment(&original_comment)]]).to_string(),
        "",
    )?;
    gh.respond(9, 0, "[[]]", "")?;
    let diagnosed = cli.run_with_fake_gh(&gh, ["--json", "doctor", "41"])?;
    assert_success(&diagnosed)?;
    let report: Value = serde_json::from_slice(&diagnosed.stdout)?;
    assert_eq!(report["integrity_health"], "ledger_integrity_error");
    assert_eq!(report["first_break"]["kind"], "edited_event");
    assert_eq!(
        report["eligible_repair_modes"],
        json!(["restore_exact_copy", "rebaseline"])
    );

    let calls_before = gh.calls()?.lines().count();
    let mutation = cli.run_with_fake_gh(
        &gh,
        [
            "--json",
            "note",
            "41",
            "must remain blocked",
            "--actor",
            "agent-b",
        ],
    )?;
    ensure!(!mutation.status.success());
    assert_eq!(gh.calls()?.lines().count(), calls_before);
    Ok(())
}

#[test]
fn unknown_event_schema_survives_cache_rebuild_and_blocks_mutation() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    let gh = FakeGh::new()?;
    let expected_hash = hash(&event("Original title"), 9001)?;
    let projection = projection(&expected_hash);
    let issue = issue(&projection, "2026-09-23T03:02:04Z", false);
    let unknown_body = format!(
        "Work Tracker History Entry: created by agent-a\n\n<!-- work-tracker:event\n{}\n-->",
        json!({
            "schema_version": 2,
            "event_id": "genesis-integrity-41",
            "kind": "created",
            "actor": "agent-a",
            "github_actor": "octocat",
            "note": "future evidence",
            "changes": {
                "title": "Original title",
                "description": "Evidence body",
                "status": "active"
            }
        })
    );
    let comment = remote_comment(&unknown_body);
    respond_sync(&gh, 1, &issue, &comment)?;

    let shown = cli.run_with_fake_gh(&gh, ["--json", "show", "41"])?;
    assert_success(&shown)?;
    let item: Value = serde_json::from_slice(&shown.stdout)?;
    assert_eq!(item["title"], "Original title");
    assert_eq!(item["ledger_integrity_error"], true);
    let warning: Value = serde_json::from_slice(&shown.stderr)?;
    assert_eq!(warning["warning"]["code"], "ledger_integrity");

    gh.respond(3, 0, &issue.to_string(), "")?;
    gh.respond(4, 0, &json!([[comment]]).to_string(), "")?;
    gh.respond(
        5,
        0,
        &json!([[{
            "event": "comment_deleted",
            "comment_id": 7777,
            "actor": {"login": "discussion-author"},
            "created_at": "2026-09-23T02:00:00Z"
        }]])
        .to_string(),
        "",
    )?;
    let diagnosed = cli.run_with_fake_gh(&gh, ["--json", "doctor", "41"])?;
    assert_success(&diagnosed)?;
    let report: Value = serde_json::from_slice(&diagnosed.stdout)?;
    assert_eq!(report["first_break"]["kind"], "unknown_schema_version");
    assert_eq!(report["first_break"]["github_comment_id"], 9001);
    assert_eq!(report["first_break"]["github_actor"], "octocat");
    assert_eq!(report["eligible_repair_modes"], json!(["rebaseline"]));

    let calls_before = gh.calls()?.lines().count();
    let mutation = cli.run_with_fake_gh(
        &gh,
        [
            "--json", "update", "41", "--title", "unsafe", "--actor", "agent-b",
        ],
    )?;
    ensure!(!mutation.status.success());
    assert_eq!(gh.calls()?.lines().count(), calls_before);
    Ok(())
}

#[test]
fn rebuilt_cache_marks_hash_continuity_failure_evidence_untrusted() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    let gh = FakeGh::new()?;
    let original_event = event("Original title");
    let original_comment = comment(&original_event)?;
    let actual_hash = hash(&original_event, 9001)?;
    let projection = projection(&"0".repeat(64));
    let issue = issue(&projection, "2026-09-23T04:02:04Z", false);
    let comment = remote_comment(&original_comment);
    respond_sync(&gh, 1, &issue, &comment)?;

    let history = cli.run_with_fake_gh(&gh, ["--json", "history", "41"])?;
    assert_success(&history)?;
    let entries: Value = serde_json::from_slice(&history.stdout)?;
    assert_eq!(entries[0]["trust"], "untrusted");
    assert_eq!(entries[0]["history_hash"], actual_hash);

    gh.respond(3, 0, &issue.to_string(), "")?;
    gh.respond(4, 0, &json!([[comment]]).to_string(), "")?;
    gh.respond(5, 0, "[[]]", "")?;
    let diagnosed = cli.run_with_fake_gh(&gh, ["--json", "doctor", "41"])?;
    assert_success(&diagnosed)?;
    let report: Value = serde_json::from_slice(&diagnosed.stdout)?;
    assert_eq!(report["first_break"]["kind"], "broken_hash_continuity");
    assert_eq!(report["first_break"]["expected_hash"], "0".repeat(64));
    assert_eq!(report["first_break"]["observed_hash"], actual_hash);
    assert_eq!(report["trusted_event_count"], 0);
    assert_eq!(report["untrusted_event_count"], 1);
    ensure!(!gh.calls()?.contains("\tPATCH\t"));
    Ok(())
}

#[derive(Serialize)]
struct ArchiveEvent<'a> {
    schema_version: u32,
    event_id: &'a str,
    kind: &'a str,
    actor: &'a str,
    github_actor: &'a str,
    note: Option<&'a str>,
    changes: Value,
    expected_state_revision: u64,
}

fn chained_hash<T: Serialize>(
    previous: Option<&str>,
    event: &T,
    comment_id: i64,
) -> Result<String> {
    let canonical = serde_json::to_vec(event)?;
    let mut hasher = Sha256::new();
    hasher.update(b"work-tracker-history-v1");
    for part in [
        previous.unwrap_or("").as_bytes(),
        canonical.as_slice(),
        &comment_id.to_be_bytes(),
    ] {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[test]
fn deleted_event_on_archived_item_is_diagnosed_without_unlocking_it() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    let gh = FakeGh::new()?;
    let genesis = event("Original title");
    let archive = ArchiveEvent {
        schema_version: 1,
        event_id: "archive-integrity-41",
        kind: "archived",
        actor: "agent-a",
        github_actor: "octocat",
        note: Some("retain forever"),
        changes: json!({"status": {"from": "active", "to": "archived"}}),
        expected_state_revision: 1,
    };
    let genesis_body = comment(&genesis)?;
    let archive_body = format!(
        "Work Tracker Mutation Proposal: archived by agent-a\n\nNote: retain forever\n\n<!-- work-tracker:event\n{}\n-->",
        serde_json::to_string(&archive)?
    );
    let genesis_hash = chained_hash(None, &genesis, 9001)?;
    let archive_hash = chained_hash(Some(&genesis_hash), &archive, 9002)?;
    let projection = format!(
        "Evidence body\n\n<!-- work-tracker:projection\n{}\n-->",
        json!({
            "schema_version": 1,
            "kind": "work_item",
            "event_id": "genesis-integrity-41",
            "creation_fingerprint": "creation-integrity-41",
            "creation_event_id_supplied": true,
            "pending_genesis_event_id": null,
            "genesis_comment_id": 9001,
            "state_revision": 2,
            "head_event_id": "archive-integrity-41",
            "head_comment_id": 9002,
            "history_hash": archive_hash
        })
    );
    let archived_issue = json!({
        "number": 41,
        "title": "Original title",
        "body": projection,
        "labels": [
            {"name": "work-tracker:item"},
            {"name": "work-tracker:status:archived"}
        ],
        "state": "closed",
        "state_reason": "not_planned",
        "locked": true,
        "updated_at": "2026-09-23T05:02:04Z"
    });
    let genesis_comment = remote_comment(&genesis_body);
    let archive_comment = json!({
        "id": 9002,
        "created_at": "2026-09-23T02:02:03Z",
        "user": {"login": "octocat"},
        "body": archive_body
    });
    gh.respond(1, 0, &json!([[archived_issue]]).to_string(), "")?;
    gh.respond(
        2,
        0,
        &json!([[genesis_comment, archive_comment]]).to_string(),
        "",
    )?;
    assert_success(&cli.run_with_fake_gh(&gh, ["--json", "show", "41"])?)?;

    gh.respond(3, 0, &json!([[archived_issue]]).to_string(), "")?;
    gh.respond(4, 0, &json!([[archive_comment]]).to_string(), "")?;
    let shown = cli.run_with_fake_gh(&gh, ["--json", "show", "41"])?;
    assert_success(&shown)?;
    assert_eq!(
        serde_json::from_slice::<Value>(&shown.stdout)?["status"],
        "archived"
    );

    gh.respond(5, 0, &archived_issue.to_string(), "")?;
    gh.respond(6, 0, &json!([[archive_comment]]).to_string(), "")?;
    gh.respond(
        7,
        0,
        &json!([[{
            "event": "comment_deleted",
            "comment_id": 9001,
            "actor": {"login": "repository-admin"},
            "created_at": "2026-09-23T05:00:00Z"
        }]])
        .to_string(),
        "",
    )?;
    let diagnosed = cli.run_with_fake_gh(&gh, ["--json", "doctor", "41"])?;
    assert_success(&diagnosed)?;
    let report: Value = serde_json::from_slice(&diagnosed.stdout)?;
    assert_eq!(report["first_break"]["kind"], "deleted_event");
    assert_eq!(report["first_break"]["github_comment_id"], 9001);
    assert_eq!(report["archived"], true);
    assert_eq!(report["timeline_evidence"][0]["event"], "comment_deleted");

    let calls_before = gh.calls()?.lines().count();
    let mutation = cli.run_with_fake_gh(
        &gh,
        ["--json", "status", "41", "active", "--actor", "agent-b"],
    )?;
    ensure!(!mutation.status.success());
    let archive = cli.run_with_fake_gh(&gh, ["--json", "archive", "41", "--actor", "agent-b"])?;
    ensure!(!archive.status.success());
    let calls = gh.calls()?;
    assert_eq!(calls.lines().count(), calls_before);
    ensure!(!calls.contains("\tDELETE\trepos/octocat/work-tracker-data/issues/41/lock"));
    Ok(())
}

#[test]
fn disconnected_projection_head_is_reported_separately_from_hash_mismatch() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    let gh = FakeGh::new()?;
    let genesis = event("Original title");
    let genesis_body = comment(&genesis)?;
    let projection = format!(
        "Evidence body\n\n<!-- work-tracker:projection\n{}\n-->",
        json!({
            "schema_version": 1,
            "kind": "work_item",
            "event_id": "genesis-integrity-41",
            "creation_fingerprint": "creation-integrity-41",
            "creation_event_id_supplied": true,
            "pending_genesis_event_id": null,
            "genesis_comment_id": 9001,
            "state_revision": 1,
            "head_event_id": "missing-head",
            "head_comment_id": 9999,
            "history_hash": "f".repeat(64)
        })
    );
    let issue = issue(&projection, "2026-09-23T06:02:04Z", false);
    let comment = remote_comment(&genesis_body);
    respond_sync(&gh, 1, &issue, &comment)?;
    assert_success(&cli.run_with_fake_gh(&gh, ["--json", "show", "41"])?)?;

    gh.respond(3, 0, &issue.to_string(), "")?;
    gh.respond(4, 0, &json!([[comment]]).to_string(), "")?;
    gh.respond(5, 0, "[[]]", "")?;
    let diagnosed = cli.run_with_fake_gh(&gh, ["--json", "doctor", "41"])?;
    assert_success(&diagnosed)?;
    let report: Value = serde_json::from_slice(&diagnosed.stdout)?;
    assert_eq!(report["first_break"]["kind"], "head_mismatch");
    assert_eq!(report["first_break"]["github_comment_id"], 9001);
    assert_eq!(report["first_break"]["event_id"], "genesis-integrity-41");
    Ok(())
}

#[test]
fn doctor_is_read_only_and_does_not_misclassify_projection_drift() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    let gh = FakeGh::new()?;
    let genesis = event("Original title");
    let genesis_body = comment(&genesis)?;
    let expected_hash = hash(&genesis, 9001)?;
    let projection = projection(&expected_hash);
    let mut drifted_issue = issue(&projection, "2026-09-23T07:02:04Z", false);
    drifted_issue["title"] = Value::String("Unsupported direct edit".to_owned());
    let comment = remote_comment(&genesis_body);
    gh.respond(1, 0, &drifted_issue.to_string(), "")?;
    gh.respond(2, 0, &json!([[comment]]).to_string(), "")?;
    gh.respond(3, 0, "[[]]", "")?;

    let diagnosed = cli.run_with_fake_gh(&gh, ["--json", "doctor", "41"])?;
    assert_success(&diagnosed)?;
    let report: Value = serde_json::from_slice(&diagnosed.stdout)?;
    assert_eq!(report["integrity_health"], "healthy");
    assert_eq!(report["first_break"], Value::Null);
    assert_eq!(report["trusted_event_count"], 1);
    gh.respond(4, 0, &drifted_issue.to_string(), "")?;
    gh.respond(5, 0, &json!([[comment]]).to_string(), "")?;
    gh.respond(6, 0, "[[]]", "")?;
    let human = cli.run_with_fake_gh(&gh, ["doctor", "41"])?;
    assert_success(&human)?;
    let rendered = std::str::from_utf8(&human.stdout)?;
    ensure!(rendered.contains("Integrity health: healthy"));
    ensure!(rendered.contains("Evidence:    1 trusted, 0 untrusted"));
    ensure!(rendered.contains("Timeline evidence: []"));
    let calls = gh.calls()?;
    ensure!(!calls.contains("\tPATCH\t"));
    ensure!(!calls.contains("\tPOST\t"));
    ensure!(!calls.contains("\tPUT\t"));
    ensure!(!calls.contains("\tDELETE\t"));
    Ok(())
}

#[test]
fn deleted_event_timeline_is_diagnosable_during_cache_reconstruction() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    let gh = FakeGh::new()?;
    let expected_hash = hash(&event("Original title"), 9001)?;
    let projection = projection(&expected_hash);
    let issue = issue(&projection, "2026-09-23T08:02:04Z", false);
    gh.respond(1, 0, &json!([[issue]]).to_string(), "")?;
    gh.respond(2, 0, "[[]]", "")?;
    let shown = cli.run_with_fake_gh(&gh, ["--json", "show", "41"])?;
    assert_success(&shown)?;
    assert_eq!(
        serde_json::from_slice::<Value>(&shown.stdout)?["ledger_integrity_error"],
        true
    );

    gh.respond(3, 0, &issue.to_string(), "")?;
    gh.respond(4, 0, "[[]]", "")?;
    gh.respond(
        5,
        0,
        &json!([[{
            "event": "comment_deleted",
            "comment_id": 9001,
            "event_id": "genesis-integrity-41",
            "actor": {"login": "repository-admin"},
            "created_at": "2026-09-23T08:00:00Z"
        }]])
        .to_string(),
        "",
    )?;
    let diagnosed = cli.run_with_fake_gh(&gh, ["--json", "doctor", "41"])?;
    assert_success(&diagnosed)?;
    let report: Value = serde_json::from_slice(&diagnosed.stdout)?;
    assert_eq!(report["first_break"]["kind"], "deleted_event");
    assert_eq!(report["first_break"]["github_comment_id"], 9001);
    assert_eq!(report["first_break"]["event_id"], "genesis-integrity-41");
    assert_eq!(report["first_break"]["github_actor"], "repository-admin");
    assert_eq!(report["first_break"]["cached_exact_copy"], Value::Null);
    assert_eq!(report["eligible_repair_modes"], json!(["rebaseline"]));
    Ok(())
}

#[test]
fn deleted_interior_event_is_correlated_without_claiming_unrelated_deletions() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    let gh = FakeGh::new()?;
    let genesis = event("Original title");
    let note = Event {
        schema_version: 1,
        event_id: "note-integrity-41",
        kind: "noted",
        actor: "agent-a",
        github_actor: "octocat",
        note: Some("middle evidence"),
        changes: json!({}),
    };
    let archive = ArchiveEvent {
        schema_version: 1,
        event_id: "archive-integrity-41",
        kind: "archived",
        actor: "agent-a",
        github_actor: "octocat",
        note: Some("retain forever"),
        changes: json!({"status": {"from": "active", "to": "archived"}}),
        expected_state_revision: 1,
    };
    let genesis_hash = chained_hash(None, &genesis, 9001)?;
    let note_hash = chained_hash(Some(&genesis_hash), &note, 9002)?;
    let archive_hash = chained_hash(Some(&note_hash), &archive, 9003)?;
    let projection = format!(
        "Evidence body\n\n<!-- work-tracker:projection\n{}\n-->",
        json!({
            "schema_version": 1,
            "kind": "work_item",
            "event_id": "genesis-integrity-41",
            "creation_fingerprint": "creation-integrity-41",
            "creation_event_id_supplied": true,
            "pending_genesis_event_id": null,
            "genesis_comment_id": 9001,
            "state_revision": 2,
            "head_event_id": "archive-integrity-41",
            "head_comment_id": 9003,
            "history_hash": archive_hash
        })
    );
    let archived_issue = json!({
        "number": 41,
        "title": "Original title",
        "body": projection,
        "labels": [
            {"name": "work-tracker:item"},
            {"name": "work-tracker:status:archived"}
        ],
        "state": "closed",
        "state_reason": "not_planned",
        "locked": true,
        "updated_at": "2026-09-23T09:02:04Z"
    });
    let genesis_comment = remote_comment(&comment(&genesis)?);
    let archive_comment = json!({
        "id": 9003,
        "created_at": "2026-09-23T03:02:03Z",
        "user": {"login": "octocat"},
        "body": format!(
            "Work Tracker Mutation Proposal: archived by agent-a\n\nNote: retain forever\n\n<!-- work-tracker:event\n{}\n-->",
            serde_json::to_string(&archive)?
        )
    });
    gh.respond(1, 0, &json!([[archived_issue]]).to_string(), "")?;
    gh.respond(
        2,
        0,
        &json!([[genesis_comment, archive_comment]]).to_string(),
        "",
    )?;
    assert_success(&cli.run_with_fake_gh(&gh, ["--json", "show", "41"])?)?;

    gh.respond(3, 0, &archived_issue.to_string(), "")?;
    gh.respond(
        4,
        0,
        &json!([[genesis_comment, archive_comment]]).to_string(),
        "",
    )?;
    gh.respond(
        5,
        0,
        &json!([[
            {
                "event": "comment_deleted",
                "comment_id": 8999,
                "actor": {"login": "discussion-author"}
            },
            {
                "event": "comment_deleted",
                "comment_id": 9002,
                "event_id": "note-integrity-41",
                "actor": {"login": "repository-admin"}
            }
        ]])
        .to_string(),
        "",
    )?;
    let diagnosed = cli.run_with_fake_gh(&gh, ["--json", "doctor", "41"])?;
    assert_success(&diagnosed)?;
    let report: Value = serde_json::from_slice(&diagnosed.stdout)?;
    assert_eq!(report["first_break"]["kind"], "deleted_event");
    assert_eq!(report["first_break"]["github_comment_id"], 9002);
    assert_eq!(report["first_break"]["event_id"], "note-integrity-41");
    assert_eq!(report["first_break"]["github_actor"], "repository-admin");
    Ok(())
}

#[test]
fn archived_rebaseline_stays_locked_and_remains_immutable() -> Result<()> {
    let cli = CliHarness::new()?;
    configure_github(&cli)?;
    let gh = FakeGh::new()?;
    let genesis = event("Original title");
    let archive = ArchiveEvent {
        schema_version: 1,
        event_id: "archive-integrity-41",
        kind: "archived",
        actor: "agent-a",
        github_actor: "octocat",
        note: Some("retain forever"),
        changes: json!({"status": {"from": "active", "to": "archived"}}),
        expected_state_revision: 1,
    };
    let genesis_body = comment(&genesis)?;
    let archive_body = format!(
        "Work Tracker Mutation Proposal: archived by agent-a\n\nNote: retain forever\n\n<!-- work-tracker:event\n{}\n-->",
        serde_json::to_string(&archive)?
    );
    let genesis_hash = chained_hash(None, &genesis, 9001)?;
    let archive_hash = chained_hash(Some(&genesis_hash), &archive, 9002)?;
    let projection = format!(
        "Evidence body\n\n<!-- work-tracker:projection\n{}\n-->",
        json!({
            "schema_version": 1,
            "kind": "work_item",
            "event_id": "genesis-integrity-41",
            "creation_fingerprint": "creation-integrity-41",
            "creation_event_id_supplied": true,
            "pending_genesis_event_id": null,
            "genesis_comment_id": 9001,
            "state_revision": 2,
            "head_event_id": "archive-integrity-41",
            "head_comment_id": 9002,
            "history_hash": archive_hash
        })
    );
    let archived_issue = json!({
        "number": 41,
        "title": "Original title",
        "body": projection,
        "labels": [
            {"name": "work-tracker:item"},
            {"name": "work-tracker:status:archived"}
        ],
        "state": "closed",
        "state_reason": "not_planned",
        "locked": true,
        "updated_at": "2026-09-23T05:02:04Z"
    });
    let genesis_comment = remote_comment(&genesis_body);
    let archive_comment = json!({
        "id": 9002,
        "created_at": "2026-09-23T02:02:03Z",
        "user": {"login": "octocat"},
        "body": archive_body
    });
    gh.respond(1, 0, &json!([[archived_issue]]).to_string(), "")?;
    gh.respond(
        2,
        0,
        &json!([[genesis_comment, archive_comment]]).to_string(),
        "",
    )?;
    assert_success(&cli.run_with_fake_gh(&gh, ["--json", "show", "41"])?)?;

    let mut damaged_archived_issue = archived_issue.clone();
    damaged_archived_issue["labels"][1]["name"] = json!("work-tracker:status:pending");
    damaged_archived_issue["state"] = json!("open");
    damaged_archived_issue["state_reason"] = Value::Null;
    gh.respond(
        3,
        0,
        &json!([[damaged_archived_issue.clone()]]).to_string(),
        "",
    )?;
    gh.respond(4, 0, &json!([[archive_comment]]).to_string(), "")?;
    assert_success(&cli.run_with_fake_gh(&gh, ["--json", "show", "41"])?)?;

    respond_rebaseline(
        &gh,
        5,
        &damaged_archived_issue,
        std::slice::from_ref(&archive_comment),
        &json!([{
            "event": "comment_deleted",
            "comment_id": 9001,
            "actor": {"login": "repository-admin"}
        }]),
        "2026-09-23T06:02:03Z",
    )?;
    let recovered = cli.run_with_fake_gh(
        &gh,
        [
            "recover",
            "41",
            "--mode",
            "rebaseline",
            "--actor",
            "reviewer-a",
            "--reason",
            "reviewed retained archive",
        ],
    )?;
    assert_success(&recovered)?;
    let human = std::str::from_utf8(&recovered.stdout)?;
    ensure!(human.contains("Recovery:    rebaseline"));
    ensure!(human.contains("Archived:    true"));
    let calls = gh.calls()?;
    ensure!(!calls.contains("\trepos/octocat/work-tracker-data/issues/41/lock"));

    let shown = cli.run_with_fake_gh(&gh, ["--offline", "--json", "show", "41"])?;
    assert_success(&shown)?;
    let item: Value = serde_json::from_slice(&shown.stdout)?;
    assert_eq!(item["status"], "archived");
    ensure!(item["ledger_integrity_error"].is_null());
    Ok(())
}
