use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output},
};

use anyhow::{Context, Result, ensure};
use serde_json::Value;

mod support;
use support::{CliHarness, assert_success, stderr, stdout};

fn write_executable(path: &Path, contents: &str) -> Result<()> {
    fs::write(path, contents)?;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

fn run_fake_live_smoke(mode: &str, invalid_uuid: bool) -> Result<(Output, String)> {
    let directory = tempfile::tempdir()?;
    let bin = directory.path().join("bin");
    let work = directory.path().join("work");
    let home = directory.path().join("home");
    let state = directory.path().join("state");
    for path in [&bin, &work, &home, &state] {
        fs::create_dir_all(path)?;
    }

    write_executable(
        &bin.join("cargo"),
        r#"#!/bin/sh
set -eu
mkdir -p target/debug
cp "$FAKE_SMOKE_BINARY_SOURCE" target/debug/work-tracker
chmod +x target/debug/work-tracker
"#,
    )?;
    let fake_binary = directory.path().join("work-tracker");
    write_executable(
        &fake_binary,
        r#"#!/bin/sh
set -eu
for argument in "$@"; do
    case "$argument" in
        init)
            case "$FAKE_SMOKE_MODE" in
                signal-int) kill -INT "$PPID" ;;
                signal-term) kill -TERM "$PPID" ;;
            esac
            ;;
        add) printf '{"id":41}\n'; exit 0 ;;
        path)
            : > "$FAKE_SMOKE_CACHE"
            printf '{"cache":"%s"}\n' "$FAKE_SMOKE_CACHE"
            exit 0
            ;;
    esac
done
printf '{}\n'
"#,
    )?;
    write_executable(
        &bin.join("gh"),
        r#"#!/bin/sh
set -eu
printf 'CALL' >> "$FAKE_SMOKE_STATE/calls"
for argument in "$@"; do printf '\t%s' "$argument" >> "$FAKE_SMOKE_STATE/calls"; done
printf '\n' >> "$FAKE_SMOKE_STATE/calls"
case "${1:-}" in
    auth) exit 0 ;;
    api)
        shift
        if [ "${1:-}" = user ]; then
            printf 'octocat\n'
            exit 0
        fi
        if [ "${1:-}" = --method ]; then
            smoke_name=
            smoke_description=
            for argument in "$@"; do
                case "$argument" in
                    name=*) smoke_name=${argument#name=} ;;
                    description=*) smoke_description=${argument#description=} ;;
                esac
            done
            smoke_repository="octocat/$smoke_name"
            printf '%s' "$smoke_repository" > "$FAKE_SMOKE_STATE/repository"
            printf '%s' "$smoke_description" > "$FAKE_SMOKE_STATE/description"
            case "$FAKE_SMOKE_MODE" in
                create-response-lost|create-response-lost-bad-provenance) exit 1 ;;
            esac
            printf '123\t%s\ttrue\t%s\n' "$smoke_repository" "$smoke_description"
            exit 0
        fi
        if [ ! -f "$FAKE_SMOKE_STATE/repository" ]; then
            case "$FAKE_SMOKE_MODE" in
                preflight-network) printf 'gh: connection reset\n' >&2 ;;
                preflight-permission) printf 'gh: Forbidden (HTTP 403)\n' >&2 ;;
                *) printf 'gh: Not Found (HTTP 404)\n' >&2 ;;
            esac
            exit 1
        fi
        smoke_repository=$(/bin/cat "$FAKE_SMOKE_STATE/repository")
        smoke_description=$(/bin/cat "$FAKE_SMOKE_STATE/description")
        smoke_id=123
        smoke_private=true
        [ "$FAKE_SMOKE_MODE" != bad-id ] || smoke_id=999
        [ "$FAKE_SMOKE_MODE" != bad-private ] || smoke_private=false
        [ "$FAKE_SMOKE_MODE" != bad-full-name ] || smoke_repository="octocat/not-the-created-repository"
        case "$FAKE_SMOKE_MODE" in
            bad-provenance|create-response-lost-bad-provenance)
                smoke_description=work-tracker-live-smoke:unrelated
                ;;
        esac
        printf '%s\t%s\t%s\t%s\n' \
            "$smoke_id" "$smoke_repository" "$smoke_private" "$smoke_description"
        ;;
    repo)
        [ "${2:-}" = delete ] || exit 1
        [ "$FAKE_SMOKE_MODE" != delete-fail ] || exit 1
        ;;
    *) exit 1 ;;
esac
"#,
    )?;
    if invalid_uuid {
        write_executable(
            &bin.join("python3"),
            r#"#!/bin/sh
if [ "${1:-}" = -c ] && printf '%s' "${2:-}" | /bin/grep -q uuid.uuid4; then
    printf 'not-a-uuid\n'
    exit 0
fi
exec /usr/bin/python3 "$@"
"#,
        )?;
    }

    let path = format!("{}:/usr/bin:/bin", bin.display());
    let script = format!(
        "{}/scripts/github-live-smoke.sh",
        env!("CARGO_MANIFEST_DIR")
    );
    let output = Command::new(script)
        .current_dir(&work)
        .env_clear()
        .env("HOME", &home)
        .env("PATH", path)
        .env("WORK_TRACKER_GITHUB_SMOKE", "1")
        .env("FAKE_SMOKE_STATE", &state)
        .env("FAKE_SMOKE_MODE", mode)
        .env("FAKE_SMOKE_BINARY_SOURCE", &fake_binary)
        .env("FAKE_SMOKE_CACHE", work.join("cache.db"))
        .output()?;
    let calls = fs::read_to_string(state.join("calls")).unwrap_or_default();
    Ok((output, calls))
}

