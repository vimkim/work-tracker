use std::{
    path::Path,
    process::{Command, Output},
};

use serde_json::Value;
use tempfile::TempDir;

fn run(database: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_work-tracker"))
        .arg("--database")
        .arg(database)
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
}

fn json(database: &Path, cwd: &Path, args: &[&str]) -> Value {
    let output = run(database, cwd, &[args, &["--json"]].concat());
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn captures_process_directory_and_records_creation_history() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("tracker.db");
    let project = temp.path().join("project");
    let nested = project.join("src");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::create_dir(project.join(".git")).unwrap();
    let expected = nested.canonicalize().unwrap().to_str().unwrap().to_owned();

    let item = json(&database, &nested, &["add", "Directory context"]);
    assert_eq!(item["workdir"], expected);
    let id = item["id"].to_string();
    let shown = json(&database, &project, &["show", &id]);
    assert_eq!(shown["workdir"], expected);
    let history = json(&database, &project, &["history", &id]);
    assert_eq!(history.as_array().unwrap().len(), 1);
    assert_eq!(history[0]["changes"]["workdir"], expected);
}

#[cfg(unix)]
#[test]
fn explicit_overrides_resolve_relative_paths_symlinks_and_significant_spaces() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("tracker.db");
    let project = temp.path().join(" project with spaces ");
    std::fs::create_dir(&project).unwrap();
    std::os::unix::fs::symlink(&project, temp.path().join("alias")).unwrap();
    let expected = project.canonicalize().unwrap().to_str().unwrap().to_owned();
    for override_path in [
        project.to_str().unwrap(),
        "./ project with spaces ",
        "alias/.",
    ] {
        let item = json(
            &database,
            temp.path(),
            &["add", "Explicit", "--workdir", override_path],
        );
        assert_eq!(item["workdir"], expected);
    }
    let file = temp.path().join("file");
    std::fs::write(&file, "not a directory").unwrap();
    for invalid in ["missing", "file"] {
        let output = run(
            &database,
            temp.path(),
            &["add", "Invalid", "--workdir", invalid, "--json"],
        );
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(serde_json::from_slice::<Value>(&output.stderr).unwrap()["error"].is_string());
    }
    assert_eq!(
        run(&database, temp.path(), &["add", "Invalid", "--workdir", ""])
            .status
            .code(),
        Some(2)
    );
    assert_eq!(
        json(&database, temp.path(), &["list", "--all"])
            .as_array()
            .unwrap()
            .len(),
        3
    );
}

