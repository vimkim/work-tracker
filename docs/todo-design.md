# Todo CLI design contract

October6 update: `todo --all` and the companion shortcuts include finished scheduled Work Items. See `docs/adr/0003-todo-all-includes-finished-scheduled-work.md` for the selection and output additions that supersede the finished-item exclusion below.

Status: user confirmed the consolidated contract on October6,2026. Implementation and verification passed; task commits are being prepared for local merge review. Installation remains a separate requested step.
Work-tracker item:272. Source session: October6,2026. Vocabulary: CONTEXT.md.

## Purpose and commands

Show the user's scheduled commitments over today or the next N consecutive calendar days. Existing `work-tracker today` and `list` retain their established semantics.

```sh
work-tracker todo today
work-tracker todo --days 3
work-tracker todo --days 5
```

`work-tracker todo` defaults to today. Days must be positive integers with a representable end date; contradictory horizon selectors are rejected. Date boundaries use Asia/Seoul. A3-day view includes today and the following2 dates, including weekends and holidays. The previous working-day/holiday proposal is superseded; `--workdays`, holiday maintenance, and holiday coverage errors are out of scope.

Shortcuts `todo-today`, `todo-3days`, `todo-5days` are relative symlinks to a single `work-todo` multicall wrapper under `~/.config/my-scripts/bin`. Its basename chooses the underlying horizon. It forwards supported options such as `--json` and `--database` and propagates stdout, stderr, and exit status. A shortcut rejects user horizon overrides instead of silently changing its meaning. Direct `work-todo` invocation forwards arguments to `work-tracker todo`.

## Scheduling model and selection

- Work Items gain optional date-only Planned Date and Due Date plus Priority(high/normal/low, default normal).
- Due Dates express completion deadlines. Overdue begins the next Asia/Seoul calendar day.
- Planned Dates express intended work from that date onward; unfinished past plans carry forward without changing stored dates. Explicit rescheduling changes Planned Date.
- Either Planned Date or Due Date on/before the horizon end selects an Actionable Work Item, once. This includes previous carried-over/overdue work. Neither date means absence from the Todo View even when Priority is high.
- Planned Date may be later than Due Date. Rescheduling work never moves its deadline implicitly or hides an overdue item.
- Main section: selected pending/active items. Separate Blocked / waiting section: selected blocked/waiting items. Exclude done/cancelled/deleted.
- Order each section by Priority(high first), then Due Date(earliest first, missing last). Remaining ties use Planned Date(earliest first, missing last), then ID ascending for stability.
- Calendar dates do not skip weekends or holidays. No timestamp deadlines, recurring schedules, dependency graph, or automatic priority inference.

## Mutations and output

Use the existing add/update interface with `--planned YYYY-MM-DD`, `--due YYYY-MM-DD`, and `--priority high|normal|low`. Update also supports `--clear-planned` and `--clear-due`. Omitted fields stay unchanged; conflicting set/clear flags and invalid dates fail. Reset Priority explicitly to normal. Effective changes and from/to field history commit atomically; equal values remain no-ops.

Human output prints the actual date window and timezone, then compact rows with ID, Priority, Status, Due Date, Planned Date, title, last update, and overdue/carried-over markers. Long descriptions remain accessible with show. Empty views explain that no scheduled items match and return success; invalid input/database errors return nonzero.

`todo --json` uses a new object containing window metadata and the two item sections with their scheduling fields/markers. Existing list/today array shapes, existing fields, and server-local Daily View behavior remain compatible; scheduling fields can be additive. No automatic GitHub/CI/network refresh. Last update means ledger mutation time, not verified external freshness. Adding an external refresh command or redesigning the web dashboard is not part of this work.

## Persistence and packaging

SQLite remains the sole writable truth. Calendar source files or secondary writable schedule stores are unnecessary. Migrate additively and idempotently after inspecting schema; preserve all existing IDs, data, statuses, and history. Existing Work Items default to no dates and normal Priority.

