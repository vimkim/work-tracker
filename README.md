# Work Tracker

Work Tracker is a small, agent-friendly status ledger for parallel and long-running work. A Rust CLI and read-only HTML dashboard share one SQLite database, so a human or agent can recover the current state and the context that led there.

## What it provides

- Transaction-safe updates from concurrent agents using SQLite WAL mode.
- Stable human-readable commands and machine-readable `--json` output.
- Immutable creation, note, update, status-transition, and deletion history.
- A default `list` that shows only actionable work, with `--all` for the full ledger.
- A Daily View containing everything updated today and every actionable item.
- A Todo View for date-based commitments, with carryover, deadlines, and explicit priorities.
- Soft deletion with a 60-day readable retention window and automatic purge.
- A localhost-only, read-only dashboard suitable for SSH tunneling.

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
| `add` | Create a Work Item, initially `pending` unless selected otherwise |
| `show ID` | Show one item, including a soft-deleted item |
| `list` | List actionable items; `--all` adds done and cancelled, `--status` selects one status |
| `today` | Show items updated today plus all actionable items |
| `todo [today]` / `todo --days N` | Show commitments through N calendar days, including today |
| `update ID` | Change title, description, planned/due dates, or priority |
| `status ID STATUS` | Apply an idempotent status transition |
| `note ID MESSAGE` | Preserve context without changing status |
| `delete ID` | Soft-delete an item for 60 days |
| `history ID` | Show the complete immutable history |
| `path` | Show the SQLite database path |
| `serve` | Host the read-only dashboard |

The statuses are `pending`, `active`, `waiting`, `blocked`, `done`, `cancelled`, and `deleted`. The first four are actionable: they are what `list` shows by default, and they remain in the Daily View even when they were not updated today.

Run `work-tracker COMMAND --help` for all options.

## Listing work

`list` answers "what is still open" and `today` answers "what happened today":

```bash
work-tracker list                 # actionable items only
work-tracker list --all           # every item, including done and cancelled
work-tracker list --status done   # exactly one status
work-tracker today                # actionable items plus anything updated today
```

Both views order work by attention first: blocked, active, waiting, pending, then finished work, most recently updated first within each group. `--all` and `--status` cannot be combined. The human-readable `list` output ends with a footer that names `--all` whenever finished items are hidden; `--json` output is always a plain array. Deleted items stay hidden from every list unless you pass `--include-deleted` or `--status deleted`.

## Planning with Todo

```bash
work-tracker add "Prepare benchmark presentation" \
  --planned 2026-10-06 --due 2026-10-07 --priority high
work-tracker todo today
work-tracker todo --days 3
work-tracker todo --days 5 --json
work-tracker update 1 --planned 2026-10-08 --note "Work rescheduled; deadline unchanged"
work-tracker update 1 --clear-planned --clear-due --priority normal
```

`todo` alone means today. An N-day horizon includes today and the following N−1 **calendar days**, including weekends and holidays, using Asia/Seoul (KST, UTC+09:00), independent of the server timezone. Dates use `YYYY-MM-DD` with years 0001–9999. Days must be positive and the end date representable. `today` and `--days` cannot be combined.

A pending, active, waiting, or blocked item appears when **either** its Planned Date or Due Date is on or before the window's end. Unfinished past plans carry forward; missed deadlines remain overdue. Each item appears once. Items without either date stay in the ordinary backlog even when high priority. Done, cancelled, and deleted items are excluded.

Actions (pending/active) and Blocked / waiting are separate sections. Within each section, sort by high/normal/low Priority, earliest Due Date, earliest Planned Date, then ID; missing dates sort last. Due today is not overdue. A Planned Date after a Due Date is allowed and does not hide the missed deadline. Reading never reschedules work.

Dates are optional and independent; normal is the default Priority. Omitted update flags preserve existing values; `--clear-planned` and `--clear-due` remove dates explicitly. Effective changes include field-level history atomically, and repeated identical updates do not add history. `show` prints scheduling fields. Existing `list` and `today` semantics stay unchanged.

Todo reads the local ledger without contacting GitHub or another service. UPDATED means ledger activity, not a live external check. Empty views succeed with an explanatory message. `--json` returns an object with `window` (`start`, `end`, `days`, `timezone`), `actions`, and `blocked_waiting`. Each row contains the Work Item fields plus `overdue` and `carried_over`; scheduling fields are `planned_date`, `due_date`, and `priority`. Existing Work Item JSON fields and list/today array shapes are preserved with additive scheduling fields.

The companion chezmoi shortcuts `todo-today`, `todo-3days`, and `todo-5days` are symlinks to a `work-todo` wrapper, dispatching by invocation name. They forward `--json` and `--database`, preserve command exit status, and reject horizon overrides. They are installed separately from this Rust binary.

### Database upgrade

Opening an existing ledger adds nullable dates and normal Priority without changing its items, statuses, or history. Migration checks actual columns under a transaction because legacy binaries reset SQLite's version marker. Old field-specific writes preserve the additional columns. Newer unknown schema versions are rejected. Keep a SQLite-consistent backup before upgrading a shared live ledger; ordinary retention housekeeping still applies on open.

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
