use std::{
    io::{Read as _, Write as _},
    net::{TcpListener, TcpStream},
    path::Path,
    process::{Child, Command, Output, Stdio},
    sync::{Arc, Barrier},
    time::Duration,
};

use rusqlite::Connection;
use serde_json::{Value, json};
use tempfile::TempDir;
use work_tracker::{
    db::{ListFilter, SCHEMA_VERSION, Tracker, backup},
    domain::Status,
};

fn run(database: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_work-tracker"))
        .arg("--database")
        .arg(database)
        .args(args)
        .env("NO_COLOR", "1")
        .env("WORK_TRACKER_ACTOR", "dep-test")
        .output()
        .unwrap()
}

fn text(database: &Path, args: &[&str]) -> String {
    let output = run(database, args);
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

fn json_out(database: &Path, args: &[&str]) -> Value {
    serde_json::from_str(&text(database, &[args, &["--json"]].concat())).unwrap()
}

fn add(database: &Path, title: &str, extra: &[&str]) -> i64 {
    let mut args = vec!["add", title];
    args.extend_from_slice(extra);
    json_out(database, &args)["id"].as_i64().unwrap()
}

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn http_request(address: &str, method: &str, path: &str) -> String {
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

#[test]
fn schema_migration_from_v1_preserves_data_and_creates_dependencies_table() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("legacy.db");
    let conn = Connection::open(&database).unwrap();
    conn.execute_batch(include_str!("fixtures/schema_v1.sql"))
        .unwrap();
    conn.execute(
        "INSERT INTO work_items (id, title, status, created_at, updated_at)
         VALUES (42, 'Legacy item', 'pending', '2026-10-01T00:00:00.000Z', '2026-10-01T00:00:00.000Z')",
        [],
    )
    .unwrap();
    drop(conn);

    let tracker = Tracker::open(&database).unwrap();
    assert_eq!(tracker.get(42).unwrap().title, "Legacy item");
    assert!(tracker.dependencies(42).unwrap().blocked_by.is_empty());

    let conn = Connection::open(&database).unwrap();
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    let table_exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='work_dependencies')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(table_exists);
}

#[test]
fn add_with_blocked_by_handles_unresolved_done_deleted_and_finished_targets() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("tracker.db");

    let c = add(&database, "Blocker C", &["--status", "active"]);
    let d = add(&database, "Blocker D", &["--status", "done"]);
    let del = add(&database, "Deleted blocker", &[]);
    json_out(&database, &["delete", &del.to_string()]);

    // Unresolved blocker C forces pending -> blocked.
    let a = add(&database, "Work A", &["--blocked-by", &format!("{c},{d}")]);
    let a_item = json_out(&database, &["show", &a.to_string()]);
    assert_eq!(a_item["status"], "blocked");
    let a_deps = json_out(&database, &["dep", "list", &a.to_string()]);
    assert_eq!(a_deps["ready"], false);
    assert_eq!(a_deps["unresolved_blockers"], 1);
    assert_eq!(a_deps["blocked_by"].as_array().unwrap().len(), 2);

    // Only done blocker D leaves target pending and ready.
    let b = add(&database, "Work B", &["--blocked-by", &d.to_string()]);
    assert_eq!(
        json_out(&database, &["show", &b.to_string()])["status"],
        "pending"
    );
    assert_eq!(
        json_out(&database, &["dep", "ls", &b.to_string()])["ready"],
        true
    );

    // Deleted blocker is rejected.
    let err_del = run(&database, &["add", "Bad", "--blocked-by", &del.to_string()]);
    assert!(!err_del.status.success());

    // Creating a done/cancelled item with blockers is rejected.
    for status in ["done", "cancelled"] {
        let err = run(
            &database,
            &[
                "add",
                "Bad",
                "--status",
                status,
                "--blocked-by",
                &c.to_string(),
            ],
        );
        assert!(!err.status.success(), "{status}");
    }
}

