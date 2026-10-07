---
status: accepted
date: 2026-10-07
---

# Associate work with physical directories

Associate a Work Item with an explicit Work Directory, defaulting to the actual
directory where the CLI is invoked. Store the absolute physical path so symlink
aliases share an association, while separate worktrees and subdirectories remain
distinct; automatic project-root discovery would discard the context an agent
actually selected. Keep list filtering explicit and allow corrections with
history, because directory association describes the current working context
rather than Actor identity or an immutable creation location.

Require an existing directory for assignment, but retain saved paths and permit
lookup after removal. Existing items remain unknown until explicitly assigned.
This preserves discoverability after worktree cleanup without inventing past
context or turning a directory into an ownership claim. See the
[design contract](../work-directory-design.md) for command and normalization rules.
