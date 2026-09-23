---
status: accepted
date: 2026-09-24
---

# Integrity recovery preserves archived state

Ledger integrity recovery is the sole exception to the rule that an Archived Work Item admits no mutation: it may restore verified event bytes or append an attributed Rebaseline because leaving corrupted authoritative evidence unrepairable would defeat indefinite retention. Recovery takes the reviewed current fields and Status from the live GitHub issue, never from the disposable SQLite cache, and may change only integrity evidence and its derived projection; an Archived Work Item must remain archived and locked before success is reported.

## Considered options

- Reject recovery for Archived Work Items. Rejected because corruption would then permanently block a ledger specifically retained as evidence.
- Treat recovery as an ordinary lifecycle mutation. Rejected because a Rebaseline acknowledges damaged history; it does not reopen or otherwise change the reviewed Work Item.
- Temporarily unlock every archived issue. Rejected because Work Tracker already requires GitHub push permission, which can append recovery evidence to a locked conversation. If GitHub behavior ever makes a temporary unlock unavoidable, the unlock and relock must be externally audited and the command must not report success until the issue is locked again.

## Consequences

- Ordinary field, Status, and note commands continue to reject Archived Work Items.
- Recovery records the live GitHub projection as the reviewed state and preserves the archived Status.
- Archived recovery keeps the issue locked throughout the current implementation, avoiding a temporary mutability window.
- Rebaseline reloads the authoritative issue, lock, anchor, and evidence after publishing, then performs a second authoritative guard immediately before projection and cache unlatching. A concurrent change leaves recovery latched and requires another explicit Rebaseline; newly observed variants are latched durably so that retry retains them and the abandoned anchor as untrusted evidence.
