mod support;

use std::fs;

use anyhow::{Context, Result, ensure};
use serde_json::Value;
use support::{CliHarness, FakeGh, assert_database_exists, assert_success, stderr, stdout};

fn json(output: &std::process::Output) -> Result<Value> {
    serde_json::from_slice(&output.stdout).context("stdout was not valid JSON")
}

const PRIVATE_REPOSITORY: &str = r#"{"full_name":"octocat/work-tracker-data","private":true,"has_issues":true,"permissions":{"admin":true,"push":true}}"#;

fn configured_labels() -> String {
    let descriptions = [
        (
            "work-tracker:item",
            "0052cc",
            "Issue managed by Work Tracker",
        ),
        (
            "work-tracker:status:pending",
            "5319e7",
            "Work Tracker Status: pending",
        ),
        (
            "work-tracker:status:active",
            "0e8a16",
            "Work Tracker Status: active",
        ),
        (
            "work-tracker:status:waiting",
            "fbca04",
            "Work Tracker Status: waiting",
        ),
        (
            "work-tracker:status:blocked",
            "b60205",
            "Work Tracker Status: blocked",
        ),
        (
            "work-tracker:status:done",
            "1d76db",
            "Work Tracker Status: done",
        ),
        (
            "work-tracker:status:cancelled",
            "6a737d",
            "Work Tracker Status: cancelled",
        ),
        (
            "work-tracker:status:archived",
            "24292e",
            "Work Tracker Status: archived",
        ),
    ];
    let labels = descriptions
        .into_iter()
        .map(|(name, color, description)| {
            serde_json::json!({"name": name, "color": color, "description": description})
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&vec![labels]).expect("static labels serialize")
}

fn configured_labels_across_pages_with_mixed_case() -> String {
    let mut pages: Value =
        serde_json::from_str(&configured_labels()).expect("configured labels are JSON");
    let labels = pages[0]
        .as_array_mut()
        .expect("configured labels have one page");
    labels[0]["name"] = Value::String("Work-Tracker:Item".to_owned());
    let second_page = labels.split_off(4);
    serde_json::to_string(&serde_json::json!([labels, second_page]))
        .expect("paged labels serialize")
}

#[test]
fn init_github_creates_a_private_default_ledger_and_cache() -> Result<()> {
    let cli = CliHarness::new()?;
    let gh = FakeGh::new()?;
    gh.respond(1, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(2, 1, "", "gh: Not Found (HTTP 404)\n")?;
    gh.respond(
        3,
        0,
        r#"{"full_name":"octocat/work-tracker-data","private":true,"has_issues":true,"permissions":{"admin":true,"push":true},"topics":[]}"#,
        "",
    )?;
    gh.respond(4, 0, "[[]]", "")?;

    let initialized = cli.run_with_fake_gh(&gh, ["--json", "init", "github"])?;
    assert_success(&initialized)?;
    assert_eq!(stderr(&initialized)?, "");
    let result = json(&initialized)?;
    assert_eq!(result["backend"], "github");
    assert_eq!(result["repository"], "octocat/work-tracker-data");
    assert_eq!(result["created"], true);
    assert_eq!(result["default"], true);
    assert_eq!(result["private"], true);
    assert_eq!(
        result["cache"],
        cli.github_cache_path("octocat", "work-tracker-data")
            .to_string_lossy()
            .as_ref()
    );

    let config: Value = serde_json::from_slice(&fs::read(cli.config_path())?)?;
    assert_eq!(config["schema_version"], 1);
    assert_eq!(config["default_repository"], "octocat/work-tracker-data");
    assert_database_exists(&cli.github_cache_path("octocat", "work-tracker-data"))?;

    let calls = gh.calls()?;
    ensure!(calls.contains("CALL\tapi\t--method\tGET\tuser"));
    ensure!(calls.contains("CALL\tapi\t--method\tGET\trepos/octocat/work-tracker-data"));
    ensure!(calls.contains("CALL\tapi\t--method\tPOST\tuser/repos"));
    ensure!(calls.contains("\t--field\tprivate=true"));
    for status in [
        "pending",
        "active",
        "waiting",
        "blocked",
        "done",
        "cancelled",
        "archived",
    ] {
        ensure!(calls.contains(&format!("work-tracker:status:{status}")));
    }
    ensure!(calls.contains("work-tracker:item"));
    ensure!(!calls.contains("git remote"));
    ensure!(!calls.contains("token"));
    ensure!(stdout(&initialized)?.starts_with('{'));
    Ok(())
}

#[test]
fn repeating_init_is_idempotent() -> Result<()> {
    let cli = CliHarness::new()?;
    let gh = FakeGh::new()?;
    gh.respond(1, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(2, 1, "", "gh: Not Found (HTTP 404)\n")?;
    gh.respond(3, 0, PRIVATE_REPOSITORY, "")?;
    gh.respond(4, 0, "[[]]", "")?;
    gh.respond(13, 0, PRIVATE_REPOSITORY, "")?;
    gh.respond(14, 0, &configured_labels(), "")?;
    gh.respond(15, 0, "[[]]", "")?;
    gh.respond(16, 0, "[[]]", "")?;

    assert_success(&cli.run_with_fake_gh(&gh, ["init", "github"])?)?;
    let original_config = fs::read(cli.config_path())?;
    let repeated = cli.run_with_fake_gh(&gh, ["--json", "init", "github"])?;
    assert_success(&repeated)?;
    assert_eq!(json(&repeated)?["created"], false);
    assert_eq!(fs::read(cli.config_path())?, original_config);

    let calls = gh.calls()?;
    assert_eq!(calls.matches("\tPOST\tuser/repos").count(), 1);
    assert_eq!(
        calls
            .matches("\tPOST\trepos/octocat/work-tracker-data/labels")
            .count(),
        8
    );
    assert_eq!(calls.matches("\tGET\tuser\n").count(), 1);
    Ok(())
}

#[test]
fn explicit_repository_override_does_not_rewrite_the_default() -> Result<()> {
    let cli = CliHarness::new()?;
    let gh = FakeGh::new()?;
    gh.respond(1, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(2, 1, "", "gh: Not Found (HTTP 404)\n")?;
    gh.respond(3, 0, PRIVATE_REPOSITORY, "")?;
    gh.respond(4, 0, "[[]]", "")?;
    assert_success(&cli.run_with_fake_gh(&gh, ["init", "github"])?)?;

    gh.respond(
        13,
        0,
        r#"{"full_name":"acme/operations","private":true,"has_issues":true,"permissions":{"push":true}}"#,
        "",
    )?;
    gh.respond(14, 0, "[[]]", "")?;
    gh.respond(15, 0, "[[]]", "")?;
    gh.respond(16, 0, "[[]]", "")?;
    gh.respond(
        25,
        0,
        r#"{"full_name":"acme/operations","private":true,"has_issues":true,"permissions":{"push":true}}"#,
        "",
    )?;
    gh.respond(26, 0, &configured_labels(), "")?;
    gh.respond(27, 0, "[[]]", "")?;
    gh.respond(28, 0, "[[]]", "")?;
    let override_init =
        cli.run_with_fake_gh(&gh, ["--json", "init", "github", "acme/operations"])?;
    assert_success(&override_init)?;
    assert_eq!(json(&override_init)?["default"], false);
    assert_database_exists(&cli.github_cache_path("acme", "operations"))?;

    let selected =
        cli.run_with_fake_gh(&gh, ["--json", "--repository", "ACME/Operations", "path"])?;
    assert_success(&selected)?;
    assert_eq!(json(&selected)?["backend"], "github");
    assert_eq!(json(&selected)?["repository"], "acme/operations");
    assert_eq!(
        json(&selected)?["cache"],
        cli.github_cache_path("acme", "operations")
            .to_string_lossy()
            .as_ref()
    );

    let default = cli.run(["--json", "path"])?;
    assert_success(&default)?;
    assert_eq!(json(&default)?["repository"], "octocat/work-tracker-data");
    let config: Value = serde_json::from_slice(&fs::read(cli.config_path())?)?;
    assert_eq!(config["default_repository"], "octocat/work-tracker-data");
    Ok(())
}

#[test]
fn missing_gh_has_a_distinct_machine_readable_diagnostic() -> Result<()> {
    let cli = CliHarness::new()?;
    let gh = FakeGh::new()?;
    gh.remove_executable()?;

    let failed = cli.run_with_fake_gh(
        &gh,
        ["--json", "init", "github", "octocat/work-tracker-data"],
    )?;
    ensure!(!failed.status.success());
    assert_eq!(stdout(&failed)?, "");
    let diagnostic: Value = serde_json::from_slice(failed.stderr.as_slice())?;
    assert_eq!(diagnostic["error"]["code"], "github_cli_missing");
    ensure!(
        diagnostic["error"]["message"]
            .as_str()
            .context("missing diagnostic message")?
            .contains("failed to execute gh")
    );
    ensure!(!cli.config_path().exists());
    Ok(())
}

fn assert_json_error(output: &std::process::Output, code: &str, message: &str) -> Result<()> {
    ensure!(!output.status.success());
    assert_eq!(stdout(output)?, "");
    let diagnostic: Value = serde_json::from_slice(output.stderr.as_slice())?;
    assert_eq!(diagnostic["error"]["code"], code);
    ensure!(
        diagnostic["error"]["message"]
            .as_str()
            .context("missing diagnostic message")?
            .contains(message)
    );
    Ok(())
}

#[test]
fn github_failure_categories_have_distinct_json_diagnostics() -> Result<()> {
    let cases = [
        (
            1,
            "",
            "gh: authentication required\n",
            "github_unauthenticated",
            "authentication required",
        ),
        (
            1,
            "",
            "gh: Resource not accessible (HTTP 403)\n",
            "github_permission_denied",
            "Resource not accessible",
        ),
        (
            0,
            r#"{"full_name":"octocat/data","private":false,"has_issues":true,"permissions":{"push":true}}"#,
            "",
            "github_invalid_visibility",
            "is not private",
        ),
        (
            0,
            r#"{"full_name":"octocat/data","private":true,"has_issues":false,"permissions":{"push":true}}"#,
            "",
            "github_incompatible_repository",
            "does not have issues enabled",
        ),
    ];

    for (exit, response, gh_stderr, code, message) in cases {
        let cli = CliHarness::new()?;
        let gh = FakeGh::new()?;
        gh.respond(1, exit, response, gh_stderr)?;
        let failed = cli.run_with_fake_gh(&gh, ["--json", "init", "github", "octocat/data"])?;
        assert_json_error(&failed, code, message)?;
        ensure!(!cli.config_path().exists());
    }
    Ok(())
}

#[test]
fn github_transport_failures_keep_actionable_json_categories() -> Result<()> {
    let cases = [
        (
            "gh: Validation Failed (HTTP 422)\n",
            "github_validation_failed",
            "Validation Failed",
        ),
        (
            "gh: API rate limit exceeded (HTTP 429)\n",
            "github_rate_limited",
            "rate limit exceeded",
        ),
        (
            "gh: error connecting to api.github.com\n",
            "github_network_failure",
            "error connecting",
        ),
        (
            "gh: GitHub service unavailable (HTTP 503)\n",
            "github_service_failure",
            "service unavailable",
        ),
    ];

    for (gh_stderr, code, message) in cases {
        let cli = CliHarness::new()?;
        let gh = FakeGh::new()?;
        gh.respond(1, 1, "", gh_stderr)?;
        let failed = cli.run_with_fake_gh(&gh, ["--json", "init", "github", "octocat/data"])?;
        assert_json_error(&failed, code, message)?;
        ensure!(!cli.config_path().exists());
    }
    Ok(())
}

#[test]
fn github_transport_failures_remain_distinguishable_in_human_diagnostics() -> Result<()> {
    for (gh_stderr, expected) in [
        ("gh: Validation Failed (HTTP 422)\n", "Validation Failed"),
        (
            "gh: API rate limit exceeded (HTTP 429)\n",
            "rate limit exceeded",
        ),
        (
            "gh: error connecting to api.github.com\n",
            "error connecting",
        ),
        (
            "gh: GitHub service unavailable (HTTP 503)\n",
            "service unavailable",
        ),
    ] {
        let cli = CliHarness::new()?;
        let gh = FakeGh::new()?;
        gh.respond(1, 1, "", gh_stderr)?;
        let failed = cli.run_with_fake_gh(&gh, ["init", "github", "octocat/data"])?;
        ensure!(!failed.status.success());
        ensure!(stderr(&failed)?.starts_with("error: GitHub API request failed:"));
        ensure!(stderr(&failed)?.contains(expected));
    }
    Ok(())
}

#[test]
fn human_diagnostics_distinguish_visibility_permission_and_compatibility() -> Result<()> {
    let cases = [
        (
            0,
            r#"{"full_name":"octocat/data","private":false,"has_issues":true,"permissions":{"push":true}}"#,
            "",
            "not private",
        ),
        (
            0,
            r#"{"full_name":"octocat/data","private":true,"has_issues":true,"permissions":{"push":false}}"#,
            "",
            "does not grant write permission",
        ),
        (
            0,
            r#"{"full_name":"octocat/data","private":true,"has_issues":false,"permissions":{"push":true}}"#,
            "",
            "does not have issues enabled",
        ),
    ];
    for (exit, response, gh_stderr, message) in cases {
        let cli = CliHarness::new()?;
        let gh = FakeGh::new()?;
        gh.respond(1, exit, response, gh_stderr)?;
        let failed = cli.run_with_fake_gh(&gh, ["init", "github", "octocat/data"])?;
        ensure!(!failed.status.success());
        assert_eq!(stdout(&failed)?, "");
        ensure!(stderr(&failed)?.starts_with("error: "));
        ensure!(stderr(&failed)?.contains(message));
    }
    Ok(())
}

#[test]
fn existing_issue_must_have_exactly_one_status_label() -> Result<()> {
    let cli = CliHarness::new()?;
    let gh = FakeGh::new()?;
    gh.respond(
        1,
        0,
        r#"{"full_name":"octocat/data","private":true,"has_issues":true,"permissions":{"push":true}}"#,
        "",
    )?;
    gh.respond(2, 0, "[[]]", "")?;
    gh.respond(
        3,
        0,
        r#"[[{"number":42,"labels":[{"name":"work-tracker:item"}]}]]"#,
        "",
    )?;
    gh.respond(4, 0, "[[]]", "")?;

    let failed = cli.run_with_fake_gh(&gh, ["--json", "init", "github", "octocat/data"])?;
    assert_json_error(
        &failed,
        "github_incompatible_repository",
        "does not have exactly one Work Tracker Status label",
    )?;
    ensure!(!cli.config_path().exists());
    ensure!(!cli.github_cache_path("octocat", "data").exists());
    ensure!(!gh.calls()?.contains("\tPOST\trepos/octocat/data/labels"));
    Ok(())
}

#[test]
fn reserved_label_collision_is_incompatible() -> Result<()> {
    let cli = CliHarness::new()?;
    let gh = FakeGh::new()?;
    gh.respond(
        1,
        0,
        r#"{"full_name":"octocat/data","private":true,"has_issues":true,"permissions":{"push":true}}"#,
        "",
    )?;
    gh.respond(
        2,
        0,
        r#"[[{"name":"work-tracker:item","color":"ffffff","description":"unrelated"}]]"#,
        "",
    )?;

    let failed = cli.run_with_fake_gh(&gh, ["--json", "init", "github", "octocat/data"])?;
    assert_json_error(
        &failed,
        "github_incompatible_repository",
        "incompatible reserved label work-tracker:item",
    )?;
    ensure!(!cli.config_path().exists());
    Ok(())
}

#[test]
fn repository_override_rejects_path_traversal() -> Result<()> {
    let cli = CliHarness::new()?;
    let failed = cli.run(["--json", "--repository", "../../escape", "path"])?;
    ensure!(!failed.status.success());
    assert_eq!(stdout(&failed)?, "");
    ensure!(stderr(&failed)?.contains("valid OWNER/REPO"));
    ensure!(!cli.github_cache_path("..", "escape").exists());
    Ok(())
}

#[test]
fn hidden_private_repository_is_reported_as_a_permission_failure() -> Result<()> {
    let cli = CliHarness::new()?;
    let gh = FakeGh::new()?;
    gh.respond(1, 1, "", "gh: Not Found (HTTP 404)\n")?;
    gh.respond(2, 0, r#"{"login":"viewer"}"#, "")?;
    gh.respond(3, 1, "", "gh: Not Found (HTTP 404)\n")?;

    let failed = cli.run_with_fake_gh(&gh, ["--json", "init", "github", "octocat/private-data"])?;
    assert_json_error(
        &failed,
        "github_permission_denied",
        "could not access or create",
    )?;
    Ok(())
}

#[test]
fn hidden_private_repository_override_is_reported_as_a_permission_failure() -> Result<()> {
    let cli = CliHarness::new()?;
    let gh = FakeGh::new()?;
    gh.respond(1, 1, "", "gh: Not Found (HTTP 404)\n")?;

    let failed = cli.run_with_fake_gh(
        &gh,
        ["--json", "--repository", "octocat/private-data", "path"],
    )?;
    assert_json_error(
        &failed,
        "github_permission_denied",
        "could not access GitHub repository",
    )?;
    Ok(())
}

#[test]
fn existing_repository_rejects_an_issue_without_work_tracker_metadata() -> Result<()> {
    let cli = CliHarness::new()?;
    let gh = FakeGh::new()?;
    gh.respond(
        1,
        0,
        r#"{"full_name":"octocat/data","private":true,"has_issues":true,"permissions":{"push":true}}"#,
        "",
    )?;
    gh.respond(2, 0, &configured_labels(), "")?;
    gh.respond(3, 0, r#"[[{"number":42,"labels":[]}]]"#, "")?;
    gh.respond(4, 0, "[[]]", "")?;

    let failed = cli.run_with_fake_gh(&gh, ["--json", "init", "github", "octocat/data"])?;
    assert_json_error(
        &failed,
        "github_incompatible_repository",
        "unsupported issue #42",
    )?;
    Ok(())
}

#[test]
fn configured_github_default_never_silently_writes_the_legacy_sqlite_ledger() -> Result<()> {
    let cli = CliHarness::new()?;
    let gh = FakeGh::new()?;
    gh.respond(1, 0, r#"{"login":"octocat"}"#, "")?;
    gh.respond(2, 1, "", "gh: Not Found (HTTP 404)\n")?;
    gh.respond(3, 0, PRIVATE_REPOSITORY, "")?;
    gh.respond(4, 0, "[[]]", "")?;
    assert_success(&cli.run_with_fake_gh(&gh, ["init", "github"])?)?;

    let add = cli.run(["--json", "add", "must not become local"])?;
    ensure!(!add.status.success());
    ensure!(stderr(&add)?.contains("failed to execute gh"));
    ensure!(!cli.database_path().exists());

    let explicit_local = cli.run([
        "--json",
        "--database",
        cli.database_path()
            .as_os_str()
            .to_str()
            .context("UTF-8 path")?,
        "add",
        "explicit local item",
    ])?;
    assert_success(&explicit_local)?;
    assert_database_exists(&cli.database_path())?;

    let local_path = cli.run([
        "--json",
        "--database",
        cli.database_path()
            .as_os_str()
            .to_str()
            .context("UTF-8 path")?,
        "path",
    ])?;
    assert_success(&local_path)?;
    assert_eq!(json(&local_path)?["backend"], "sqlite");
    assert_eq!(
        json(&local_path)?["database"],
        cli.database_path().to_string_lossy().as_ref()
    );
    Ok(())
}

#[test]
fn compatibility_checks_all_label_pages_case_insensitively() -> Result<()> {
    let cli = CliHarness::new()?;
    let gh = FakeGh::new()?;
    gh.respond(
        1,
        0,
        r#"{"full_name":"octocat/data","private":true,"has_issues":true,"permissions":{"push":true}}"#,
        "",
    )?;
    gh.respond(2, 0, &configured_labels_across_pages_with_mixed_case(), "")?;
    gh.respond(
        3,
        0,
        r#"[[{"number":7,"labels":[{"name":"work-tracker:item"},{"name":"work-tracker:status:pending"}]}]]"#,
        "",
    )?;
    gh.respond(4, 0, "[[]]", "")?;

    let initialized = cli.run_with_fake_gh(&gh, ["--json", "init", "github", "octocat/data"])?;
    assert_success(&initialized)?;
    let calls = gh.calls()?;
    ensure!(calls.contains("\t--paginate\t--slurp"));
    ensure!(!calls.contains("\tPOST\trepos/octocat/data/labels"));
    Ok(())
}