#[test]
fn directory_filters_are_exact_composable_and_applied_before_the_limit() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("tracker.db");
    let a = temp.path().join("a");
    let child = a.join("child");
    let b = temp.path().join("b");
    std::fs::create_dir_all(&child).unwrap();
    std::fs::create_dir(&b).unwrap();
    let selected = json(&database, &a, &["add", "Selected"]);
    json(&database, &child, &["add", "Nested"]);
    json(&database, &b, &["add", "Other"]);
    let done = json(&database, &a, &["add", "Done", "--status", "done"]);
    let deleted = json(&database, &a, &["add", "Deleted"]);
    json(&database, &b, &["delete", &deleted["id"].to_string()]);
    let mut tracker = work_tracker::db::Tracker::open(&database).unwrap();
    let unknown = tracker
        .create(
            "Legacy",
            None,
            work_tracker::domain::Status::Pending,
            "test",
            None,
        )
        .unwrap();
    drop(tracker);

    let here = json(&database, &a, &["list", "--here", "--limit", "1"]);
    assert_eq!(here.as_array().unwrap().len(), 1);
    assert_eq!(here[0]["id"], selected["id"]);
    let relative = json(&database, &b, &["list", "--workdir", "../a"]);
    assert_eq!(relative.as_array().unwrap().len(), 1);
    assert_eq!(relative[0]["id"], selected["id"]);
    assert_eq!(json(&database, &a, &["list"]).as_array().unwrap().len(), 4);
    let finished = json(&database, &a, &["list", "--here", "--status", "done"]);
    assert_eq!(finished.as_array().unwrap().len(), 1);
    assert_eq!(finished[0]["id"], done["id"]);
    assert_eq!(
        json(&database, &a, &["list", "--here", "--all"])
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        json(
            &database,
            &a,
            &["list", "--here", "--all", "--include-deleted"]
        )
        .as_array()
        .unwrap()
        .len(),
        3
    );
    assert_eq!(
        json(&database, &a, &["list", "--here", "--status", "deleted"])[0]["id"],
        deleted["id"]
    );
    let unassigned = json(&database, &a, &["list", "--without-workdir"]);
    assert_eq!(unassigned.as_array().unwrap().len(), 1);
    assert_eq!(unassigned[0]["id"], unknown.id);
    assert!(unassigned[0]["workdir"].is_null());
    for args in [
        vec!["list", "--here", "--workdir", "."],
        vec!["list", "--here", "--without-workdir"],
        vec!["list", "--workdir", ".", "--without-workdir"],
    ] {
        assert_eq!(run(&database, &a, &args).status.code(), Some(2), "{args:?}");
    }
    let text = run(&database, &a, &["list", "--here"]);
    assert!(text.status.success());
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.contains("Selected") && text.contains("--all"));
    assert!(!text.contains("Other") && !text.contains("Nested"));
}

#[cfg(unix)]
#[test]
fn directory_lookup_survives_worktree_removal_and_resolves_existing_ancestors() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("tracker.db");
    let a = temp.path().join("a");
    let child = a.join("removed");
    std::fs::create_dir_all(&child).unwrap();
    std::os::unix::fs::symlink(&a, temp.path().join("alias")).unwrap();
    let item = json(&database, &child, &["add", "Removed worktree"]);
    let via_alias = json(
        &database,
        temp.path(),
        &["list", "--workdir", "alias/removed"],
    );
    assert_eq!(via_alias[0]["id"], item["id"]);
    std::fs::remove_dir(&child).unwrap();
    for path in [
        item["workdir"].as_str().unwrap(),
        "alias/removed",
        "a/missing/../removed",
    ] {
        let found = json(&database, temp.path(), &["list", "--workdir", path]);
        assert_eq!(found.as_array().unwrap().len(), 1);
        assert_eq!(found[0]["id"], item["id"]);
    }
    let history = json(
        &database,
        temp.path(),
        &["history", &item["id"].to_string()],
    );
    assert_eq!(history.as_array().unwrap().len(), 1);
    assert!(
        json(
            &database,
            temp.path(),
            &["list", "--workdir", "never/existed"]
        )
        .as_array()
        .unwrap()
        .is_empty()
    );
}

