---
status: accepted
date: 2026-10-29
---

# Recover from complete SQLite snapshots

Preserve the archive as independent SQLite snapshots through SQLite's online
backup API. A snapshot captures Work Items, Statuses, History Entries, schema,
and ID allocation together, including committed changes still in WAL. Source
connections are read-only and bypass normal open-time migration and retention.
Each snapshot passes integrity, foreign-key, and schema checks before publication
under a unique filename. Manual and scheduled backups use the same operation.

Recover into a new database and refuse existing files and SQLite sidecars.
This makes recovery reviewable while active writers and the previous ledger
remain intact. Clients switch explicitly after inspection. The recovered
ledger resumes normal migrations and retention when opened by ordinary commands.
Restore does not invent item-level History Entries or replay external actions.

Keep snapshots until the user removes them. Automatic expiry would discard
recovery points from an archive whose long-term preservation prompted this work.
Daily scheduling belongs to an optional systemd user timer, which can catch a
missed run without changing the CLI's behavior. Local snapshots need independent
copies on another disk or machine to survive server loss.

History alone cannot reconstruct a lost database, and retention removes deleted
rows after 60 days. JSON views can omit data through filters and limits. Complete
SQLite snapshots avoid relying on either as the recovery format. Existing list
and History Entry contracts remain unchanged, and SQLite stays the only live
writable store.

See the [backup and recovery guide](../../README.md#back-up-and-recover-your-archive)
and the [SQLite backup API documentation](https://www.sqlite.org/backup.html).
