# Soyo Harness

[English](README.en.md)

使用 Rust 构建的 Agent Runtime Harness，同时支持编程任务与通用任务。Soyo 自己驱动模型与工具之间的执行循环，CLI 与默认 TUI 复用共享运行时，GUI 按后续计划推进。当前支持 macOS 15+ 与 Linux glibc 2.35+（`x86_64` / `aarch64`）。

> **更名说明**：本项目原名 **Saya Harness**，自提交 `1452a0b851ad40b385bf00e3765a4186509c4dd0` 之后更名为 **Soyo Harness**。变更：二进制 `saya`→`soyo`、crate `saya-*`→`soyo-*`、数据目录 `.saya`→`.soyo`（`.soyo` 优先、`.saya` 自动回退）、环境变量 `SAYA_*`→`SOYO_*`（`SOYO_*` 优先、`SAYA_*` 自动回退）。存储与线缆格式标识符（`saya_*_v1`、`SAYAENV1`/`SAYAOUT1`/`SAYAMED1`、`saya_capture_*`、`saya/browser/*` 等）保留旧拼写以保持既有数据与协议兼容。`v0.2.0` 及更早 Release 仍发布历史 `saya` 资产，见 [docs/releases/](docs/releases/)。

## 快速上手

### 1. 构建与安装

仓库固定 Rust `1.99.1`（见 `rust-toolchain.toml`）。在仓库根目录构建或安装 CLI：

```sh
cargo build --workspace --release --locked
# 或安装到 ~/.cargo/bin
cargo install --path crates/soyo-cli --locked
```

### 2. 初始化与校验配置

复制无凭据示例配置（或运行 `soyo config init`），填入你的模型端点、模型名与 API Key 环境变量名（凭据只通过环境变量引用，不写入配置文件）：

```sh
mkdir -p ~/.config/soyo
cp examples/config.example.toml ~/.config/soyo/config.toml
# 或使用自定义路径：
# ./target/release/soyo --config /path/to/soyo.toml config init

./target/release/soyo config check
```

> `config check` 只校验 TOML 语法、规则与环境变量引用，**不会联网或调用付费模型**。
>
> 如需在受控沙箱内运行常见构建工具链，可在 `[sandbox]` 中显式启用 `ca_certs = true` 与 `toolchain_presets = ["auto"]`（或按需指定 `["rust", "python", "node", "homebrew", "xcode_clt", "minimal_etc"]`）。若对接官方 Anthropic Messages 服务，可在对应 `[profiles.<name>]` 设置 `prompt_cache = true`（或 `--anthropic-prompt-cache`）仅启用缓存，或设置 `anthropic_official_provider = true`（兼容 `anthropic_offical_provider` 别名）全开启 Prompt Caching、自适应思考摘要与 `max_tokens` 纯文本续写（默认关闭以兼容 DeepSeek Anthropic 接口）。

### 3. 启动运行

```sh
# 进入默认交互式 TUI
./target/release/soyo --cwd /path/to/project

# 启动纯文本对话模式
./target/release/soyo --cwd /path/to/project chat

# 非交互单次执行（可显式指定工作模式）
./target/release/soyo --cwd /path/to/project --mode code run "Inspect this repository"
```

详细用法、快捷键、会话恢复与导出见[使用指南](docs/usage.md)和[配置说明](docs/configuration.md)。

## 核心特性

- **运行模式**：提供 `WORK`（默认，通用任务）、`CODE`（代码读写与调试）、`PLAN`（严格只读调研与规划）、`BUILD`（端到端实现与交付），同一会话保留上下文手动切换，在调用边界生效。
- **权限与隔离**：默认 `auto`（规则 + 命令 CST 结构分析 + 可选独立模型核查 + 人工确认），另支持 `bypass`（仍遵守 PLAN 只读、显式 `deny` 与沙箱硬边界）与 `manual`（逐工具人工确认）。原生执行默认启用 macOS Seatbelt / Linux Bubblewrap + Seccomp + `NetworkBroker` 域名白名单代理，默认网络 `deny`。
- **模型协议**：支持 OpenAI Chat Completions、OpenAI Responses API 与 Anthropic Messages；统一维护上下文预算、输出限制与超时重试。
- **工具与长程能力**：内置文件读写与搜索（`read_file`、`list_dir`、`find_files`、`search_text`、`write_file`、`edit_file`、`apply_patch`）、受控命令执行（`exec`）、运行内后台任务、预授权非手动只读并发批处理、stdio / Streamable HTTP MCP、SQLite WAL 会话持久化与导出、上下文压缩、插件／Hooks、`soyo module browser|vm enable|disable` 模块开关与 Goal 编排。

