---
status: accepted
date: 2026-11-26
---

# Model Work Item dependencies as a DAG with automatic unblock

Model blocking relationships between Work Items in a separate `work_dependencies(blocked_id, blocker_id, created_at)` table rather than adding columns to `work_items`. Keeping `work_items` columns unchanged preserves existing `WorkItem` JSON contracts, `--compact` payloads, and live schema checks while allowing many-to-many dependencies with `ON DELETE CASCADE`.

Enforce a directed acyclic graph on every insertion inside an `IMMEDIATE` SQLite transaction using a recursive CTE reachability check, rejecting self-dependencies, deleted blockers, finished targets, and cycles even under concurrent writers.

Treat only `done` as resolving a blocker. Transitioning a blocker to `done` (or removing the last unresolved blocker) atomically moves downstream `blocked` items to `pending` and records a structured `unblocked` History Entry (`reason: "dependencies_resolved"`) in the same transaction. Cancelling or deleting a blocker never auto-unblocks downstream work, avoiding silent execution on broken prerequisites.

Keep `work-tracker` strictly a ledger: do not block CLI calls waiting on peers or emit push notifications from inside transactions. Agents discover runnable work via `list --ready`, `brief`, and `show`, while future external notifiers can tail `unblocked` History Entries asynchronously.
