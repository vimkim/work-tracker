---
status: accepted
date: 2026-09-23
---

# GitHub Issues is the authoritative ledger

Work Tracker will use a dedicated private GitHub Issues repository as its authoritative ledger so one account can share Work Items across machines. The CLI remains the only supported mutation surface and uses `gh api`; issue fields are a human-readable current-state projection, while structured issue comments preserve typed History Entries and deterministic first-valid-wins concurrency. Each machine keeps a disposable SQLite cache for reads, every write requires GitHub connectivity, and unavailable reads may fall back to an explicitly stale cache.

The remote ledger starts empty instead of importing the existing local ledger. The legacy SQLite ledger remains available only through an explicit local backend. Work Item IDs are GitHub issue numbers; the dedicated repository therefore remains issues-only, with no manually created issues or pull requests.

## Considered options

- Committing the SQLite database was rejected because its WAL and locking model does not compose with Git merges or concurrent machines.
- A Git repository of append-only event files would support offline writes, but offline mutation was not required and the user preferred GitHub Issues.
- Using GitHub's native issue timeline without structured events was rejected because it would lose stable typed history, exact before-and-after values, idempotent retry, and deterministic conflict detection.

## Consequences

- Actionable statuses map to open issues; done maps to closed/completed; cancelled and archived map to closed/not-planned. Exactly one status label preserves the seven-way distinction.
- Archival replaces deletion: Archived Work Items are immutable but remain readable indefinitely. The 60-day Retention Window and automatic purge no longer exist.
- Direct GitHub edits are unsupported. Synchronization repairs issue title, body, labels, and open/closed state from accepted structured events.
- A concurrent field or Status mutation based on a stale State Revision becomes a Rejected Mutation and exits nonzero. Standalone notes commute and are accepted in GitHub comment order.
- Accepted events form a hash chain. An edited, deleted, or disconnected event raises a Ledger Integrity Error: inspection remains available, but mutations stop until explicit repair.
- Integrity repair is conservative: diagnose without mutation, restore only from an exact verified copy when possible, and otherwise require an attributed Rebaseline that retains the damaged history as untrusted evidence. Repair is never automatic.
- Archival closes and locks the issue. Done and cancelled issues remain unlocked so their Status may change later.
- Normal reads first attempt incremental synchronization, fall back to an explicitly stale cache when GitHub is unavailable, and support strict-fresh and deliberate-offline modes.
- One repository is active by default, with an explicit repository override. Events record both the responsible Actor and the authenticated GitHub account.
