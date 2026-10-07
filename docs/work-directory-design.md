---
status: accepted
date: 2026-10-07
---

# Work Directory association

Agents often work in one worktree and need to recover the Work Items associated
with that directory. Work Directory describes the current working context;
Actor continues to identify who recorded each History Entry.

Work Item: 297. All six interview decisions were accepted on 2026-10-07.
The user confirmed shared understanding and authorized implementation on the same day.

## Capture and correction

- `add` captures the command's actual current directory automatically. It does
  not discover a Git worktree or project root.
- `add --workdir PATH` explicitly selects a different directory, including an
  agent's project worktree when the command runs elsewhere.
- `update ID --workdir PATH` corrects or moves the association. An effective
  change records the old and new directory in a History Entry in the same
  transaction. An identical update remains idempotent, and Deleted Work Items
  remain immutable.
- `update ID --clear-workdir` explicitly removes the association. Omitting
  both directory flags preserves it. Setting and clearing conflict.
- Creation records the Work Directory in the creation History Entry, in the
  same transaction as the Work Item.

## Path identity and removed worktrees

- Store an absolute physical directory path. Resolve relative arguments against
  the command's current directory and resolve symlinks on existing directories.
  Thus `--workdir .`, an absolute path, and a symlink alias select the same
  existing directory.
- Automatic capture and explicit add/update assignments require an existing
  directory. Invalid paths fail without creating a Work Item or recording a
  field change. Failure to obtain the current directory is an error, rather
  than silently recording an unknown value.
- A saved association survives directory deletion or renaming. It changes only
  through an explicit CLI update; reads do not rewrite associations or discover
  a replacement worktree.
- `list --workdir PATH` permits a missing path for historical lookup. Existing
  paths resolve symlinks; for a missing path, resolve the existing ancestor and
  normalize the remaining path components. The saved absolute physical path
  remains usable after deletion. A removed symlink alias cannot reveal its
  former target; query with the saved physical path instead.
- Match complete normalized paths exactly. A parent directory does not include
  its descendants, and separate Git worktrees do not collapse into one project.
- Preserve significant spaces in paths. Empty paths are invalid. Use valid
  UTF-8 text for SQLite and JSON; reject unrepresentable paths with a clear
  error instead of silently changing their identity. CLI errors retain the
  existing nonzero exits and JSON error presentation.

## Discovery and presentation

- Plain `list` keeps its global, Actionable Work Item default. `list --workdir
  PATH` selects one exact Work Directory; `list --here` selects the command's
  current directory. `list --without-workdir` selects unknown associations.
  These three selectors are mutually exclusive.
- Directory filters combine with existing status and deletion scope. Apply
  filtering in the database query before ordering and the row limit.
- Unknown associations appear in ordinary global lists, but do not match a
  specific directory. Assign them through `update ID --workdir PATH`.
- Directory filtering is scoped to `list`. Daily and Todo Views keep their
  existing selection rules and remain global.
- Add a nullable `workdir` JSON field to Work Items across commands and views.
  Preserve existing fields and response shapes, including Todo's flattened
  Work Item fields. Unknown associations serialize as `null`.
- Show the association in human item details and the read-only dashboard,
  including both the main view and item details. Escape paths as HTML text.
  Human list/Today/Todo layouts can retain their existing compact columns;
  agents have the full field in JSON and humans can inspect item details.

## Persistence and compatibility

- SQLite remains the only writable store. Add a nullable Work Directory column;
  existing Work Items start unknown, without fabricated creation information,
  field changes, or History Entries.
- Migration preserves IDs, all existing fields and History Entries, and the
  retention lifecycle. Inspect actual columns under the existing write lock
  so repeated/concurrent opens and legacy version-marker resets do not add the
  column twice.
- Advance the supported schema version from 2 to 3. The currently installed
  version-2 binary rejects a version-3 database; installation is separate from
  this change. Preserve additive-column data under legacy field-specific writes
  where older binaries permit access. Reject future unsupported versions.
- Directory capture and filesystem validation belong to the CLI. Persistence
  APIs accept explicit directory data and do not silently capture the process
  working directory. Existing library creation paths may produce unknown
  associations for compatibility.

## Verification and handoff

Cover automatic capture using subprocess working directories; absolute and
relative overrides; symlink equivalence; significant path spaces; missing/file
assignment errors; removed-worktree lookup; exact matches excluding descendants;
unknown-only filtering; status/deletion combinations; filtering before the limit;
atomic, clearable, and idempotent corrections; and Deleted Work Item immutability.
Test migration from both earlier schemas, repeated and concurrent opens,
preserved history/fields, and future-version rejection.

Exercise human-readable and JSON CLI output, including invalid arguments and
runtime failures. Smoke-test `/` and `/items/{id}`, including HTML escaping.
Run `just check` and review the complete diff against repository standards and
this contract. Commit task changes and hand back a clean topic worktree for the
separate local rebase/fast-forward-merge confirmation required by `AGENTS.md`.
Pushing, installation, and deployment remain separate requested actions.

The confirmed test surfaces are CLI subprocesses, public Tracker persistence APIs,
and the dashboard HTTP routes.

## Verification evidence — 2026-10-07

- `just check` passes formatting, Clippy, and 40 tests, including 12 new tests.
  CLI subprocesses exercise human and JSON output, overrides, symlinks, removed
  paths, exact filters, clearing, and errors. HTTP checks cover `/` and item
  details, escaped directory text, and rejection of mutations.
- Migration fixtures cover versions 1 and 2, preserved fields and history,
  legacy field-specific writes, repeated opens, and future-version rejection.
- The actual installed version-2 binary was exercised against an isolated
  ledger: upgrade preserves its existing history and leaves the old item
  unknown; that binary rejects version 3; the new binary captures the directory.
  The live ledger was not upgraded during verification.
- The concurrent upgrade regression uses 32 fresh version-2 ledgers with eight
  simultaneous Tracker opens each. It reproduced the WAL setup race on round 1
  before the repair and passes after the repair.

## Standards

No Standards findings. Module responsibilities, domain vocabulary, transactional
history, idempotent updates, and the read-only dashboard match repository
guidance. The follow-up WAL retry remains in the persistence layer and restores
the normal busy timeout before migration. No material baseline smell warrants
a finding.

## Spec

One required finding was repaired: concurrent opens could return `DatabaseBusy`
while enabling WAL, before acquiring the schema migration lock. WAL setup now
retries within one monotonic five-second budget, limiting both SQLite's busy
timeout and retry sleeps to the remaining time. SQLite can bypass its busy
handler to avoid deadlock, as explained in the
[SQLite busy-handler documentation](https://www.sqlite.org/c3ref/busy_handler.html).

The independent reviewer reran the original CLI reproduction on 30 fresh
version-2 ledgers with eight concurrent processes each and observed no failures.
The version guard and column inspection share the same immediate transaction.
No further missing, incorrect, or out-of-scope behavior was found.

Review totals: Standards 0 findings; Spec 1 finding repaired, 0 outstanding.
