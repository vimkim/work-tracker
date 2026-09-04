---
status: accepted
date: 2026-09-04
---

# `list` shows Actionable Work Items by default

`work-tracker list` used to show every non-deleted Work Item ordered by last update, so a ledger with 49 done items buried the 10 that still needed attention, and the owner began asking how to delete done items purely to clean the view. We decided that `list` shows only Actionable Work Items by default, that `--all` adds done and cancelled items, that `--status` keeps its single exact filter and overrides the default, and that `list` adopts the Daily View ordering: blocked, active, waiting, pending, then finished work, most recently updated first within each group. The `--json` output stays an array with unchanged field names; the human-readable table gains a one-line footer naming `--all` whenever the default filter is in effect.

## Considered options

- Separate `list-all` and `list-undone` subcommands, the original proposal. Rejected because two commands would serve one query and "undone" would be a synonym for the existing term Actionable Work Item.
- Default to "everything except done" so cancelled items stay visible. Rejected because cancelled work is finished work and belongs with done in the full view.
- Bulk-delete done items to clean the view. Rejected because deletion becomes permanent after the 60-day Retention Window and the `track-work` skill tells agents to keep done items as history. The new default removes the reason for it.

## Consequences

- `today` stays a separate command. It differs from `list` by also showing work finished today, which is why agents check `today --json` before registering work and use `list --json` to answer "what is open".
- A caller that relied on `list` returning finished items must now pass `--all`. On 2026-09-04 no known skill or script did.
- The dashboard is unchanged. Its `/` page is the Daily View, which already hides stale finished work and orders by status.
- Staleness of long-running active items is out of scope and tracked separately.