fn assert_exact_smoke_create(calls: &str) -> Result<String> {
    let create_calls = calls
        .lines()
        .filter(|call| call.contains("\t--method\tPOST\t"))
        .collect::<Vec<_>>();
    assert_eq!(
        create_calls.len(),
        1,
        "repository must be created exactly once"
    );
    let create = create_calls[0];
    ensure!(create.contains("\tPOST\tuser/repos\t"));
    assert_eq!(create.matches("\t-F\tprivate=true\t").count(), 1);
    ensure!(create.contains("\t-F\thas_wiki=false\t"));
    let name_start = create
        .find("\tname=")
        .map(|index| index + "\tname=".len())
        .context("create request omitted repository name")?;
    let name_end = create[name_start..]
        .find('\t')
        .map(|offset| name_start + offset)
        .context("repository name was not field-delimited")?;
    let name = &create[name_start..name_end];
    let run_id = name
        .strip_prefix("work-tracker-disposable-smoke-")
        .context("repository name omitted disposable prefix")?;
    ensure!(
        run_id.len() > 17,
        "repository name omitted timestamp or nonce"
    );
    let (timestamp, nonce) = run_id.split_at(16);
    ensure!(timestamp.ends_with('Z') && timestamp.as_bytes()[8] == b'T');
    ensure!(
        timestamp
            .bytes()
            .enumerate()
            .all(|(index, byte)| index == 8 || index == 15 || byte.is_ascii_digit())
    );
    let nonce = nonce
        .strip_prefix('-')
        .context("repository name did not delimit its UUID nonce")?;
    let parsed_nonce = uuid::Uuid::parse_str(nonce)?;
    assert_eq!(parsed_nonce.get_version_num(), 4);
    ensure!(create.contains(&format!(
        "\t-f\tdescription=work-tracker-live-smoke:{nonce}\t"
    )));
    let repository = format!("octocat/{name}");
    let expected_preflight = format!("CALL\tapi\trepos/{repository}\t--silent");
    assert_eq!(
        calls
            .lines()
            .filter(|call| *call == expected_preflight)
            .count(),
        1,
        "smoke must preflight the exact generated repository name once"
    );
    Ok(repository)
}

fn assert_exact_smoke_delete(calls: &str, repository: &str, expected_count: usize) {
    let expected_delete = format!("CALL\trepo\tdelete\t{repository}\t--yes");
    assert_eq!(
        calls
            .lines()
            .filter(|call| *call == expected_delete)
            .count(),
        expected_count,
        "cleanup must target exactly the repository created by the smoke"
    );
}

#[test]
fn live_github_smoke_refuses_to_run_without_explicit_opt_in() -> Result<()> {
    let justfile = fs::read_to_string(format!("{}/justfile", env!("CARGO_MANIFEST_DIR")))?;
    assert!(justfile.contains("./scripts/github-live-smoke.sh {{owner}}"));
    assert!(!justfile.contains("WORK_TRACKER_GITHUB_SMOKE=1 ./scripts/github-live-smoke.sh"));

    let directory = tempfile::tempdir()?;
    let gh = directory.path().join("gh");
    let calls = directory.path().join("gh-calls");
    fs::write(
        &gh,
        format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\n", calls.display()),
    )?;
    let mut permissions = fs::metadata(&gh)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&gh, permissions)?;

    let script = format!(
        "{}/scripts/github-live-smoke.sh",
        env!("CARGO_MANIFEST_DIR")
    );
    let output = Command::new(&script)
        .env_clear()
        .env("PATH", directory.path())
        .output()
        .with_context(|| format!("failed to invoke {script}"))?;

    ensure!(!output.status.success(), "unguarded smoke test succeeded");
    ensure!(
        String::from_utf8(output.stderr)?.contains("WORK_TRACKER_GITHUB_SMOKE=1"),
        "refusal did not explain the opt-in gate"
    );
    ensure!(!calls.exists(), "smoke test invoked gh before opt-in");
    Ok(())
}