#[test]
fn dep_add_transitions_to_blocked_is_idempotent_and_rejects_cycles() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("tracker.db");

    let a = add(&database, "Task A", &["--status", "active"]);
    let b = add(&database, "Task B", &["--status", "waiting"]);
    let c = add(&database, "Task C", &["--status", "pending"]);

    // A blocked by B
    let res = json_out(
        &database,
        &[
            "dep",
            "add",
            &a.to_string(),
            "--blocked-by",
            &b.to_string(),
            "--note",
            "needs B first",
        ],
    );
    assert_eq!(res["action"], "added");
    assert_eq!(res["changed_blockers"], json!([b]));
    assert_eq!(
        res["status_changed"],
        json!({"from": "active", "to": "blocked"})
    );
    assert_eq!(res["item"]["status"], "blocked");

    // Repeated add is idempotent and appends no history.
    let before_len = json_out(&database, &["history", &a.to_string()])
        .as_array()
        .unwrap()
        .len();
    let repeat = json_out(
        &database,
        &["dep", "add", &a.to_string(), "--blocked-by", &b.to_string()],
    );
    assert_eq!(repeat["changed_blockers"], json!([]));
    assert!(repeat["status_changed"].is_null());
    let after_len = json_out(&database, &["history", &a.to_string()])
        .as_array()
        .unwrap()
        .len();
    assert_eq!(before_len, after_len);

    // Self-dependency rejected.
    assert!(
        !run(
            &database,
            &["dep", "add", &a.to_string(), "--blocked-by", &a.to_string()]
        )
        .status
        .success()
    );

    // 2-cycle rejected: B blocked by A.
    assert!(
        !run(
            &database,
            &["dep", "add", &b.to_string(), "--blocked-by", &a.to_string()]
        )
        .status
        .success()
    );

    // B blocked by C succeeds, then 3-cycle C blocked by A rejected.
    assert!(
        run(
            &database,
            &["dep", "add", &b.to_string(), "--blocked-by", &c.to_string()]
        )
        .status
        .success()
    );
    assert!(
        !run(
            &database,
            &["dep", "add", &c.to_string(), "--blocked-by", &a.to_string()]
        )
        .status
        .success()
    );
}

#[test]
fn concurrent_cycle_creation_allows_only_one_direction() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("concurrent.db");
    let mut tracker = Tracker::open(&database).unwrap();
    let a = tracker
        .create("Item A", None, Status::Pending, "tester", None)
        .unwrap()
        .id;
    let b = tracker
        .create("Item B", None, Status::Pending, "tester", None)
        .unwrap()
        .id;
    drop(tracker);

    let barrier = Arc::new(Barrier::new(2));
    let db1 = database.clone();
    let b1 = Arc::clone(&barrier);
    let h1 = std::thread::spawn(move || {
        let mut t = Tracker::open(&db1).unwrap();
        b1.wait();
        t.add_dependencies(a, &[b], "worker-1", None).is_ok()
    });
    let db2 = database.clone();
    let b2 = Arc::clone(&barrier);
    let h2 = std::thread::spawn(move || {
        let mut t = Tracker::open(&db2).unwrap();
        b2.wait();
        t.add_dependencies(b, &[a], "worker-2", None).is_ok()
    });
    let ok1 = h1.join().unwrap();
    let ok2 = h2.join().unwrap();
    assert_ne!(ok1, ok2, "exactly one direction must succeed");
}

