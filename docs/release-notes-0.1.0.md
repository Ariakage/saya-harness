# Saya Harness 0.1.0 候选版本说明（草案）

**未发布，P7 尚未验收完成。** 当前本地候选与平台验证边界见[发行候选验证](release-validation.md)；这里的功能描述针对已实现的开发预览，并不构成发行或平台兼容性承诺。

Saya 是 Rust 实现的 CLI Agent Runtime，提供 PLAN、BUILD、CODE、WORK 工作模式；三种模型协议；文件、搜索与受权限控制的命令工具；运行内后台任务；本地 SQLite 会话与中断恢复；JSON 结果和 JSONL 事件。配置和使用见[使用指南](usage.md)。

默认 WORK／auto；PLAN 始终只读。Manual Approval 对每次工具调用要求人工批准，bypass 跳过人工确认和路径限制但保留 PLAN 与显式 deny。命令在没有操作系统沙箱的情况下运行。后台任务只属于当前运行；强制终止时，未知的子进程可能仍在运行。

配置文件及输出使用 schema v1；现有会话内部数据库 schema 为 2。识别到 schema 1 会先在线备份、再事务迁移；失败时保留原库与备份，更高版本或损坏数据库拒绝打开。详见[存储说明](storage.md)。实际使用前应自行保留数据备份；不恢复历史授权，也不自动重放未完成工具。

三协议已有本机替身测试和指定服务／模型的有限真实联调。Linux 两目标已在 Ubuntu 22.04 用户空间 smoke，macOS 两目标已在 macOS 27.2 smoke；x86_64 均使用模拟。macOS 15 双架构 CI 构建及 smoke 已通过；**先前本地候选包尚未在 macOS 15 实测，不能把 CI 结果归因于这些包。** 包哈希见[候选记录](release-validation.md)。

当前不包含 OS 沙箱、MCP、插件运行、自动上下文压缩、TUI 或 GUI；已知凭据遮蔽无法保证识别所有未知秘密。完整范围、后续计划和发行检查见[后续能力路线图](capability-roadmap.md)及[发行检查草案](release-checklist.md)。