#[test]
fn cli_corrections_and_clears_preserve_history_and_reject_invalid_assignments() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("tracker.db");
    let a = temp.path().join("a");
    let b = temp.path().join("b");
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&b).unwrap();
    let item = json(&database, &a, &["add", "Original"]);
    let id = item["id"].to_string();
    let moved = json(
        &database,
        &a,
        &[
            "update",
            &id,
            "--workdir",
            "../b",
            "--actor",
            "corrector",
            "--note",
            "Correction",
        ],
    );
    assert_eq!(
        moved["workdir"],
        b.canonicalize().unwrap().to_str().unwrap()
    );
    let history = json(&database, &a, &["history", &id]);
    assert_eq!(history[1]["actor"], "corrector");
    assert_eq!(history[1]["note"], "Correction");
    assert_eq!(history[1]["changes"]["workdir"]["from"], item["workdir"]);
    assert_eq!(history[1]["changes"]["workdir"]["to"], moved["workdir"]);
    let unchanged = json(&database, &b, &["update", &id, "--workdir", "."]);
    assert_eq!(unchanged["updated_at"], moved["updated_at"]);
    assert_eq!(
        json(&database, &a, &["history", &id])
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        json(&database, &a, &["update", &id, "--title", "Renamed"])["workdir"],
        moved["workdir"]
    );
    let invalid = run(
        &database,
        &a,
        &[
            "update",
            &id,
            "--title",
            "Invalid",
            "--workdir",
            "missing",
            "--json",
        ],
    );
    assert_eq!(invalid.status.code(), Some(1));
    assert!(serde_json::from_slice::<Value>(&invalid.stderr).unwrap()["error"].is_string());
    assert_eq!(json(&database, &a, &["show", &id])["title"], "Renamed");
    assert_eq!(
        run(
            &database,
            &a,
            &["update", &id, "--workdir", ".", "--clear-workdir"]
        )
        .status
        .code(),
        Some(2)
    );
    assert!(json(&database, &a, &["update", &id, "--clear-workdir"])["workdir"].is_null());
    assert_eq!(
        json(&database, &a, &["list", "--without-workdir"])[0]["id"],
        item["id"]
    );
    json(&database, &a, &["update", &id, "--clear-workdir"]);
    assert_eq!(
        json(&database, &a, &["history", &id])
            .as_array()
            .unwrap()
            .len(),
        4
    );
    json(&database, &a, &["delete", &id]);
    assert_eq!(
        run(&database, &a, &["update", &id, "--workdir", "."])
            .status
            .code(),
        Some(1)
    );
}

#[test]
fn item_details_show_the_directory_and_existing_json_views_preserve_it() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("tracker.db");
    let item = json(
        &database,
        temp.path(),
        &["add", "Visible directory", "--planned", "2000-01-01"],
    );
    let id = item["id"].to_string();
    let output = run(&database, temp.path(), &["show", &id]);
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("Work dir:") && text.contains(item["workdir"].as_str().unwrap()));
    for command in ["list", "today"] {
        assert_eq!(
            json(&database, temp.path(), &[command])[0]["workdir"],
            item["workdir"]
        );
    }
    assert_eq!(
        json(&database, temp.path(), &["todo"])["actions"][0]["workdir"],
        item["workdir"]
    );
    json(&database, temp.path(), &["update", &id, "--clear-workdir"]);
    let text = String::from_utf8(run(&database, temp.path(), &["show", &id]).stdout).unwrap();
    assert!(text.contains("Work dir:    unknown"));
}

#[cfg(unix)]
#[test]
fn lookup_normalizes_parent_components_before_a_reachable_symlink() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("tracker.db");
    let target = temp.path().join("target");
    std::fs::create_dir(&target).unwrap();
    std::os::unix::fs::symlink(&target, temp.path().join("alias")).unwrap();
    let item = json(&database, &target, &["add", "Physical path"]);
    let result = json(
        &database,
        temp.path(),
        &["list", "--workdir", "missing/../alias"],
    );
    assert_eq!(result.as_array().unwrap().len(), 1);
    assert_eq!(result[0]["id"], item["id"]);
}

#[cfg(unix)]
#[test]
fn unrepresentable_directory_paths_fail_without_creating_items() {
    use std::os::unix::ffi::OsStrExt;
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("tracker.db");
    let invalid = temp
        .path()
        .join(std::ffi::OsStr::from_bytes(b"invalid-\xff"));
    std::fs::create_dir(&invalid).unwrap();
    for args in [
        vec!["add", "Invalid", "--json"],
        vec!["list", "--here", "--json"],
    ] {
        let output = run(&database, &invalid, &args);
        assert_eq!(output.status.code(), Some(1));
        assert!(
            serde_json::from_slice::<Value>(&output.stderr).unwrap()["error"]
                .as_str()
                .unwrap()
                .contains("UTF-8")
        );
    }
    assert!(
        json(&database, temp.path(), &["list", "--all"])
            .as_array()
            .unwrap()
            .is_empty()
    );
}