#[test]
fn diamond_unblocks_all_dependents_only_when_last_blocker_is_done() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("diamond.db");

    let c = add(&database, "Blocker C", &["--status", "active"]);
    let d = add(&database, "Blocker D", &["--status", "active"]);
    let a = add(
        &database,
        "Task A",
        &[
            "--blocked-by",
            &c.to_string(),
            "--blocked-by",
            &d.to_string(),
        ],
    );
    let b = add(&database, "Task B", &["--blocked-by", &format!("{c},{d}")]);

    // Completing C keeps A and B blocked because D is still active.
    let c_done_text = text(&database, &["status", &c.to_string(), "done"]);
    assert!(!c_done_text.contains("Unblocked:"));
    assert_eq!(
        json_out(&database, &["show", &a.to_string()])["status"],
        "blocked"
    );
    assert_eq!(
        json_out(&database, &["show", &b.to_string()])["status"],
        "blocked"
    );

    // Completing D unblocks both A and B to pending and prints Unblocked section.
    let d_done_text = text(&database, &["status", &d.to_string(), "done"]);
    assert!(d_done_text.contains("Unblocked:"), "{d_done_text}");
    assert!(
        d_done_text.contains(&format!("#{a} [pending] Task A")),
        "{d_done_text}"
    );
    assert!(
        d_done_text.contains(&format!("#{b} [pending] Task B")),
        "{d_done_text}"
    );

    for id in [a, b] {
        assert_eq!(
            json_out(&database, &["show", &id.to_string()])["status"],
            "pending"
        );
        let history = json_out(&database, &["history", &id.to_string()]);
        let last = history.as_array().unwrap().last().unwrap();
        assert_eq!(last["kind"], "unblocked");
        assert_eq!(last["note"], format!("Unblocked: blocker #{d} done"));
        assert_eq!(
            last["changes"],
            json!({
                "status": {"from": "blocked", "to": "pending"},
                "reason": "dependencies_resolved",
                "resolved_by": d,
            })
        );
    }
}

#[test]
fn cancelled_and_deleted_blockers_do_not_unblock_until_dep_removed() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("cancel.db");

    let c = add(&database, "Blocker C", &["--status", "active"]);
    let d = add(&database, "Blocker D", &["--status", "active"]);
    let a = add(&database, "Task A", &["--blocked-by", &c.to_string()]);
    let b = add(&database, "Task B", &["--blocked-by", &d.to_string()]);

    let cancel_out = text(&database, &["status", &c.to_string(), "cancelled"]);
    assert!(
        cancel_out.contains(&format!("Still blocked by #{c}:")),
        "{cancel_out}"
    );
    assert!(
        cancel_out.contains(&format!("#{a} [blocked] Task A")),
        "{cancel_out}"
    );
    assert_eq!(
        json_out(&database, &["show", &a.to_string()])["status"],
        "blocked"
    );

    let del_out = text(&database, &["delete", &d.to_string()]);
    assert!(
        del_out.contains(&format!("Still blocked by #{d}:")),
        "{del_out}"
    );
    assert_eq!(
        json_out(&database, &["show", &b.to_string()])["status"],
        "blocked"
    );

    // Removing the cancelled blocker auto-unblocks A with structured history.
    let rm_out = json_out(
        &database,
        &["dep", "rm", &a.to_string(), "--blocked-by", &c.to_string()],
    );
    assert_eq!(rm_out["action"], "removed");
    assert_eq!(rm_out["changed_blockers"], json!([c]));
    assert_eq!(
        rm_out["status_changed"],
        json!({"from": "blocked", "to": "pending"})
    );
    assert_eq!(rm_out["item"]["status"], "pending");
    let history = json_out(&database, &["history", &a.to_string()]);
    let last = history.as_array().unwrap().last().unwrap();
    assert_eq!(last["kind"], "unblocked");
    assert_eq!(last["changes"]["removed_blockers"], json!([c]));
}

#[test]
fn manual_blocked_without_unresolved_deps_is_not_unblocked_when_done_dep_removed() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("manual.db");

    let done_blocker = add(&database, "Already done", &["--status", "done"]);
    let target = add(
        &database,
        "Manual block",
        &["--blocked-by", &done_blocker.to_string()],
    );
    json_out(
        &database,
        &[
            "status",
            &target.to_string(),
            "blocked",
            "--note",
            "manual hold",
        ],
    );

    let rm = json_out(
        &database,
        &[
            "dep",
            "remove",
            &target.to_string(),
            "--blocked-by",
            &done_blocker.to_string(),
        ],
    );
    assert!(rm["status_changed"].is_null());
    assert_eq!(rm["item"]["status"], "blocked");
}

