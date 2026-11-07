# MVP 验收证据索引

本索引将[需求规格](requirements.md) AC-01 至 AC-23 对应到可运行的受控测试。测试使用本地 HTTP 模型替身、临时目录和受控子进程；替身验证 Saya 的协议、权限及持久化行为，不证明真实模型能完成相同任务。四个任务夹具会实际读写临时工作区；BUILD／CODE 使用当前固定工具链的 `rustc` 编译并独立运行程序。

运行全部检查：

```sh
./scripts/check.sh
```

只运行四类任务、三协议与模式矩阵：

```sh
cargo test -p saya-cli --test acceptance --locked
```

下表中的测试名位于链接文件中；同一条验收可能需要组合 CLI、运行时和存储层证据。

| 验收 | 证据 |
| --- | --- |
| AC-01 | [CLI 交互测试](../crates/saya-cli/tests/chat.rs) `continuous_chat_keeps_history_and_changes_mode` |
| AC-02 | [CLI 端到端夹具](../crates/saya-cli/tests/acceptance.rs) `all_three_protocols_drive_cli_direct_and_tool_round_trips`；[CLI 测试](../crates/saya-cli/tests/cli.rs) `prompt_sources_are_explicit_mutually_exclusive_and_nonempty` |
| AC-03 | [CLI 端到端夹具](../crates/saya-cli/tests/acceptance.rs) `all_three_protocols_drive_cli_direct_and_tool_round_trips`；[后台协议测试](../crates/saya-core/tests/background_protocols.rs) `all_protocols_continue_while_job_runs_and_cleanup_is_observable` |
| AC-04 | [CLI 端到端夹具](../crates/saya-cli/tests/acceptance.rs) `plan_investigates_rust_project_without_changing_workspace` |
| AC-05 | [CLI 端到端夹具](../crates/saya-cli/tests/acceptance.rs) `all_work_and_permission_modes_enforce_write_boundaries_in_cli`、`plan_rejects_even_exactly_preauthorized_command`；[CLI 测试](../crates/saya-cli/tests/cli.rs) `plan_never_executes_provider_requested_writes_or_commands` |
| AC-06 | [CLI 交互测试](../crates/saya-cli/tests/chat.rs) `model_suggestion_never_switches_plan_mode_without_user_action` |
| AC-07 | [CLI 交互测试](../crates/saya-cli/tests/chat.rs) `switching_from_build_to_plan_during_model_call_blocks_generated_write`、`continuous_chat_keeps_history_and_changes_mode` |
| AC-08 | [CLI 端到端夹具](../crates/saya-cli/tests/acceptance.rs) `build_creates_and_verifies_a_text_statistics_program`、`code_reproduces_and_fixes_the_statistics_defect`、`work_summarizes_three_sources_into_a_report` |
| AC-09 | [CLI 测试](../crates/saya-cli/tests/cli.rs) `saved_session_can_be_listed_shown_and_used_for_a_new_task`；[交互测试](../crates/saya-cli/tests/chat.rs) `resume_waits_for_input_and_uses_original_mode_with_new_permissions` |
| AC-10 | [CLI 端到端夹具](../crates/saya-cli/tests/acceptance.rs) `model_tool_and_budget_failures_do_not_report_completion`；[CLI 测试](../crates/saya-cli/tests/cli.rs) `jsonl_interrupted_model_has_matching_finish_and_cancelled_result`；[命令测试](../crates/saya-core/tests/exec_tools.rs) `cancellation_terminates_process_group_and_records_result` |
| AC-11 | [CLI 端到端夹具](../crates/saya-cli/tests/acceptance.rs) 与 [HTTP 测试替身](../crates/saya-core/tests/fixtures/http_support/mod.rs)；默认无外网与付费 API 调用 |
| AC-12 | [CLI 端到端夹具](../crates/saya-cli/tests/acceptance.rs) `default_new_session_uses_work_and_auto` |
| AC-13 | [CLI 端到端夹具](../crates/saya-cli/tests/acceptance.rs) `auto_review_invalid_response_falls_back_without_executing_command`；[审核测试](../crates/saya-core/tests/review.rs) `invalid_unavailable_or_tool_call_falls_back_to_human` |
| AC-14 | [授权测试](../crates/saya-core/tests/approvals.rs) `dispatcher_integrates_all_seven_tools_and_confirmation`、`manual_background_tools_each_require_one_user_approval`；[CLI 测试](../crates/saya-cli/tests/cli.rs) `jsonl_interactive_approval_uses_tty_and_reports_resolution`、`exact_command_preauthorization_executes_only_in_that_run` |
| AC-15 | [CLI 端到端夹具](../crates/saya-cli/tests/acceptance.rs) `bypass_accesses_outside_root_but_explicit_deny_still_wins`、`all_work_and_permission_modes_enforce_write_boundaries_in_cli` |
| AC-16 | [CLI 交互测试](../crates/saya-cli/tests/chat.rs) `switching_from_build_to_plan_during_model_call_blocks_generated_write` |
| AC-17 | [CLI 端到端夹具](../crates/saya-cli/tests/acceptance.rs) `auto_review_invalid_response_falls_back_without_executing_command`；[CLI 测试](../crates/saya-cli/tests/cli.rs) `manual_noninteractive_write_does_not_execute_without_approval` |
| AC-18 | [CLI 测试](../crates/saya-cli/tests/cli.rs) `exact_command_preauthorization_executes_only_in_that_run`；[CLI 端到端夹具](../crates/saya-cli/tests/acceptance.rs) `plan_rejects_even_exactly_preauthorized_command` |
| AC-19 | [CLI 测试](../crates/saya-cli/tests/cli.rs) `json_run_emits_one_complete_result_without_text_stream_pollution`、`jsonl_run_emits_ordered_events_and_one_durable_final_result`、`machine_modes_preserve_streamed_text_and_jsonl_sequence_across_runs`、`jsonl_interactive_approval_uses_tty_and_reports_resolution`；[契约测试](../crates/saya-core/tests/contracts.rs) `result_matches_baseline_shape_including_nulls`、`final_event_contains_the_same_result_and_required_envelope` |
| AC-20 | [CLI 端到端夹具](../crates/saya-cli/tests/acceptance.rs) `resumed_session_uses_current_permissions_and_never_reuses_bypass`；[CLI 测试](../crates/saya-cli/tests/cli.rs) `exact_command_preauthorization_executes_only_in_that_run` |
| AC-21 | [CLI 交互测试](../crates/saya-cli/tests/chat.rs) `running_controls_queue_mode_and_cancel_returns_to_the_same_session`、`changing_mode_invalidates_pending_approval`；[授权测试](../crates/saya-core/tests/approvals.rs) `waiting_confirmation_is_invalidated_by_mode_or_policy_change` |
| AC-22 | [CLI 测试](../crates/saya-cli/tests/cli.rs) `background_launch_intent_failure_prevents_command_execution`、`failed_write_result_is_reported_and_recovery_never_replays_it`、`crashed_background_command_is_reported_unknown_and_never_replayed`、`busy_session_does_not_prevent_resuming_another_session`；[SQLite 测试](../crates/saya-core/tests/sqlite.rs) `real_process_lock_and_readonly_live_wal_preflight` |
| AC-23 | [CLI 测试](../crates/saya-cli/tests/cli.rs) `crashed_run_is_recovered_with_unknown_result_without_replay`；[后台协议测试](../crates/saya-core/tests/background_protocols.rs) `recovered_tool_pairs_remain_valid_for_all_three_provider_protocols` |

本阶段在 macOS aarch64／Rust 1.99.1 执行完整本机检查，共 337 项测试；P6 提交 a58da26 的四平台 CI 已通过；CI 不代替最低运行环境验证。[配置与预授权示例测试](../crates/saya-cli/tests/documented_examples.rs)另外验证文档示例的 CLI 行为。AC-24 的当前目标与最低环境证据见[发行候选验证](release-validation.md)，新的 macOS 15 部署目标候选尚待双架构最低环境验证。
