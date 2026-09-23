---
status: accepted
date: 2026-09-24
---

# History Entry identity is scoped to a Work Item

A History Entry belongs to one Work Item. Its numeric ID identifies the entry within that Work Item, not across the whole ledger. GitHub-backed trusted entries use GitHub comment IDs, while integrity recovery may assign negative IDs to multiple retained variants of the same damaged comment. Treating those IDs as globally unique lets retained evidence for one Work Item collide with evidence for another after GitHub has already accepted a Rebaseline.

## Considered options

- Derive a globally unique negative ID by hashing the Work Item, comment, and body. Rejected because a bounded hash cannot guarantee collision freedom and would make identity harder to inspect.
- Reserve global synthetic IDs before publishing a Rebaseline. Rejected because reservations add durable coordination state for an identity that is only meaningful within its Work Item.
- Store retained recovery evidence outside History Entries. Rejected because untrusted evidence must remain visible in the same history view as the trusted Rebaseline that supersedes it.

## Consequences

- SQLite schema version 9 keys History Entries by Work Item ID and History Entry ID together.
- Existing History Entry IDs and stable JSON field names are preserved during migration.
- Local-backend History Entry IDs continue to be allocated monotonically; GitHub-backed entries continue to use comment IDs; recovery variants may use per-Work-Item negative IDs without cross-item collisions.
- A successful per-item cache recovery clears only that Work Item's integrity latch. It does not claim that repository-wide synchronization succeeded or advance repository freshness.