#[test]
fn reopening_done_blocker_does_not_reblock_but_excludes_from_ready() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("reopen.db");

    let c = add(&database, "Blocker C", &["--status", "active"]);
    let a = add(&database, "Task A", &["--blocked-by", &c.to_string()]);
    json_out(&database, &["status", &c.to_string(), "done"]);
    assert_eq!(
        json_out(&database, &["show", &a.to_string()])["status"],
        "pending"
    );
    assert_eq!(
        json_out(&database, &["dep", "list", &a.to_string()])["ready"],
        true
    );

    // Reopen C -> A stays pending (no silent status overwrite), but is no longer ready.
    json_out(&database, &["status", &c.to_string(), "active"]);
    assert_eq!(
        json_out(&database, &["show", &a.to_string()])["status"],
        "pending"
    );
    let deps = json_out(&database, &["dep", "list", &a.to_string()]);
    assert_eq!(deps["ready"], false);
    assert_eq!(deps["unresolved_blockers"], 1);

    let ready = json_out(&database, &["list", "--ready"]);
    let ready_ids: Vec<i64> = ready
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ready_ids, vec![c]);
}

#[test]
fn list_ready_and_show_preserve_json_contracts_while_human_outputs_show_deps() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("views.db");

    let blocker = add(&database, "Blocker", &["--status", "active"]);
    let waiting = add(&database, "Waiting work", &["--status", "waiting"]);
    let blocked = add(
        &database,
        "Blocked work",
        &["--blocked-by", &blocker.to_string()],
    );
    let plain_pending = add(&database, "Plain pending", &[]);

    let ready_json = json_out(&database, &["list", "--ready"]);
    let ready_ids: Vec<i64> = ready_json
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ready_ids, vec![blocker, plain_pending]);

    let ready_human = text(&database, &["list", "--ready"]);
    assert!(ready_human.contains("Showing 2 ready work items."));
    assert!(!ready_human.contains("Waiting work"));
    assert!(!ready_human.contains("Blocked work"));
    let _ = waiting;

    // --ready conflicts with --all and --status
    assert!(
        !run(&database, &["list", "--ready", "--all"])
            .status
            .success()
    );
    assert!(
        !run(&database, &["list", "--ready", "--status", "pending"])
            .status
            .success()
    );

    // show --json keeps exact work_items columns (no extra keys).
    let shown_json = json_out(&database, &["show", &blocked.to_string()]);
    assert!(shown_json.get("blocked_by").is_none());

    // list --compact --json keeps exact 8 keys.
    let compact = json_out(&database, &["list", "--ready", "--compact"]);
    assert_eq!(compact[0].as_object().unwrap().len(), 8);

    // Human show and list --details both include Blocked by / Blocks when present.
    let show_blocked = text(&database, &["show", &blocked.to_string()]);
    let show_blocker = text(&database, &["show", &blocker.to_string()]);
    assert!(show_blocked.contains(&format!("Blocked by:  #{blocker} [active] Blocker\n")));
    assert!(show_blocker.contains(&format!("Blocks:      #{blocked} [blocked] Blocked work\n")));
    let details = text(&database, &["list", "--details"]);
    assert!(details.contains(&show_blocked));
    assert!(details.contains(&show_blocker));
}

