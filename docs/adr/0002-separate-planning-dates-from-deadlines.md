---
status: accepted
date: 2026-10-06
---

# Separate planned work from completion deadlines

A deadline-only view would omit work that must begin before its deadline, such as preparing Wednesday's presentation on Tuesday. Store Planned Date and Due Date as distinct scheduling concepts on Work Items and select the Todo View using either date. Unfinished planned work carries forward without rewriting its date; a missed plan is carried over, while only a missed deadline is overdue.

Keep this planning view distinct from the existing Daily View. Todo queries use local ledger data with the Asia/Seoul calendar dates and perform no automatic external status checks, so a routine list is available offline and does not depend on GitHub. SQLite remains the sole writable source of truth; schedule Markdown explains the plan rather than becoming a competing field store. Calendar-day horizons count weekends and holidays; no holiday dataset or calendar maintenance is required. Exact migration and mutation details are consolidated in the Todo design contract.
