use serde_json::Value;
use std::process::{Command, Output};
use tempfile::TempDir;

fn run(temp: &TempDir, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_work-tracker"))
        .arg("--database")
        .arg(temp.path().join("cli.db"))
        .args(args)
        .env("TZ", "Pacific/Honolulu")
        .output()
        .unwrap()
}

#[test]
fn cli_schedule_text_json_clear_and_defaults() {
    let temp = TempDir::new().unwrap();
    let output = run(
        &temp,
        &[
            "add",
            "Presentation",
            "--planned",
            "2000-01-01",
            "--due",
            "2000-01-02",
            "--priority",
            "high",
            "--json",
        ],
    );
    assert!(output.status.success(), "{:?}", output);
    let item: Value = serde_json::from_slice(&output.stdout).unwrap();
    let id = item["id"].to_string();
    assert_eq!(item["priority"], "high");
    let output = run(&temp, &["todo", "--days", "3", "--json"]);
    assert!(output.status.success());
    let view: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(view["window"]["timezone"], "Asia/Seoul");
    assert_eq!(view["window"]["days"], 3);
    assert_eq!(view["actions"][0]["id"], item["id"]);
    assert_eq!(view["actions"][0]["overdue"], true);
    assert_eq!(view["actions"][0]["carried_over"], true);
    assert!(view["blocked_waiting"].as_array().unwrap().is_empty());
    for args in [&["todo"][..], &["todo", "today"][..]] {
        let output = run(&temp, args);
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("Presentation"));
        assert!(text.contains("1 calendar day(s)"));
        assert!(text.contains("[overdue]"));
    }
    assert!(
        run(
            &temp,
            &[
                "update",
                &id,
                "--clear-planned",
                "--clear-due",
                "--priority",
                "normal"
            ]
        )
        .status
        .success()
    );
    let output = run(&temp, &["todo", "--json"]);
    let view: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(view["actions"].as_array().unwrap().is_empty());
    let output = run(&temp, &["list", "--json"]);
    assert!(
        serde_json::from_slice::<Value>(&output.stdout)
            .unwrap()
            .is_array()
    );
    let output = run(&temp, &["today", "--json"]);
    assert!(
        serde_json::from_slice::<Value>(&output.stdout)
            .unwrap()
            .is_array()
    );
}

#[test]
fn invalid_arguments_do_not_create_items() {
    let temp = TempDir::new().unwrap();
    for args in [
        vec!["todo", "today", "--days", "3"],
        vec!["todo", "--days", "0"],
        vec!["todo", "--days", "-1"],
        vec!["todo", "--days", "4294967295"],
        vec!["add", "Bad", "--due", "2026-02-30"],
        vec!["add", "Bad", "--due", "2026-2-03"],
        vec!["add", "Bad", "--due", "0000-01-01"],
        vec!["add", "Bad", "--priority", "urgent"],
        vec!["update", "1", "--planned", "2026-10-06", "--clear-planned"],
        vec!["update", "1", "--due", "2026-10-06", "--clear-due"],
    ] {
        assert!(!run(&temp, &args).status.success(), "{args:?}");
    }
    let output = run(&temp, &["list", "--all", "--json"]);
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        serde_json::json!([])
    );
}

#[test]
fn colors_preserve_plain_layout_and_json_contract() {
    let temp = TempDir::new().unwrap();
    let output = run(
        &temp,
        &[
            "add",
            "Colored item",
            "--planned",
            "2000-01-01",
            "--due",
            "2000-01-02",
            "--priority",
            "high",
            "--status",
            "blocked",
            "--json",
        ],
    );
    assert!(output.status.success());
    let item: Value = serde_json::from_slice(&output.stdout).unwrap();
    let id = item["id"].to_string();
    for args in [vec!["todo"], vec!["list"], vec!["today"], vec!["show", &id]] {
        let plain = run(&temp, &[args.as_slice(), &["--color", "never"]].concat());
        let auto = run(&temp, &args);
        let colored = run(&temp, &[args.as_slice(), &["--color", "always"]].concat());
        assert!(plain.status.success() && auto.status.success() && colored.status.success());
        assert_eq!(auto.stdout, plain.stdout, "captured stdout must stay plain");
        let colored = String::from_utf8(colored.stdout).unwrap();
        assert!(colored.contains("\x1b[31m"), "blocked status should be red");
        let mut stripped = String::new();
        let mut chars = colored.chars();
        while let Some(ch) = chars.next() {
            if ch == '\x1b' {
                assert_eq!(chars.next(), Some('['));
                for ch in chars.by_ref() {
                    if ch == 'm' {
                        break;
                    }
                }
            } else {
                stripped.push(ch);
            }
        }
        assert_eq!(stripped.as_bytes(), plain.stdout);
        let json = run(
            &temp,
            &[args.as_slice(), &["--color=always", "--json"]].concat(),
        );
        assert!(json.status.success());
        assert!(!json.stdout.contains(&0x1b));
        serde_json::from_slice::<Value>(&json.stdout).unwrap();
    }
    assert!(!run(&temp, &["todo", "--color", "invalid"]).status.success());
}
