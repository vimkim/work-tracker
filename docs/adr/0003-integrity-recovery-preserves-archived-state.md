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
- If live evidence indicates Archived while the issue is unlocked, recovery fails closed before restoring or publishing any comment; an operator must relock it and retry.
- Exact restoration latches the diagnosis before changing authoritative evidence and clears it only after full replay, projection, and cache completion, so a failed post-restore validation cannot reopen ordinary mutation. Because GitHub's comment-update API provides no conditional write precondition, recovery performs the strongest available fail-closed guard: it rereads the target immediately before the update and latches every newly observed variant instead of overwriting when that observation changed.
- Rebaseline atomically latches the diagnosis, any cacheless snapshot, and a pending anchor containing its stable event ID and exact intended bytes before publication. A lost POST response is reconciled by exact event ID, body, and Actor; the discovered GitHub comment identity is bound into the latch before further authoritative reads. Cacheless recovery conservatively treats every comment between the projected genesis and head identities as possible damaged evidence, including markerless interior comments.
- Rebaseline reloads the authoritative issue, lock, anchor, and evidence after publishing, then performs a second authoritative guard immediately before projection. A concurrent change leaves recovery latched and requires another explicit Rebaseline; newly observed variants are latched durably so that retry retains them and the abandoned anchor as untrusted evidence. If the anchor is changed or deleted before validation, both the locally published bytes and any observed changed bytes remain explicit untrusted evidence.
- Archived recovery unconditionally reapplies the lock after projection. Both recovery modes then reload the authoritative issue and comments, revalidate the complete projected sequence and full projection (readable fields, Status label, state/reason, and lock), and build the cache only from that final snapshot before clearing the integrity latch or reporting success.
