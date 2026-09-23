# Work Tracker

Work Tracker is a small, agent-friendly Status ledger for parallel and long-running Work Items. A Rust CLI and read-only HTML dashboard preserve Work Item Status and the History Entries that led there. A private GitHub Issues ledger can be initialized explicitly for cross-machine use; the existing SQLite ledger remains available while the GitHub-backed command set is introduced.

## What it provides

- Transaction-safe updates from concurrent agents using SQLite WAL mode.
- Stable human-readable commands and machine-readable `--json` output.
- Immutable creation, note, update, status-transition, and archival history.
- A default `list` that shows only actionable work, with `--all` for the full ledger.
- A Daily View containing everything updated today and every actionable item.
- Permanent archival that keeps Work Items and their history readable indefinitely.
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

Work Tracker delegates GitHub authentication to the GitHub CLI and never stores a token. Install
`gh`, authenticate the account that will own or access the data repository, and verify the active
account before initialization:

```bash
gh auth login
gh auth status
```

The account needs permission to create a private repository when the default repository does not
exist, or push permission plus Issues access for an existing repository. Initialize a private
GitHub ledger only after confirming that account:

```bash
work-tracker init github
# defaults to <authenticated-user>/work-tracker-data
```

Pass `OWNER/REPO` to create or validate a different private repository. The first successful initialization becomes the default; later explicit repositories act as per-command overrides and do not rewrite it. `work-tracker path` reports the selected repository and local cache, and `--repository OWNER/REPO` selects an override.

Initialization stores repository configuration but no GitHub token. It creates the repository only after the explicit command, validates existing repositories before use, and is safe to repeat.

### Fresh, stale, and offline reads

Normal GitHub-backed reads synchronize first. If GitHub has a network or service outage after at
least one successful synchronization, they return cached data with a prominent stale warning.
JSON data stays on standard output and a structured warning is written to standard error.

- Add `--fresh` when stale data is unacceptable. The command fails nonzero instead of falling back.
- Add `--offline` to deliberately skip GitHub and inspect the cache. The output is explicitly
  marked offline/stale, and it fails until that machine has completed one successful sync.
- `--offline doctor ID` never contacts GitHub. It reports the durable cached integrity diagnosis
  for that Work Item, or fails clearly when the cache has no diagnosis to review.
- GitHub-backed writes always require GitHub. `--offline` rejects a write before changing the
  cache, and authentication, permission, validation, rate-limit, network, and service failures are
  reported separately.

### GitHub retries and repair

GitHub writes are convergent. For `add`, `update`, `status`, `note`, and `archive`, pass the same `--event-id` when retrying an uncertain publication; Work Tracker finds the prior structured event, validates its content, and returns its accepted or rejected result instead of publishing another proposal. Creation also carries a request fingerprint and pending genesis marker, but cache-independent retry requires the original event ID so a legitimate later add with identical content remains a distinct Work Item.

| Interrupted or uncertain step | Retry outcome |
|---|---|
| Issue creation response is lost | The pending marker/fingerprint locates the existing issue; no second Work Item is created. |
| Genesis comment publication is interrupted | The next create retry, or synchronization with retained local intent, publishes or discovers the same genesis event. |
| Mutation comment response is lost | Retrying with the same `--event-id` discovers the accepted or rejected proposal and does not add another effective History Entry. |
| Title, body, Status label, issue state/reason, or archive lock update is interrupted | Synchronization derives the complete projection from accepted history and repairs it. |
| Cache batch commit is interrupted | The item/history changes and synchronization cursor roll back together; the next synchronization replays the batch. |
| A projection field is edited directly on GitHub | Synchronization repairs it and emits `github_projection_repaired`; unstructured comments are left untouched. |

A field or Status race is different from an interrupted request. The first valid proposal in
GitHub comment order wins. A losing command exits nonzero with a **Rejected Mutation** containing
the expected and current State Revisions and the current values. Inspect `show`, `history`, and
`rejected`; if the change is still appropriate, submit a new proposal with a new event ID based on
the refreshed state. Reusing the losing event ID returns the same rejected result—it does not turn
the old proposal into a new mutation.

Structured event edits, deletions, unknown event schemas, and broken history heads are never repaired automatically. The affected Work Item remains inspectable with an integrity warning, trusted and untrusted History Entries are labelled, and every mutation of that item is refused before publication. Diagnose the first break without modifying GitHub:

```bash
work-tracker doctor 41
work-tracker --json doctor 41
```

The report includes GitHub comment, event, and Actor identities; expected and observed hashes; any cached exact copy; relevant timeline evidence; and whether exact restoration or only an explicit Rebaseline is eligible. Use `--offline doctor ID` to review the last durable diagnosis without making any network request.

Recovery is never automatic. Keep the diagnosis available for review, then choose exactly one
eligible operation:

```bash
# Only when doctor reports an exact verified cached copy.
work-tracker recover 41 --mode restore-exact-copy

# When exact restoration is impossible, after reviewing the live issue state and damaged evidence.
work-tracker recover 41 --mode rebaseline \
  --actor operator-name \
  --reason "reviewed the live projection and retained damaged evidence"
```

