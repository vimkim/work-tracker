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

**Todo View**:
A planning view of unfinished Work Items selected by planned work dates or deadlines within a requested horizon, including overdue work.
_Avoid_: Daily View, backlog

**Due Date**:
The Asia/Seoul calendar date by which a Work Item must be completed; it becomes overdue on the following calendar day.
_Avoid_: Planned Date, start date

**Planned Date**:
The calendar date from which work on a Work Item is intended to remain visible until completion or explicit rescheduling, distinct from its completion deadline.
_Avoid_: Due Date, deadline

**Planning Horizon**:
A span of consecutive Asia/Seoul calendar dates starting today and ending N−1 days later, with weekends and holidays counted. Unfinished carried-over or overdue work from before this span also remains eligible for the Todo View.
_Avoid_: Working-day window, deadline, duration estimate

**Priority**:
The user's explicit ordering of work as high, normal, or low, with normal as the default. Priority takes precedence over Due Date within a Todo View section.
_Avoid_: Status, urgency score

**Carried-over Work**:
Unfinished work whose Planned Date has passed and which remains visible without an explicit reschedule.
_Avoid_: Overdue work

**Overdue Work**:
Unfinished work whose Due Date has passed.
_Avoid_: Carried-over work, blocked work
