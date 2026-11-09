use std::{
    io::Write as _,
    path::Path,
    process::{Command, Output, Stdio},
};

use serde_json::{Value, json};
use tempfile::TempDir;

fn command(database: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_work-tracker"));
    command.arg("--database").arg(database).env("NO_COLOR", "1");
    command
}

fn run(database: &Path, args: &[&str]) -> Output {
    command(database).args(args).output().unwrap()
}

fn hook(database: &Path, stdin: &str) -> Output {
    let mut child = command(database)
        .args(["hook", "session-start"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn context(output: &Output) -> String {
    assert!(output.status.success(), "{output:?}");
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        envelope["hookSpecificOutput"]["hookEventName"],
        "SessionStart"
    );
    envelope["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .to_owned()
}

struct Fixture {
    _directory: TempDir,
    database: std::path::PathBuf,
    project: std::path::PathBuf,
    other: std::path::PathBuf,
}

fn fixture() -> Fixture {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("tracker.db");
    let project = directory.path().join("project");
    let other = directory.path().join("other");
    std::fs::create_dir(&project).unwrap();
    std::fs::create_dir(&other).unwrap();
    let project_arg = project.to_str().unwrap();
    for (title, status, workdir) in [
        ("Watch CI", "waiting", project_arg),
        ("Finished work", "done", project_arg),
        ("Elsewhere", "active", other.to_str().unwrap()),
    ] {
        let output = run(
            &database,
            &[
                "add",
                title,
                "--status",
                status,
                "--workdir",
                workdir,
                "--actor",
                "brief",
                "--description",
                "description stays out",
            ],
        );
        assert!(output.status.success(), "{output:?}");
    }
    let output = run(
        &database,
        &["note", "1", "queued at abc123", "--actor", "brief"],
    );
    assert!(output.status.success(), "{output:?}");
    Fixture {
        _directory: directory,
        database,
        project,
        other,
    }
}

#[test]
fn brief_shows_only_actionable_items_of_the_work_directory() {
    let fixture = fixture();
    let workdir = fixture.project.to_str().unwrap();
    let output = run(&fixture.database, &["brief", "--workdir", workdir]);
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        text.contains("#1 [waiting] Watch CI\n  Latest note: queued at abc123\n"),
        "{text}"
    );
    assert!(!text.contains("Finished work"));
    assert!(!text.contains("Elsewhere"));
    assert!(!text.contains("description stays out"));

    let output = run(
        &fixture.database,
        &["brief", "--workdir", workdir, "--json"],
    );
    let brief: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        (brief["shown"].clone(), brief["omitted"].clone()),
        (json!(1), json!(0))
    );
    assert_eq!(brief["context"].as_str().unwrap(), text);
}

#[test]
fn brief_reports_an_empty_directory() {
    let fixture = fixture();
    let empty = fixture.project.join("sub");
    std::fs::create_dir(&empty).unwrap();
    let output = run(
        &fixture.database,
        &["brief", "--workdir", empty.to_str().unwrap()],
    );
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "No actionable work items in this Work Directory.\n"
    );
}

#[test]
fn session_start_hook_injects_the_brief_for_the_hook_cwd() {
    let fixture = fixture();
    let input = json!({"source": "startup", "cwd": fixture.project}).to_string();
    let text = context(&hook(&fixture.database, &input));
    assert!(text.contains("Watch CI"), "{text}");

    let input = json!({"source": "compact", "cwd": fixture.other}).to_string();
    let text = context(&hook(&fixture.database, &input));
    assert!(
        text.contains("Elsewhere") && !text.contains("Watch CI"),
        "{text}"
    );
}

#[test]
fn session_start_hook_skips_resumed_sessions() {
    let fixture = fixture();
    let input = json!({"source": "resume", "cwd": fixture.project}).to_string();
    assert_eq!(context(&hook(&fixture.database, &input)), "");
}

#[test]
fn session_start_hook_exits_zero_with_an_empty_context_on_failure() {
    let fixture = fixture();
    let output = hook(&fixture.database, "not json");
    assert_eq!(context(&output), "");
    assert!(String::from_utf8_lossy(&output.stderr).starts_with("work-tracker hook:"));

    let input = json!({"source": "startup", "cwd": "/does/not/exist"}).to_string();
    assert_eq!(context(&hook(&fixture.database, &input)), "");
}

#[test]
fn session_start_hook_does_not_create_a_missing_database() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("missing.db");
    let input = json!({"source": "startup", "cwd": directory.path()}).to_string();
    assert_eq!(context(&hook(&database, &input)), "");
    assert!(!database.exists());
}