Verified compatibility trap: old Tracker::open resets PRAGMA user_version to1. Migration cannot rely on that marker alone. Existing old update SQL names old fields explicitly, preserving additive columns. Test actual old/new access rather than trusting version metadata. Preserve current retention behavior and do not introduce unrelated database rewrites.

Tracker repository: `/home/vimkim/gh/work-tracker`; design/source worktree: `/home/vimkim/gh/work-tracker-todo-working-days`, branch `feat/todo-working-days` (historical branch name retained).
Shortcut source belongs to chezmoi under `private_dot_config/my-scripts/bin`, following existing `ls-tree -> ls-by-name`. Make source changes in its own sibling task worktree; do not edit deployed scripts as source. Follow repository review, local merge, and installation/deployment authorization rules at handoff.

## Daily-schedule integration and initial commitments

Update the source-owned daily-schedule skill to record user-supplied dates/Priority through the CLI, preserving the distinction between explicit commitments and suggested Planned Dates. Do not silently turn advice into a commitment. Ledger fields are authoritative; Markdown carries narrative and links. Locate skill source ownership before edits.

Populate these five explicitly authorized commitments after implementation, rechecking current items/statuses and avoiding duplicates:

- New independent Work Item274: make PR7990 ready for review; due2026-10-06, high; link broader221 in description without changing its overall deadline.
- Item267: make PR7925 ready for review; due2026-10-06, high.
- Item268: finish review8095; due2026-10-06, high.
- Item269: benchmark framework design+POC+team-leader presentation; due2026-10-07, high.
- Item270: finish review8096; due2026-10-08, high.

Do not invent Planned Dates, reopen completed items, rewrite original deadline dates if implementation happens later, or date the older backlog. Independently completable commitments get separate Work Items linked to broader objectives in their descriptions.

## Verification and completion

Run repository `just check`. Verify meaningful selection cases: empty and undated backlog; inclusive1/3/5-day bounds; weekend/month/year/leap transitions; Asia/Seoul day boundary; dates equal to today; overdue/carried-over union without duplicates; planned-after-due; priority versus deadline ordering; status sections; done/cancelled/deleted exclusion. Verify mutation/clear/idempotence/history and old-schema migration, repeated opens, old-writer preservation, and unchanged existing command semantics. Exercise text and JSON CLI output. Test wrapper dispatch, option forwarding, errors, and exit propagation without network or GUI.

Inspect source diffs, commit meaningful task changes, and hand back clean task worktrees for local merge review under the user's workflow. Do not claim installed aliases or seeded live dates before those steps are actually verified.

## Decision record

- Rounds1–3 established date-based selection, carryover, Priority ordering, Status sections, offline runtime, date-only deadlines, separate milestone items, and scheduling-workflow population.
- Final calendar correction: user removed holiday maintenance and requested plain3/5-day planning. This supersedes working-day counting and makes missing holiday coverage Q13 inapplicable. Q14 accepted Planned Dates later than Due Dates.
- Routine CLI, output, migration, and verification choices above complete the proposal. User confirmed shared understanding and authorized implementation on October6,2026.

## Implementation evidence — October6

- `just check`: formatting and Clippy pass;24 Rust tests pass (17 existing,7 integration tests for Todo/migration/CLI).
- Multicall wrapper:4 focused unittest tests pass; all3 shortcuts also passed end-to-end against the actual task binary in an isolated directory.
- Installed legacy binary and task binary alternated against an isolated ledger: scheduled fields survived legacy open/update/create and subsequent new reads.
- All4 chezmoi target diffs verified; tests are already excluded by repository `.chezmoiignore`. No deployment performed.
- Updated daily-schedule source passed skill-creator quick validation using an isolated uv environment with PyYAML.
- User-authorized live population completed using the task binary after SQLite-consistent backup. New milestone274 plus items267–270 have their original deadlines and high Priority; statuses and Planned Dates preserved. Today's view has3 items;3-day and5-day views each have5 items.
- Live backup and receipt: `/home/vimkim/.cache/todo-schedule-seed-20261006-163746/`. Existing installed executable and installed skill copies remain unchanged.