#[test]
fn brief_and_web_dashboard_display_dependencies_and_stay_read_only() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("web.db");
    let project = temp.path().join("proj");
    std::fs::create_dir(&project).unwrap();
    let wd = project.to_str().unwrap();

    let c = add(
        &database,
        "Upstream <C>",
        &["--status", "active", "--workdir", wd],
    );
    let a = add(
        &database,
        "Downstream <A>",
        &["--workdir", wd, "--blocked-by", &c.to_string()],
    );

    let brief = text(&database, &["brief", "--workdir", wd]);
    assert!(
        brief.contains(&format!("Blocked by: #{c} [active] Upstream <C>")),
        "{brief}"
    );

    // Completing C unblocks A and brief surfaces the unblock note without Blocked by.
    text(&database, &["status", &c.to_string(), "done"]);
    let brief_after = text(&database, &["brief", "--workdir", wd]);
    assert!(
        brief_after.contains(&format!("#{a} [pending] Downstream <A>")),
        "{brief_after}"
    );
    assert!(
        brief_after.contains(&format!("Latest note: Unblocked: blocker #{c} done")),
        "{brief_after}"
    );
    assert!(!brief_after.contains("Blocked by:"), "{brief_after}");

    // Re-add dependency via dep add so the dashboard checks see both blocked_by and blocks.
    let c2 = add(
        &database,
        "Upstream <C2>",
        &["--status", "active", "--workdir", wd],
    );
    text(
        &database,
        &[
            "dep",
            "add",
            &a.to_string(),
            "--blocked-by",
            &c2.to_string(),
        ],
    );

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    drop(listener);
    let mut server = Server(
        Command::new(env!("CARGO_BIN_EXE_work-tracker"))
            .arg("--database")
            .arg(&database)
            .args(["serve", "--bind", &address])
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    for _ in 0..200 {
        if TcpStream::connect(&address).is_ok() {
            break;
        }
        assert!(server.0.try_wait().unwrap().is_none());
        std::thread::sleep(Duration::from_millis(10));
    }

    let index_html = http_request(&address, "GET", "/");
    assert!(index_html.starts_with("HTTP/1.1 200"));
    assert!(index_html.contains("blocked by 1"), "{index_html}");

    let a_html = http_request(&address, "GET", &format!("/items/{a}"));
    assert!(a_html.starts_with("HTTP/1.1 200"));
    assert!(a_html.contains("Blocked by"));
    assert!(a_html.contains(&format!("href=\"/items/{c2}\"")));
    assert!(a_html.contains("Upstream &lt;C2&gt;"));

    let c2_html = http_request(&address, "GET", &format!("/items/{c2}"));
    assert!(c2_html.contains("Blocks"));
    assert!(c2_html.contains(&format!("href=\"/items/{a}\"")));
    assert!(http_request(&address, "POST", "/").starts_with("HTTP/1.1 405"));
}

#[test]
fn backup_restore_and_purge_preserve_dependencies_and_foreign_keys() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join("ledger.db");
    let mut tracker = Tracker::open(&database).unwrap();
    let c = tracker
        .create("Upstream", None, Status::Active, "tester", None)
        .unwrap()
        .id;
    let a = tracker
        .create_item_with_blockers(
            work_tracker::db::NewWorkItem {
                title: "Downstream",
                description: None,
                status: Status::Pending,
                schedule: Default::default(),
                workdir: None,
            },
            &[c],
            "tester",
            None,
        )
        .unwrap()
        .id;

    let snapshot = backup::create(&database, &temp.path().join("backups")).unwrap();
    let restored = temp.path().join("restored.db");
    backup::restore(&snapshot.path, &restored).unwrap();

    let restored_tracker = Tracker::open(&restored).unwrap();
    let deps = restored_tracker.dependencies(a).unwrap();
    assert_eq!(deps.blocked_by.len(), 1);
    assert_eq!(deps.blocked_by[0].id, c);

    // Deleting and purging C cascades its row in work_dependencies cleanly.
    let deleted = tracker
        .set_status(c, Status::Deleted, "tester", None)
        .unwrap();
    let future = deleted.purge_after.unwrap() + chrono::Duration::seconds(1);
    assert_eq!(tracker.purge_expired(future).unwrap(), 1);
    assert!(tracker.dependencies(a).unwrap().blocked_by.is_empty());
    assert_eq!(
        tracker
            .list(ListFilter::Actionable, false, 10)
            .unwrap()
            .len(),
        1
    );
}
