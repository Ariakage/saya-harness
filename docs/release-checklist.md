# 0.1.0 发行检查

**P7 验收完成，0.1.0 已发布。** 本清单汇总正式发行证据。四个平台包、校验和、验证记录与中英双语说明已公开上传并完成下载复核。[GitHub Release](https://github.com/Ariakage/saya-harness/releases/tag/v0.1.0)、[发行验证](release-validation.md)、[真实模型联调](live-model-validation.md)

## 基线证据索引

下面逐项标明主要实现／验证入口；“本机受控”表示在指定测试环境使用替身或故障注入，不能推断全部供应商、平台和未知故障都已通过。完整细节与当时限制见[开发进度](progress.md)和[验收索引](acceptance.md)。

| 基线 | 主要证据／当前缺口 |
| --- | --- |
| B-01 产品边界 | [项目范围](vision.md)、[CLI 使用指南](usage.md)；插件、MCP 等属于后续规划 |
| B-02 工程与分发 | [workspace](../Cargo.toml)、[工具链](../rust-toolchain.toml)、[许可证](../LICENSE)；四原生目标在 Ubuntu 22.04／macOS 15 完成解包验收 |
| B-03 CLI 与输入 | [CLI 端到端测试](../crates/saya-cli/tests/cli.rs)、[交互测试](../crates/saya-cli/tests/chat.rs) |
| B-04 配置 | [配置测试](../crates/saya-core/tests/config.rs)、[配置说明](configuration.md) |
| B-05 模型接入 | [协议说明](model-protocols.md)、[真实服务记录](live-model-validation.md)；仅一个服务和模型抽样 |
| B-06 上下文 | [运行时测试](../crates/saya-core/tests/runtime.rs)、[协议测试](../crates/saya-core/tests/responses.rs)；不自动摘要或跨协议迁移 |
| B-07 工作模式 | [模式与权限 CLI 夹具](../crates/saya-cli/tests/acceptance.rs)、[模式说明](modes.md) |
| B-08 调度与切换 | [运行时测试](../crates/saya-core/tests/runtime.rs)、[后台协议测试](../crates/saya-core/tests/background_protocols.rs)、[交互测试](../crates/saya-cli/tests/chat.rs) |
| B-09 权限边界 | [权限测试](../crates/saya-core/tests/permissions.rs)、[模式与权限 CLI 夹具](../crates/saya-cli/tests/acceptance.rs) |
| B-10 auto 与核查 | [核查测试](../crates/saya-core/tests/review.rs)、[profile 审核测试](../crates/saya-core/tests/profile_review.rs)；模型审核不是绝对安全保证 |
| B-11 人工确认与预授权 | [授权测试](../crates/saya-core/tests/approvals.rs)、[CLI 测试](../crates/saya-cli/tests/cli.rs) |
| B-12 文件操作 | [读取测试](../crates/saya-core/tests/read_tools.rs)、[写入测试](../crates/saya-core/tests/write_tools.rs)；外部并发修改不是原子 CAS |
| B-13 命令工具 | [命令测试](../crates/saya-core/tests/exec_tools.rs)；没有操作系统沙箱 |
| B-14 工具接口 | [运行时测试](../crates/saya-core/tests/runtime.rs)、[授权测试](../crates/saya-core/tests/approvals.rs)、[CLI 测试](../crates/saya-cli/tests/cli.rs) |
| B-15 运行预算 | [运行时测试](../crates/saya-core/tests/runtime.rs)、[配置测试](../crates/saya-core/tests/config.rs) |
| B-16 错误、取消与重试 | [运行时测试](../crates/saya-core/tests/runtime.rs)、[传输测试](../crates/saya-core/tests/transport.rs)、[命令测试](../crates/saya-core/tests/exec_tools.rs) |
| B-17 输出与退出码 | [契约测试](../crates/saya-core/tests/contracts.rs)、[CLI 测试](../crates/saya-cli/tests/cli.rs)、[验收索引](acceptance.md) |
| B-18 存储位置与格式 | [SQLite 测试](../crates/saya-core/tests/sqlite.rs)、[存储说明](storage.md) |
| B-19 保存与恢复 | [SQLite 测试](../crates/saya-core/tests/sqlite.rs)、[CLI 测试](../crates/saya-cli/tests/cli.rs) |
| B-20 并发、配置变更与兼容 | [SQLite 测试](../crates/saya-core/tests/sqlite.rs)、[CLI 测试](../crates/saya-cli/tests/cli.rs)；schema 1→2 迁移说明见存储文档 |
| B-21 日志、凭据与用量 | [CLI 测试](../crates/saya-cli/tests/cli.rs)、[开发进度 DEV-027](progress.md)、[配置说明](configuration.md)；未知秘密不保证识别 |
| B-22 项目指令与提示词 | [项目上下文测试](../crates/saya-core/tests/project_context.rs)、[提示词测试](../crates/saya-core/tests/prompts.rs)、[来源说明](project-context.md) |
| B-23 验收与 CI | [验收索引](acceptance.md)、[四平台正式发行验收](https://github.com/Ariakage/saya-harness/actions/runs/42383135600)、[发行验证](release-validation.md)；四原生目标最低环境与解包演练通过 |
| B-24 版本与后续变更 | [完整基线](mvp-baseline.md)、[存储说明](storage.md)、[决策记录](decisions.md)；双语发行说明已整理 |

AC-01 至 AC-23 的具体测试名见[验收索引](acceptance.md)。AC-24 已由原生四平台构建、342 项测试与最终候选解包演练补齐。四包的源码和哈希与实际最低环境证据一致，见[最终验收记录](release-validation.md)。

## 已知限制与待办

风险优先审查的五项确认问题已在 `854be1b` 修复并通过独立复核，其运行时代码与正式源码 `eaade53` 一致。四个正式包内的二进制均通过工具往返、会话恢复及五项审查回归，证据按各自包／二进制哈希记录。[审查记录](review-2026-11-07.md)、[发行证据](release-validation.md)

- 工具命令没有 OS 沙箱；进程组不能约束主动脱离的后代，外部并发文件替换不具备原子比较交换保证。
- 已知凭据遮蔽不识别所有未知秘密；配置显式指向模型服务时，任务和适用上下文会发送给该服务。
- 强制终止后，失去管理的后台进程可能继续运行；未知状态不会自动重放，也不表示进程已停止。
- 用量与成本依赖服务返回的数字和用户显式费率；估算不是账单。单服务小样本联调不等于对任意模型的质量承诺。
- macOS ARM 候选带链接器 ad-hoc 签名，Intel 候选未签名。正式包保持该签名状态，最低环境验收已完成，外部发布已获项目负责人授权。
- 重复键、过深嵌套和无法保真处理的数值采用失败关闭，部分无秘密 JSON 内容也可能被拒绝。
