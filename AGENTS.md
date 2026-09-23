# Work Tracker Agent Guide

## Purpose

Work Tracker preserves the current status and context of long-running work across human and agent sessions. Use the terms defined in `CONTEXT.md` in code, commands, and documentation. Record hard-to-reverse decisions in `docs/adr/`.

## Architecture

- `src/domain.rs` owns Work Item, Status, and History Entry types.
- `src/db.rs` owns SQLite schema, transactions, queries, and retention.
- `src/cli.rs` owns the command-line contract and database path resolution.
- `src/output.rs` owns human-readable and JSON presentation.
- `src/web.rs` owns the read-only HTML dashboard.
- `src/main.rs` wires commands to the domain and persistence layers.

## Invariants

- GitHub Issues is the authoritative ledger after explicit GitHub initialization; SQLite remains the single source of truth only for the explicit local backend and is otherwise a disposable cache. See ADR 0002.
- Every effective mutation and standalone note appends a History Entry in the same transaction.
- Repeating an already-applied status is idempotent and does not append history.
- Archived Work Items are immutable and retained indefinitely with their history.
- The Daily View uses the server's local day and includes every Actionable Work Item.
- `list` shows only Actionable Work Items unless `--all` or `--status` widens it; see `docs/adr/0001-list-shows-actionable-work-by-default.md`.
- Keep the HTML interface read-only. Mutations belong in the CLI.
- Preserve stable JSON field names and nonzero error exits for agent callers.

## Verification

- Run `just check` before handing off code changes.
- Add database tests for lifecycle, history, retention, or concurrency changes.
- Exercise both human-readable and `--json` output when changing commands.
- Smoke-test `/` and `/items/{id}` when changing the dashboard.

## Repository workflows

- `just build` — build a debug binary.
- `just test` — run tests.
- `just check` — run formatting, Clippy, and tests.
- `just release` — build an optimized binary.
- `just install` / `just uninstall` — manage the current-user installation.
- `just serve` — run the localhost dashboard with the default database.

## Agent skills

### Issue tracker

Issues and specs are tracked in GitHub Issues for `vimkim/work-tracker`. See `docs/agents/issue-tracker.md`.

### Triage labels

Use the five canonical triage labels. See `docs/agents/triage-labels.md`.

### Domain docs

This is a single-context repository with `CONTEXT.md` and `docs/adr/` at the root. See `docs/agents/domain.md`.