Exact restoration refuses an unverified copy. A Rebaseline records the Actor and reason, retains
the damaged evidence as untrusted, and starts a new verified chain at the reviewed live state.
Either operation fails closed if evidence changes during recovery. Archived Work Items remain
archived and locked throughout recovery.

In `--json` mode, GitHub authentication, permission, validation, rate-limit, network, service, and unknown API failures have distinct error codes on standard error. Successful data remains on standard output, including when a warning is emitted.

### Local SQLite ledger

The database is created automatically at `$XDG_DATA_HOME/work-tracker/work-tracker.db`, or at `~/.local/share/work-tracker/work-tracker.db` when `XDG_DATA_HOME` is unset. After a GitHub default is configured, pass `--database PATH` to select this explicit local backend.

An existing local ledger is never uploaded during GitHub initialization. Continue to inspect it
explicitly, including after configuring a GitHub default:

```bash
work-tracker --database /path/to/legacy.db list --all
work-tracker --database /path/to/legacy.db show 17
```

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
| `init github [OWNER/REPO]` | Create or validate a private GitHub Issues ledger and its local cache |
| `add` | Create a Work Item, initially `pending` unless selected otherwise |
| `show ID` | Show one item, including an Archived Work Item |
| `list` | List actionable items; `--all` adds done and cancelled, `--status` selects one status |
| `today` | Show items updated today plus all actionable items |
| `update ID` | Change title or description |
| `status ID STATUS` | Apply an idempotent status transition |
| `note ID MESSAGE` | Preserve context without changing status |
| `archive ID` | Permanently archive an item while retaining its history |
| `history ID` | Show the complete immutable history |
| `doctor ID` | Read-only diagnosis of a GitHub ledger integrity break |
| `path` | Show the selected repository/cache or SQLite database path |
| `serve` | Host the read-only dashboard |

The statuses are `pending`, `active`, `waiting`, `blocked`, `done`, `cancelled`, and `archived`. The first four are actionable: they are what `list` shows by default, and they remain in the Daily View even when they were not updated today.

Run `work-tracker COMMAND --help` for all options.

## Listing work

`list` answers "what is still open" and `today` answers "what happened today":

```bash
work-tracker list                 # actionable items only
work-tracker list --all           # every item, including done and cancelled
work-tracker list --status done   # exactly one status
work-tracker today                # actionable items plus anything updated today
```

Both views order work by attention first: blocked, active, waiting, pending, then finished work, most recently updated first within each group. `--all` and `--status` cannot be combined. The human-readable `list` output ends with a footer that names `--all` whenever finished items are hidden; `--json` output is always a plain array. Archived Work Items stay hidden from every list unless you pass `--include-archived` or `--status archived`.

## Agent and script usage

Every command accepts global options before or after the subcommand:

```bash
work-tracker --json today
work-tracker show 1 --json
work-tracker --database /srv/work-tracker/team.db list --json
```

`WORK_TRACKER_DB` selects the shared database without repeating `--database`. Successful commands exit with status 0. Command-line validation uses exit status 2 and code `cli_validation_failed`; runtime domain validation uses exit status 1 and code `domain_validation_failed`. Both emit `{"error":{"code":"...","message":"..."}}` to standard error when `--json` is enabled. Other runtime and lookup failures also exit with status 1; GitHub and cache failures retain their specialized structured codes.

For multiple agents, point every process at the same database file on the same Linux host. SQLite serializes writes, waits up to five seconds for a busy writer, and keeps reads responsive through WAL mode. Do not put the database on a filesystem that does not support SQLite locking semantics.

## Archival and compatibility

`archive` changes the status to `archived` and records the actor and reason. Archived Work Items remain available through `show`, `history`, `list --include-archived`, or `list --status archived`; they cannot be changed and are never purged.

For existing scripts, `delete`, `deleted`, and `--include-deleted` remain accepted as deprecated input aliases for `archive`, `archived`, and `--include-archived`. Human-readable and JSON output always use canonical archival language. Work Item JSON adds `archived_at`, keeps deprecated `deleted_at` as the same timestamp, and keeps deprecated `purge_after` as `null`.

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

### Opt-in live GitHub smoke

Ordinary tests use an isolated fake `gh` executable and never contact a real repository. A separate
live procedure is available only for release qualification. It creates a uniquely named
**disposable private repository**, verifies privacy before recording a Work Item, exercises the
GitHub-backed lifecycle and a cache rebuild, then deletes only that guarded disposable repository:

```bash
WORK_TRACKER_GITHUB_SMOKE=1 just smoke-github
# Optional owner or organization with private-repository create/delete permission:
WORK_TRACKER_GITHUB_SMOKE=1 just smoke-github my-org
```

The active `gh` account must be allowed to create private repositories and delete the disposable
repository (typically the `repo` and `delete_repo` scopes for classic tokens). Do not point this
procedure at an existing repository. It is intentionally absent from `just check`.
