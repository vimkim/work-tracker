# Work Tracking

This context describes work that humans and agents coordinate over long-running, parallel workflows.

## Language

**Work Item**:
A unit of work whose current state and history need to remain discoverable across human and agent sessions.
_Avoid_: Job, task, todo

**Status**:
The current lifecycle state of a Work Item: pending, active, waiting, blocked, done, cancelled, or deleted.
_Avoid_: State, phase

**Actionable Work Item**:
A Work Item with pending, active, waiting, or blocked status that still needs attention or observation.
_Avoid_: Open job, unfinished task

**History Entry**:
An immutable record of a Work Item's creation, note, field update, or Status transition, attributed to an Actor.
_Avoid_: Audit row, log line

**Actor**:
The human, agent, or automation identity responsible for a History Entry.
_Avoid_: Owner, assignee

**Deleted Work Item**:
A soft-deleted Work Item that remains readable with its History Entries during the Retention Window.
_Avoid_: Archived item, removed task

**Retention Window**:
The 60-day period after deletion during which a Deleted Work Item and its History Entries remain readable.
_Avoid_: Grace period

**Daily View**:
The server-local-day view containing Work Items updated that day and every Actionable Work Item, regardless of its last update time.
_Avoid_: Today's jobs, daily log
