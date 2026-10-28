# Work Tracker

Work Tracker is a small, agent-friendly status ledger for parallel and long-running work. A Rust CLI and read-only HTML dashboard share one SQLite database, so a human or agent can recover the current state and the context that led there.

## What it provides

- Transaction-safe updates from concurrent agents using SQLite WAL mode.
- Stable human-readable commands and machine-readable `--json` output.
- Immutable creation, note, update, status-transition, and deletion history.
- A default `list` that shows only actionable work, with `--all` for the full ledger.
- Automatic Work Directory capture, explicit overrides, and exact directory filters.
- A Daily View containing everything updated today and every actionable item.
- A Todo View for date-based commitments, with carryover, deadlines, and explicit priorities.
- Soft deletion with a 60-day readable retention window and automatic purge.
- A localhost-only, read-only dashboard suitable for SSH tunneling.
- Verified manual SQLite snapshots, optional daily backups, and recovery to a saved snapshot.

## Build and install

Rust and [`just`](https://github.com/casey/just) are required.

```bash
just build
just test
just install
```

`just install` installs `work-tracker` through Cargo for the current user. Other workflows:

```bash
just                 # list workflows
just check           # format check, Clippy, and tests
just release         # optimized binary in target/release/
just uninstall
```

## Quick start

The database is created automatically at `$XDG_DATA_HOME/work-tracker/work-tracker.db`, or at `~/.local/share/work-tracker/work-tracker.db` when `XDG_DATA_HOME` is unset.

```bash
work-tracker add "Watch company CI" \
  --description "SQL and medium suites are queued" \
  --status waiting \
  --actor codex-ci \
  --note "submitted at PR head abc123"

work-tracker list
work-tracker today
work-tracker show 1
work-tracker note 1 "Runner assigned; results expected in 30 minutes" --actor codex-ci
work-tracker status 1 active --actor codex-ci --note "analyzing failures"
work-tracker history 1
work-tracker status 1 done --actor codex-ci --note "all required checks passed"
```

Use `WORK_TRACKER_ACTOR` to avoid repeating `--actor`:

```bash
export WORK_TRACKER_ACTOR=codex-ci
work-tracker status 1 waiting --note "queued behind 12 builds"
```

If neither is set, the CLI uses the current `USER`, then `unknown` as a last resort.

## Commands

| Command | Purpose |
|---|---|
| `add` | Create a Work Item and capture its Work Directory; initially `pending` unless selected otherwise |
| `show ID` | Show one item, including a soft-deleted item |
| `list` | List actionable items; `--all` adds done and cancelled, `--status` selects one status |
| `today` | Show items updated today plus all actionable items |
| `todo [today]` / `todo --days N` | Show commitments through N calendar days, including today |
| `update ID` | Change title, description, planned/due dates, priority, or Work Directory |
| `status ID STATUS` | Apply an idempotent status transition |
| `note ID MESSAGE` | Preserve context without changing status |
| `delete ID` | Soft-delete an item for 60 days |
| `history ID` | Show the complete immutable history |
| `path` | Show the SQLite database path |
| `backup` | Save a complete, verified SQLite snapshot |
| `backups` | List and verify snapshots, newest first |
| `restore SNAPSHOT --to PATH` | Recover a snapshot into a new database |
| `serve` | Host the read-only dashboard |

The statuses are `pending`, `active`, `waiting`, `blocked`, `done`, `cancelled`, and `deleted`. The first four are actionable: they are what `list` shows by default, and they remain in the Daily View even when they were not updated today.

Run `work-tracker COMMAND --help` for all options.

## Listing work

`list` answers "what is still open" and `today` answers "what happened today":

```bash
work-tracker list                 # actionable items only
work-tracker list --all           # every item, including done and cancelled
work-tracker list --status done   # exactly one status
work-tracker list --details       # show-style details for each selected item
work-tracker today                # actionable items plus anything updated today
```

Both views order work by attention first: blocked, active, waiting, pending, then finished work, most recently updated first within each group. `--all` and `--status` cannot be combined. The human-readable `list` output ends with a footer that names `--all` whenever finished items are hidden; `--json` output is always a plain array. Deleted items stay hidden from every list unless you pass `--include-deleted` or `--status deleted`.

`list --details` prints each selected Work Item using the same formatter as
`show ID`, with a blank line between items. Existing filters, ordering, limits,
and the actionable footer still apply. `--details --json` produces the same JSON
as `--json` alone; plain `list` keeps its compact table.

`show` and `list --details` display every column in SQLite's `work_items` table,
including `deleted_at` and unset fields (`-`; an unset Work Directory is
`unknown`). Detailed timestamps preserve fractional seconds and show the local
UTC offset. History Entries remain available through `history ID`.

Before `show`, `list --details`, or `list --json` prints anything, a runtime check
compares the live SQLite columns with the detail fields. Each returned item's
serialized fields must also match. Missing or unknown fields cause exit status
1 with a diagnostic on standard error, rather than silently returning incomplete
details. The schema check runs even when the list is empty. JSON field names,
null values, and the list array shape are preserved.

## Work Directory

`add` automatically records the directory where the command runs. An agent
running the command elsewhere can supply its project or worktree explicitly:

```bash
work-tracker add "Investigate failure"
work-tracker add "Watch CI" --workdir /path/to/project-worktree
work-tracker list --here --json
work-tracker list --workdir /path/to/project-worktree --json
work-tracker update 1 --workdir /path/to/another-worktree --note "Work moved"
work-tracker list --without-workdir
work-tracker update 1 --clear-workdir
```

Assignments require an existing directory. Paths are stored as absolute physical
paths: relative arguments resolve from the command's current directory and
symlink aliases share an association. Significant spaces are preserved; paths
must be valid UTF-8. The CLI records its actual working directory, including a
subdirectory, without discovering a Git root.

Plain `list` stays global. `--here`, `--workdir PATH`, and `--without-workdir` are
mutually exclusive directory selectors; combine any one with existing status,
deletion, and limit options. Matching is exact, so a parent directory does not
include its descendants. Directory selection happens before the limit. Daily
and Todo Views retain their global selection rules.

Existing items start with an unknown directory and remain visible globally.
Assign them with `update --workdir`, or clear an association with
`--clear-workdir`. Effective changes record the old and new value in history;
repeating the same assignment or clear adds no history. Creation records its
directory in the creation entry. Deleted items remain immutable.

Removing a worktree preserves its saved association. `list --workdir` accepts
the saved absolute path even after removal; for missing paths, it resolves an
existing ancestor and normalizes the remaining components. Use the saved
physical path when a symlink alias has also been removed. Directory reads never
move or rewrite an association.

`show` and the dashboard display Work Directory. Work Item JSON adds the stable
`workdir` field, a string or `null`, across commands and views; compact human
list/Today/Todo columns retain their existing layout.

The directory migration upgrades SQLite's schema version to 3 while preserving
existing fields and history. Version-2 binaries reject an upgraded ledger;
all clients of that ledger need the new binary. Older field-specific writes
that permit access preserve the additional column.

## Terminal colors

Human-readable Todo, list, today, and item output uses color automatically when
stdout is a terminal. Headers are bold; active/done statuses are green, waiting
is yellow, blocked and overdue dates/labels are red, high Priority is bold
magenta, and carryover labels are yellow. Timestamps and secondary text are dim.
Labels remain readable without color.

Pipes and redirected output stay plain. A nonempty `NO_COLOR` or `TERM=dumb`
disables automatic color. Use the global `--color auto|always|never` option to
override detection; an explicit `always` overrides those environment settings.
JSON output is always uncolored, including when `--color always` is given.

```bash
work-tracker todo --color never
work-tracker list --color always | less -R
```

The companion Todo shortcuts also accept `--color` once their separately
managed wrapper is updated.

## Planning with Todo

```bash
work-tracker add "Prepare benchmark presentation" \
  --planned 2026-10-06 --due 2026-10-07 --priority high
work-tracker todo today
work-tracker todo --days 3
work-tracker todo --days 5 --json
work-tracker todo today --all
work-tracker update 1 --planned 2026-10-08 --note "Work rescheduled; deadline unchanged"
work-tracker update 1 --clear-planned --clear-due --priority normal
```

`todo` alone means today. An N-day horizon includes today and the following N−1 **calendar days**, including weekends and holidays, using Asia/Seoul (KST, UTC+09:00), independent of the server timezone. Dates use `YYYY-MM-DD` with years 0001–9999. Days must be positive and the end date representable. `today` and `--days` cannot be combined.

A pending, active, waiting, or blocked item appears when **either** its Planned Date or Due Date is on or before the window's end. Unfinished past plans carry forward; missed deadlines remain overdue. Each item appears once. Items without either date stay in the ordinary backlog even when high priority. `--all` also includes done and cancelled items when either date falls within the window, in a separate Done / cancelled section. Finished items do not carry forward or receive overdue markers. Deleted items remain excluded.

Actions (pending/active) and Blocked / waiting are separate sections. Within each section, sort by high/normal/low Priority, earliest Due Date, earliest Planned Date, then ID; missing dates sort last. Due today is not overdue. A Planned Date after a Due Date is allowed and does not hide the missed deadline. Reading never reschedules work.

Dates are optional and independent; normal is the default Priority. Omitted update flags preserve existing values; `--clear-planned` and `--clear-due` remove dates explicitly. Effective changes include field-level history atomically, and repeated identical updates do not add history. `show` prints scheduling fields. Existing `list` and `today` semantics stay unchanged.

Todo reads the local ledger without contacting GitHub or another service. UPDATED means ledger activity, not a live external check. Empty views succeed with an explanatory message. `--json` returns an object with `window` (`start`, `end`, `days`, `timezone`), `actions`, `blocked_waiting`, and `finished` (empty without `--all`). Each row contains the Work Item fields plus `overdue` and `carried_over`; scheduling fields are `planned_date`, `due_date`, and `priority`. Existing Work Item JSON fields and list/today array shapes are preserved with additive scheduling fields.

The companion chezmoi shortcuts `todo-today`, `todo-3days`, and `todo-5days` are symlinks to a `work-todo` wrapper, dispatching by invocation name. The wrapper includes `--all` by default, including for direct `work-todo` invocation. Shortcuts forward `--json`, `--database`, and `--color`, preserve command exit status, and reject horizon overrides. They are installed separately from this Rust binary; the updated wrapper requires a binary supporting `todo --all`.

### Database upgrade

Opening an existing ledger adds nullable dates and normal Priority without changing its items, statuses, or history. Migration checks actual columns under a transaction because legacy binaries reset SQLite's version marker. Old field-specific writes preserve the additional columns. Newer unknown schema versions are rejected. Keep a SQLite-consistent backup before upgrading a shared live ledger; ordinary retention housekeeping still applies on open.

## Back up and recover your archive

Save a snapshot manually and list the available recovery points:

```bash
work-tracker backup
work-tracker backups
work-tracker backups --json
```

By default, snapshots go in `backups/` beside the selected database. For the
default ledger, this is `~/.local/share/work-tracker/backups/`. Each file has a
UTC timestamp and a unique suffix. Use `--directory PATH` or
`WORK_TRACKER_BACKUP_DIR` to choose another directory for `backup` and `backups`.
Use separate directories for separate ledgers.

Each snapshot contains every Work Item, Status, field, and History Entry present
at one consistent point during the backup. This includes done, cancelled, and
deleted work still stored in SQLite. Backup captures committed WAL data while
other agents continue working. It does not migrate the source database, purge
expired deletions, or add History Entries. Missing source databases fail instead
of creating an empty ledger.

The command verifies SQLite integrity, foreign keys, and the Work Tracker schema
before publishing a completed snapshot. Incomplete files use `.partial` and are
excluded from `backups`. Concurrent backups produce separate files. On Unix,
new backup directories have mode `0700`, and snapshots have mode `0600`.
Existing directories keep their permissions. Snapshots are never automatically
deleted, so monitor free disk space. If a backup fails, earlier snapshots remain
available. `backups` reports a corrupt recognized snapshot as an error with its
path rather than silently listing it as usable.

### Enable daily backups on Linux

Install the new binary with `just install`. The supplied systemd user units use
`~/.cargo/bin/work-tracker`, the default database, and the default backup directory.
If you use `WORK_TRACKER_DB`, `XDG_DATA_HOME`, a custom Cargo installation, or a
different backup directory, edit `ExecStart` in the copied service before starting
it. The service does not inherit your shell environment. Quote paths containing
spaces using systemd unit syntax.

From the repository directory:

```bash
install -Dm644 systemd/work-tracker-backup.service ~/.config/systemd/user/work-tracker-backup.service
install -Dm644 systemd/work-tracker-backup.timer ~/.config/systemd/user/work-tracker-backup.timer
# Edit ~/.config/systemd/user/work-tracker-backup.service here if needed.
systemctl --user daemon-reload
systemctl --user start work-tracker-backup.service
systemctl --user enable --now work-tracker-backup.timer
systemctl --user list-timers work-tracker-backup.timer
```

The timer saves one snapshot at 03:00 in the server's local timezone each day.
`Persistent=true` triggers a missed run when the user manager starts again.
For backups while logged out, enable lingering for your user if the server's
policy permits it:

```bash
loginctl enable-linger "$USER"
```

Check the latest result and any failures:

```bash
systemctl --user status work-tracker-backup.service
journalctl --user -u work-tracker-backup.service -n 30 --no-pager
work-tracker backups
```

To stop future daily backups while keeping snapshots:

```bash
systemctl --user disable --now work-tracker-backup.timer
```

### Resume from a snapshot

Choose a file from `work-tracker backups`. Recover it into a new path:

```bash
work-tracker restore /path/to/snapshot.db --to ~/.local/share/work-tracker/recovered.db
work-tracker --database ~/.local/share/work-tracker/recovered.db list --all --include-deleted --limit 100000
work-tracker --database ~/.local/share/work-tracker/recovered.db history 1
```

`restore` checks the snapshot and refuses an existing destination, even with
`--database` set. It also refuses SQLite sidecars at the destination. The recovered
database initially matches the snapshot, including IDs and timestamps. Ordinary
commands apply current migrations and the usual 60-day deletion retention, so
expired Deleted Work Items can disappear on the first normal open. Done and
cancelled Work Items have no expiry.

To resume work, stop the dashboard and pause agents that use the old ledger.
Set the recovered path for every client, then restart them:

```bash
export WORK_TRACKER_DB="$HOME/.local/share/work-tracker/recovered.db"
work-tracker path
work-tracker today
```

Persist that setting in your shell or service configuration. Update the daily
backup service's explicit `--database` path and reload systemd as well. Changing
one shell does not redirect existing agents or a running dashboard. Keep the
previous live database until you have checked the recovered ledger.

Recovery returns the whole ledger to the selected snapshot's statuses and notes.
Later changes stay in the previous database and are absent from the recovered
copy. This does not rewind Git repositories, GitHub, CI, or files mentioned by a
Work Item. Daily backups give roughly daily recovery points while successful
backups run each day. Take a manual snapshot before an important change for a
closer recovery point.

### Protect against losing the server

Local snapshots protect against accidental ledger changes or loss of the live
database. Copy completed `.db` snapshots to a separate machine or disk to
protect against losing this server. Preserve older copies there as well. For
example, from another machine:

```bash
rsync -av your-linux-server:.local/share/work-tracker/backups/ ./work-tracker-backups/ --include='*.db' --exclude='*'
```

Only copy completed snapshots. A raw copy of the live database can miss
committed changes in its WAL. Test recovery from an external copy with `restore`
and `show` before relying on that destination.

## Agent and script usage

Every command accepts global options before or after the subcommand:

```bash
work-tracker --json today
work-tracker show 1 --json
work-tracker --database /srv/work-tracker/team.db list --json
```

`WORK_TRACKER_DB` selects the shared database without repeating `--database`. Successful commands exit with status 0. Command validation uses exit status 2; runtime and lookup errors use exit status 1 and emit `{"error":"..."}` to standard error when `--json` is enabled.

For multiple agents, point every process at the same database file on the same Linux host. SQLite serializes writes, waits up to five seconds for a busy writer, and keeps reads responsive through WAL mode. Do not put the database on a filesystem that does not support SQLite locking semantics.

## Deletion and retention

`delete` changes the status to `deleted`, records the actor and reason, and sets `purge_after` to exactly 60 days after deletion. Deleted items remain available through `show`, `history`, `list --include-deleted`, or `list --status deleted`. They cannot be changed. Opening the tracker lazily purges items whose retention window has expired, including their history.

## HTML dashboard

Start the read-only server on the Linux host:

```bash
work-tracker serve
# or: just serve
```

It binds to `127.0.0.1:8787` by default. From your PC, create an SSH tunnel and open <http://127.0.0.1:8787>:

```bash
ssh -L 8787:127.0.0.1:8787 your-linux-server
```

A different socket can be selected explicitly with `work-tracker serve --bind 127.0.0.1:9000`. Binding to a non-loopback address exposes an unauthenticated dashboard and should only be done behind appropriate network controls.

## Agent skill

The `track-work` skill is maintained in the `my-cubrid-skills` collection. Install it for Claude Code and Codex with:

```bash
npx skills add vimkim/my-cubrid-skills -y -g --agent claude-code --agent codex
```

The skill teaches agents to create a Work Item before long-running work, record meaningful notes and transitions, inspect the Daily View and the actionable list, and preserve the final outcome.

## Development

See `AGENTS.md` for architecture and invariants, and `docs/adr/` for recorded decisions. `CLAUDE.md` is a symlink to the same guidance so both agent environments receive one canonical instruction file.
