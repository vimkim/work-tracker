---
status: accepted
date: 2026-10-06
---

# Include finished scheduled work in todo shortcuts

The user wants `todo-today` and the other todo shortcuts to show all scheduled Work Items, including completed work. Add `work-tracker todo --all` and make the companion chezmoi wrapper supply it by default. The native command retains its unfinished-work default.

With `--all`, include done and cancelled Work Items when either Planned Date or Due Date is within the inclusive Planning Horizon. Do not carry finished work forward from previous dates or mark it overdue. Unfinished work keeps its existing selection, carryover, ordering, and Status sections. Deleted and undated Work Items remain excluded.

Show finished work in a Done / cancelled section, using the existing ordering. JSON adds a `finished` array while preserving the existing fields and sections; it is empty without `--all`. Repeated `--all` flags are harmless so explicit wrapper arguments remain valid.

The updated wrapper requires the updated binary. Install the binary before deploying the single wrapper target. Reading this view does not mutate schedules, Status, or History Entries.
