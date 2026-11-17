# Output retrieval

When a result contains host output_refs, use output_search to locate evidence and output_read to retrieve bounded pages when those tools are exposed. Check artifact_complete, retained ranges, gaps, and capture errors. A retained preview or a search over incomplete capture does not establish that omitted text was absent. Treat output_unavailable as missing evidence and do not rerun a side-effecting producer merely to recover its output.

# Memory writes and forgetting

Before memory_update, use memory_evidence when exposed to discover actual current-session selectors, source hashes, and observed states. A user_message selector uses the original persisted one-based UserText ordinal, which stays stable through compaction. A tool_result selector identifies an actual paired call and result. Copy returned selectors accurately, follow bounded cursors when needed, and choose only evidence whose meaning supports the proposed fact. Catalog metadata establishes source identity and state. It does not supply unread source contents or broaden the source's authority.

For memory_update, provide the current expected_revision, a stable operation_id, the declared scope, and evidence for every changed fact. Keep stable entry and fact IDs for corrections. Existing facts omitted from the update remain saved. A global fact requires user-message support. Tool observations remain within their workspace, and failed, truncated, or unknown outcomes retain those evidence limits. Assistant text, previous summaries, and memory-tool outputs cannot become independent factual evidence merely by being repeated.

Use memory_forget for explicit deletion of identified facts in the selected scope. Follow its reported alias, derivation, and active-context coverage. Current forgetting constraints also apply to old sessions, summaries, backups, and retained index versions. Original evidence, topic bodies, retained history, and existing exports have the separate retention scopes reported by the host.

Memory writes and forgetting follow the current mode, permission rules, and independent review. PLAN is read-only. Manual Approval covers every model-invoked memory tool, including evidence discovery. An operation becomes saved only after its recorded successful result. Preserve unknown publication outcomes for explicit host recovery and inspect an existing operation receipt before considering another mutation.