#[test]
fn live_github_smoke_enforces_nonce_identity_privacy_and_cleanup() -> Result<()> {
    let (valid, calls) = run_fake_live_smoke("valid", false)?;
    ensure!(
        valid.status.success(),
        "valid smoke failed: {}",
        String::from_utf8_lossy(&valid.stderr)
    );
    let repository = assert_exact_smoke_create(&calls)?;
    assert_exact_smoke_delete(&calls, &repository, 1);

    let (invalid_nonce, calls) = run_fake_live_smoke("valid", true)?;
    ensure!(!invalid_nonce.status.success());
    ensure!(!calls.contains("\t--method\tPOST\t"));
    ensure!(!calls.contains("\trepo\tdelete\t"));

    for mode in ["bad-id", "bad-private", "bad-full-name", "bad-provenance"] {
        let (guarded, calls) = run_fake_live_smoke(mode, false)?;
        ensure!(
            !guarded.status.success(),
            "{mode} cleanup unexpectedly passed"
        );
        let repository = assert_exact_smoke_create(&calls)?;
        assert_exact_smoke_delete(&calls, &repository, 0);
    }

    for mode in ["preflight-network", "preflight-permission"] {
        let (preflight_failed, calls) = run_fake_live_smoke(mode, false)?;
        ensure!(!preflight_failed.status.success());
        ensure!(!calls.contains("\t--method\tPOST\t"));
        ensure!(!calls.contains("\trepo\tdelete\t"));
    }

    let (delete_failed, calls) = run_fake_live_smoke("delete-fail", false)?;
    ensure!(!delete_failed.status.success());
    let repository = assert_exact_smoke_create(&calls)?;
    assert_exact_smoke_delete(&calls, &repository, 1);

    let (ambiguous_unrelated, calls) =
        run_fake_live_smoke("create-response-lost-bad-provenance", false)?;
    ensure!(!ambiguous_unrelated.status.success());
    let repository = assert_exact_smoke_create(&calls)?;
    assert_exact_smoke_delete(&calls, &repository, 0);
    ensure!(String::from_utf8_lossy(&delete_failed.stderr).contains("failed to delete"));

    let (ambiguous_create, calls) = run_fake_live_smoke("create-response-lost", false)?;
    ensure!(!ambiguous_create.status.success());
    let repository = assert_exact_smoke_create(&calls)?;
    assert_exact_smoke_delete(&calls, &repository, 1);

    for (mode, expected_code) in [("signal-int", 130), ("signal-term", 143)] {
        let (interrupted, calls) = run_fake_live_smoke(mode, false)?;
        assert_eq!(interrupted.status.code(), Some(expected_code), "{mode}");
        let repository = assert_exact_smoke_create(&calls)?;
        assert_exact_smoke_delete(&calls, &repository, 1);
    }
    Ok(())
}

#[test]
fn readme_covers_the_github_user_and_operator_runbook() {
    let readme = include_str!("../README.md");
    for required in [
        "gh auth login",
        "gh auth status",
        "--fresh",
        "--offline",
        "Rejected Mutation",
        "recover 41 --mode restore-exact-copy",
        "recover 41 --mode rebaseline",
        "--database /path/to/legacy.db",
        "WORK_TRACKER_GITHUB_SMOKE=1 just smoke-github",
        "disposable private repository",
    ] {
        assert!(
            readme.contains(required),
            "README omitted required operator guidance: {required}"
        );
    }
}

#[test]
fn qualification_matrix_tracks_every_parent_and_release_acceptance_statement() {
    let matrix = include_str!("../docs/release-qualification.md");
    for statement in 1..=60 {
        let identifier = format!("| P-{statement:02} |");
        assert!(
            matrix.contains(&identifier),
            "qualification matrix omitted parent #1 statement {statement}"
        );
    }
    for statement in 1..=9 {
        let identifier = format!("| Q-{statement:02} |");
        assert!(
            matrix.contains(&identifier),
            "qualification matrix omitted ticket #15 criterion {statement}"
        );
    }
}

#[test]
fn compiled_cli_command_and_exit_contract_is_stable() -> Result<()> {
    let cli = CliHarness::new()?;

    let help = cli.run(["--help"])?;
    assert_success(&help)?;
    assert_eq!(stderr(&help)?, "");
    let help_text = stdout(&help)?;
    for command in [
        "init", "add", "show", "list", "today", "update", "status", "note", "archive", "history",
        "rejected", "doctor", "recover", "path", "serve",
    ] {
        assert!(help_text.contains(command), "help omitted {command}");
    }

    let success = cli.run(["--json", "path"])?;
    assert_eq!(success.status.code(), Some(0));
    assert_eq!(stderr(&success)?, "");
    let value: Value = serde_json::from_slice(&success.stdout)?;
    ensure!(value["database"].is_string());

    let runtime_error = cli.run(["--json", "show", "999"])?;
    assert_eq!(runtime_error.status.code(), Some(1));
    assert_eq!(stdout(&runtime_error)?, "");
    let value: Value = serde_json::from_slice(&runtime_error.stderr)?;
    ensure!(value["error"].is_string());

    let validation_error = cli.run(["--json", "list", "--all", "--status", "done"])?;
    assert_eq!(validation_error.status.code(), Some(2));
    assert_eq!(stdout(&validation_error)?, "");
    let value: Value = serde_json::from_slice(&validation_error.stderr)?;
    assert_eq!(value["error"]["code"], "cli_validation_failed");
    ensure!(
        value["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("cannot be used with"))
    );

    let human_validation = cli.run(["list", "--all", "--status", "done"])?;
    assert_eq!(human_validation.status.code(), Some(2));
    ensure!(stderr(&human_validation)?.starts_with("error:"));
    Ok(())
}
