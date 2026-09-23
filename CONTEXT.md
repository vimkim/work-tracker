# Work Tracking

This context describes work that humans and agents coordinate over long-running, parallel workflows.

## Language

**Work Item**:
A unit of work whose current state and history need to remain discoverable across human and agent sessions.
_Avoid_: Job, task, todo

**Status**:
The current lifecycle state of a Work Item: pending, active, waiting, blocked, done, cancelled, or archived.
_Avoid_: State, phase

**Actionable Work Item**:
A Work Item with pending, active, waiting, or blocked status that still needs attention or observation.
_Avoid_: Open job, unfinished task

**History Entry**:
An immutable record of a Work Item's creation, note, field update, or Status transition, attributed to an Actor.
_Avoid_: Audit row, log line

**State Revision**:
A monotonically advancing version of a Work Item's title, description, and Status used to reject stale concurrent changes. Standalone notes do not advance it.
_Avoid_: Version, update count

**Rejected Mutation**:
A proposed field or Status change that was not applied because it targeted a stale State Revision. It remains discoverable as conflict evidence but is not a History Entry.
_Avoid_: Failed History Entry, overwritten change

**Actor**:
The human, agent, or automation identity responsible for a History Entry, independent of the external account used to submit it.
_Avoid_: Owner, assignee, authenticated account

**Ledger Integrity Error**:
A detected break in the recorded History Entry sequence that leaves a Work Item readable but prevents further mutation until explicit repair.
_Avoid_: Merge conflict, stale cache

**Rebaseline**:
An explicit recovery that acknowledges untrusted history and establishes a reviewed current state as the root of a new valid History Entry sequence without erasing prior evidence.
_Avoid_: Reset, automatic repair

**Archived Work Item**:
A Work Item removed from active use and retained indefinitely with its History Entries; only explicit integrity recovery may repair its evidence without changing its reviewed fields, archived Status, or lock.
_Avoid_: Deleted Work Item, removed task

**Daily View**:
The server-local-day view containing Work Items updated that day and every Actionable Work Item, regardless of its last update time.
_Avoid_: Today's jobs, daily log
