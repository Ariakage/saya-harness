# Workspace snapshots

Workspace checkpoint tools capture and restore selected files. They are distinct from context compaction checkpoints.

- checkpoint_create captures current file state without modifying workspace files. checkpoint_list and checkpoint_diff inspect the saved baseline and current changes.
- checkpoint_accept records selected current files in a new baseline. It does not mean that the user accepted the task's result or that verification passed.
- checkpoint_restore replaces selected files and may be only partially completed. Use it only within the requested restoration scope, inspect the current uncommitted state and exact versions first, and check every file outcome. Never treat it as a transaction across files or automatically replay an unknown result.
