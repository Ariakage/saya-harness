# 开发指南

## 当前状态

仓库已有固定 Rust 1.99.1 的最小 workspace、`saya-core` 与 `saya-cli`；核心契约与测试专用假模型循环已实现；真实 Agent 循环和 CLI 产品功能尚未实现。CI 在此前提交上已通过四平台验证，当前未提交改动仅有本机检查记录。`saya` 当前仅输出未实现提示并以非零状态退出。实际验证见[开发进度](progress.md)。

## 开发流程

1. 阅读 [AGENTS.md](../AGENTS.md)、[开发进度](progress.md)及任务涉及的设计文档。
2. 检查当前文件与工作区修改，按[实施任务](implementation-plan.md)的依赖和阶段退出条件确定当前任务。
3. 按已确认基线实现；只有改变范围、默认值或公共契约时才补充[决策记录](decisions.md)，普通内部实现自行处理。
4. 完成聚焦、完整的改动并同步相关文档，按[路线图](roadmap.md)中的完成标准验证。
5. 在进度记录中更新状态、证据、阻碍与下一任务，报告实际结果及未验证范围。

工程与全部产品契约已由 D-027 确认，见[完整基线](mvp-baseline.md)。固定 Rust 1.99.1／edition 2024，workspace 为 saya-core＋saya-cli，二进制 saya，许可证 MIT。具体依赖补丁版本在初始化时按兼容性选择并提交 Cargo.lock，不重新提出偏好问题。

根目录 [LICENSE](../LICENSE)、.gitignore、workspace 和工具链固定文件已建立。B-23 的顺序已细化为路线图 P0 至 P7、DEV-001 至 DEV-032；核心与 CLI 的依赖边界遵循 B-02。工作量估算与任务完成状态分别维护，不把计划日程当作已完成证据。

提交信息遵循 [AGENTS.md](../AGENTS.md) 中的 Conventional Commits 约定。

## 文档修改的验证

- 检查仓库内 Markdown 链接是否存在。
- 检查 README、范围、架构和决策之间是否一致。
- 明确区分现有能力与拟议功能。
- 命令示例应能在注明的条件下执行；尚未实现的命令不得写成使用说明。

## Rust 工程建立后的检查

在仓库根目录运行 `./scripts/check.sh` 可依次执行以下检查；也可在其他目录通过脚本绝对路径运行。实际结果见[开发进度](progress.md)：

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --workspace --release --locked
```

GitHub Actions 使用 Rust 1.99.1 与相同的检查脚本，配置原生 runner：Ubuntu 22.04 x86_64／aarch64（`ubuntu-22.04`／`ubuntu-22.04-arm`），macOS 15 aarch64／x86_64（`macos-15`／`macos-15-intel`）；macOS 构建设置最低部署版本 13.0。runner 的可用性和 CI 结果须以实际运行记录为准，发布前仍需验证最低运行环境。

核心使用 serde／serde_json 表达结构化协议数据，uuid 生成 UUIDv7 会话与运行标识；Tokio 目前仅作为测试依赖运行异步替身。版本在 Cargo.lock 中固定。测试使用 `tempfile` 开发依赖提供临时工作区；`crates/saya-core/tests/support` 包含辅助函数，`crates/saya-core/tests/fixtures` 为模型、核查和三协议预留离线数据目录。目录本身不表示替身或协议已实现。

CLI 功能实现后提供与 B-03 一致的安装、配置和启动说明；未构建或未 smoke 验证的产物不标记已支持。

## 核心验证要求

Agent 循环的测试优先使用可控的模型与工具替身，覆盖直接回答、工具调用后继续、工具失败、模型失败、取消以及达到限制。测试应验证可观察行为和边界，避免绑定不必要的内部步骤。

验证四种工作模式、三种权限模式、PLAN 始终只读且禁用 exec、用户切换在调用边界生效及同会话上下文保留。覆盖串行工具、最后请求优先、确认失效、取消优先以及派发前重新检查。

三类模型协议分别验证直接回答、工具请求与工具结果的往返。核查模型使用替身，覆盖规则判断、模型核查、用户兜底和非交互无法确认的路径。

会话验证保存、列出与恢复、工具协议记录闭合、unknown、存储失败、写锁与迁移。恢复后等待输入且不重执行工具，使用本次启动权限，不恢复历史授权。按 B-17 验证流式、JSON、JSONL、事件顺序与退出码。验收夹具及用例见[需求规格](requirements.md)与 B-23。

真实模型联调单独执行，明确所需配置与可能产生的费用，不作为默认测试的隐含前提。密钥通过最终选定的配置机制提供，仓库只保留不含真实凭据的示例。

## 文档随实现更新

建立 Rust 工程后更新“当前状态”；实现 CLI 后补充真实命令、配置和退出行为；公共接口稳定后再补接口说明。尚未验证的 TUI、GUI 或恢复能力继续保留为计划。