## 开发

开发任务由根目录 [justfile](justfile) 提供。安装方式和完整命令见[开发指南](docs/development.md#just-开发入口)。

```sh
just
just check
just test-tooling
just tui-preview syntax
```

## 当前版本与开发线状态

- **已发布版本**：**Soyo Harness 0.2.0** 提供 CLI 和默认 TUI（含已确认的[完整基线](docs/mvp-baseline.md) B-01 至 B-24、D-029、D-033、D-035）。历史 0.1.0、0.2.0-rc.2 与 0.2.0 发行各自保留独立源码与产物归属。[GitHub Release](https://github.com/Ariakage/soyo-harness/releases/tag/v0.2.0)、[0.2.0 双语说明](docs/releases/release-notes-0.2.0.md)、[开发进度](docs/progress.md)
- **0.2.0 之后的主线增量**：
  - N2 原生隔离、命令 CST 风险事实、stdio／Streamable HTTP MCP 与凭据管理：固定源码 `aaec8c5` 已完成四平台验收（[N2 验收](docs/n2-validation.md)）。
  - N3 上下文压缩、插件／Hooks 与全局记忆：固定源码 `e1da373` 已完成四平台验收（[N3 验收](docs/n3-validation.md)）。
  - N4 Goal 独立持久存储、结构化计划、手动／有限自动 Run 与证据验收：已完成本机受控验收（[N4 验收](docs/n4-validation.md)）。
  - N5 Browser 已交付受控 Web、托管 worker、专用 VM 网络与文件 broker、定制 pre-capture 遮蔽截图引擎、CLI／TUI 与原生／VM 媒体链路；本机 macOS、专用 VM 联合验收（`--require-screenshot` 22 passed、0 failed）及 Linux ARM64（`n5arm22` 1729 passed + 原生 Masked PNG 实测）已通过，见 [N5 验收](docs/n5-validation.md)。Linux x86-64／macOS Intel、多架构定制引擎分发及远端四平台 CI／发行证据仍有外部平台范围待完成，暂未推送或发布。
  - GUI 继续按依赖规划，后续版本号在发行范围收敛时确定。[后续开发计划](docs/post-mvp-plan.md)

完整接口、默认值与验证条件见[需求规格](docs/requirements.md)、[执行与恢复契约](docs/execution.md)与[验收索引](docs/acceptance.md)。

## 文档

| 文档 | 内容 |
| --- | --- |
| [AGENTS.md](AGENTS.md) | 编码 Agent 的工作约定 |
| [文档索引](docs/README.md) | 文档用途与状态说明 |
| [项目范围](docs/vision.md) | 已确定的产品目标、MVP 范围与演进路线 |
| [需求规格](docs/requirements.md) | 已确认需求、任务流程与验收条件 |
| [完整基线](docs/mvp-baseline.md) | 已整套确认的 24 组决定与具体契约 |
| [工作模式](docs/modes.md) | PLAN、BUILD、CODE、WORK 的职责与切换规则 |
| [执行与恢复契约](docs/execution.md) | 三种权限模式、切换边界、预授权、输出与会话恢复 |
| [运行时接口](docs/runtime.md) | 可复用 Agent 循环、调用边界和验证范围 |
| [架构设计](docs/architecture.md) | Agent 循环、模块边界与实现约束 |
| [开发指南](docs/development.md) | 开发流程与验证方式 |
| [CLI 使用指南](docs/usage.md) | 构建、配置、使用示例与故障处理 |
| [配置组件](docs/configuration.md) | 配置来源、模型 profile、凭据引用和兼容选项 |
| [模型协议](docs/model-protocols.md) | 三协议适配、流式边界和验证范围 |
| [CLI 入口与实现记录](docs/cli-preview.md) | 单次运行、文本会话与默认 TUI 入口 |
| [MVP 验收证据](docs/acceptance.md) | AC-01 至 AC-24 的测试与发行验收映射 |
| [0.1.0 双语版本说明](docs/releases/release-notes-0.1.0.md) | 功能、安装、兼容性与已知限制 |
| [0.2.0 双语版本说明](docs/releases/release-notes-0.2.0.md) | 已发布的 N0／N1、交互完善、审查修复与发行限制 |
| [0.2.0-rc.1 与开发线双语说明](docs/releases/release-notes-0.2.0-rc.1.md) | 候选与后续交互增量、迁移和限制 |
| [发行验证](docs/releases/release-validation.md) | 历史产物、源码关联、最低环境与下载复核 |
| [项目指令与 Skill 发现](docs/project-context.md) | 用户级／工作区上下文、优先级和安全边界 |
| [SQLite 存储组件](docs/storage.md) | 事务、数据路径、持久化与当前边界 |
| [soyo-plugin 规范](docs/soyo-plugin.md) | manifest v1、结构 schema、本地管理、权限与运行生命周期 |
| [后续能力路线图](docs/capability-roadmap.md) | 已有扩展基础、N3 实现范围与多 Agent 等后续规划 |
| [后续开发计划](docs/post-mvp-plan.md) | 0.2.0 发行范围、已完成的 N3 与后续阶段，DEV-038 至 DEV-089 |
| [全局记忆](docs/memory.md) | 可读 MEMORY.md、专用工具、审阅、更正／遗忘与回滚 |
| [原生 Sandbox](docs/sandbox.md) | Seatbelt、Bubblewrap／Seccomp／socat、HTTP／CONNECT broker 和 CST 边界 |
| [MCP 与凭据管理](docs/mcp.md) | stdio／HTTP 工具、服务生命周期、私有凭据与 OAuth |
| [N2 验收](docs/n2-validation.md) | 固定源码、四平台原生／MCP／Markdown／TUI 和性能证据 |
| [N3 验收](docs/n3-validation.md) | 上下文、插件／Hooks 与全局记忆的四平台固定源码验收 |
| [上下文与长输出](docs/context-management.md) | OutputStore、检索、Compact、协议及缓存 |
| [Browser 设计](docs/browser.md) | 托管浏览器、页面操作、来源、权限及产物 |
| [N5 Browser 验收](docs/n5-validation.md) | 本机／专用 VM 证据、联合矩阵与未满足的退出条件 |
| [N5 中英双语说明](docs/releases/release-notes-n5.md) | Browser 开发线功能、权限、兼容性与截图限制 |
| [TUI 设计](docs/tui.md) | 默认 soyo 入口、共享应用层、终端交互与兼容 |
| [TUI 布局](docs/tui-design.md) | 参考布局、侧栏、输入区与预览 |
| [TUI 验证](docs/tui-validation.md) | 候选及后续源码的真实终端验收 |
| [日志与追踪](docs/observability.md) | 五级诊断、关联 span、日志查询与安全边界 |
| [Session 导出](docs/session-export.md) | 一致性快照、Markdown／JSON 与文件保存 |
| [Goal 设计](docs/goal.md) | 目标、步骤、证据、手动与有限自动推进 |
| [N4 Goal 验收](docs/n4-validation.md) | Goal 存储、手动／自动推进、worker、预算、报告与故障矩阵 |
| [N4 中英双语说明](docs/releases/release-notes-n4.md) | Goal 命令、预算、恢复、安全边界与当前限制 |
| [MVP 实施路线图](docs/roadmap.md) | 8 个 MVP 里程碑、工作量估算与交付条件 |
| [实施任务](docs/implementation-plan.md) | 37 个任务、依赖、交付与基线／验收覆盖 |
| [开发进度](docs/progress.md) | 当前状态、下一任务与实际验证证据 |
| [提示词定制](docs/prompts.md) | 英文提示词、模式与审核装配、来源取舍对照和离线预览 |
| [决策记录](docs/decisions.md) | 已确认方向、基线和历史变更 |

当前实现和验证证据见[开发进度](docs/progress.md)，[使用指南](docs/usage.md)和[无凭据配置示例](examples/config.example.toml)提供 CLI 入门。

0.2.0 已交付共享应用服务、增强日志、Session Export 和默认 TUI。N2 已完成默认原生隔离、MCP 与对应四平台验收。N3 的上下文、OutputStore、Compact、选定文件检查点、本地插件／Hooks 与全局记忆已接入共享运行入口，并由固定源码 `e1da373` 完成四平台联合验收。N4 Goal 已完成本机实现与受控验收。N5 Browser 已交付受控 Web、托管 worker、专用 VM 网络与文件 broker、定制 pre-capture 遮蔽截图引擎、CLI／TUI 和原生／VM 媒体链路；本机 macOS、专用 VM 联合验收（`--require-screenshot` 22 passed、0 failed）及 Linux ARM64（`n5arm22` 1729 passed + 原生 Masked PNG 实测）已通过，见 [N5 验收](docs/n5-validation.md)。Linux x86-64／macOS Intel、多架构定制引擎分发及远端四平台 CI／发行证据仍有外部平台范围待完成，暂未推送或发布。GUI 继续按依赖规划，后续版本号在发行范围收敛时确定。[开发计划](docs/post-mvp-plan.md)、[N3 实际范围与证据](docs/n3-validation.md#四平台最终验收)、[N4验收](docs/n4-validation.md)

## 许可证

项目采用 [MIT 许可证](LICENSE)。
