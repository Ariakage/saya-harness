# 默认 TUI 设计

**状态为后续设计草案。项目负责人明确要求直接运行 `saya` 进入 TUI，能力归 F-14，候选交付为 0.2.0。**

## 默认入口

| 调用 | 规划行为 |
| --- | --- |
| `saya` | stdin、stdout 均连接终端时进入 TUI |
| `saya --cwd PATH --mode code` | 使用现有全局参数进入 TUI |
| `saya chat` | 进入已有文本交互会话 |
| `saya run "TASK"` | 执行单次任务，按所选输出模式返回 |
| `saya resume ID` | 使用已有文本会话恢复入口 |
| `saya session ...`、`saya config ...` | 执行对应 CLI 子命令 |
| `saya --help`、`saya --version` | 显示帮助或版本 |
| 无子命令且无 TTY，或 stdin 来自管道 | 用法错误／2，stderr 提示显式使用 `saya run --stdin` 等入口 |
| 无子命令且选择 `--output json` 或 `jsonl` | 用法错误／2，提示选择具体 CLI 子命令 |

顶层 `Option<Command>` 已存在，实施重点为路由、TTY 判断、参数兼容和终端生命周期。位置路径简写与顶层 `--session` 属于后续易用性候选。首版通过 `--cwd` 选择工作区，通过 TUI 会话浏览器恢复。

路由和 TTY 校验先于运行准备、数据库打开及终端切换。用法错误路径的模型请求与工具调用均为零。

## 共享应用层

规划 workspace 形状如下，当前实际结构以 Cargo.toml 为准。

```text
crates/
  saya-core/   Agent 循环、协议、工具、权限、持久化
  saya-app/    模型与会话准备、运行控制、事件与应用服务
  saya-tui/    library，输入、状态、组件与终端绘制
  saya-cli/    saya 可执行入口、参数路由、文本与机器输出
```

依赖方向为 `saya-cli -> saya-app / saya-tui`、`saya-tui -> saya-app`、`saya-app -> saya-core`。`saya-core` 保持宿主界面独立，发布产物使用统一 `saya` 可执行入口。

先将 `main.rs` 的模型组装、`build_runtime`、`session.rs` 的准备／恢复，以及 `events.rs` 的共享能力迁入应用服务。会话写锁随应用会话句柄持有，UI 切换和退出由服务完成运行清理与锁释放。

## 命令、事件与背压

界面向应用服务提交发送消息、请求模式、空闲时修改权限、回复确认／澄清、取消运行、新建／恢复会话及导出等命令。服务验证 ID、运行阶段和请求版本后调用现有 core 接口。

事件包含持久业务事件、文本增量和应用状态。已有 `ModelTextDelta`、EventWriter／EventModel、RuntimeHandle 可作为迁移基础。UI 侧 reducer 更新状态，渲染使用状态快照。

- 有界队列合并文本刷新，持久化序列及最终文本保留原有语义。
- 审批、错误、模式生效、最终结果等关键变化可靠传递，或通过持久水位和快照重建。
- 控制通道与绘制数据队列分开，取消和确认回复能够及时到达应用服务。
- 消费者断开后执行明确的前台运行取消及清理，保存中断结果。未来 F-08 持久任务再提供独立生命周期。

## 首版交互范围

| 功能 | 首版范围 |
| --- | --- |
| 对话 | 流式文本、多行编辑、滚动、代码块、清楚的用户／assistant／工具来源 |
| 状态栏 | 工作区、profile／model、四工作模式、权限、运行状态与用量 |
| 确认与澄清 | 参数与原因、批准／拒绝、过期提示、输入问题和取消 |
| 工具详情 | 执行中、结果、错误、截断、unknown 与关联 ID |
| 会话 | 新建、浏览、恢复、等待输入及导出 |
| 后台任务 | 当前运行的任务列表、输出与取消结果 |
| 控制 | 当前／待切换模式、空闲权限设置、停止与退出 |

界面以对话为主，工具、会话和诊断信息按需展开。候选快捷键为 Enter 发送、Alt+Enter 换行、Ctrl+P 命令面板、Ctrl+C 取消运行或清空输入、空闲时 Ctrl+D 退出，实际组合经终端兼容测试后固定。

Diff、模型选择、Compact、MCP／Skills、Goal 和多 Agent 面板按其底层能力交付进度开放。首版命令列表通过实际可用能力生成。

## 模式、权限与安全呈现

PLAN、BUILD、CODE、WORK 的选择来自用户操作。UI 同时展示当前模式与待切换请求，实际生效由 core 的调用边界和后台清理结果决定。Manual Approval 继续覆盖每次 Agent 工具调用，过期确认绑定的调用／策略版本由 core 拒绝。

界面查看已取得的运行记录和 Agent 发起工具调用具有不同事件来源。工具参数与输出进入显示前经过脱敏、大小控制及终端转义。用户输入、模型输出、文件内容和工具内容分别标记来源，控制序列作为可见数据处理。

## 技术与终端恢复

候选技术采用 Ratatui、Crossterm 和已有 Tokio，输入／状态更新与绘制分离。具体库版本、编辑组件、Markdown 呈现和 feature flags 在 DEV-047 的验证中选定。Ratatui 提供多种应用组织方式，Crossterm 的 `EventStream` 支持异步事件输入，需要启用对应 feature。[Ratatui 应用模式](https://ratatui.rs/concepts/application-patterns/)、[Crossterm EventStream](https://docs.rs/crossterm/latest/crossterm/event/struct.EventStream.html)

终端 guard 管理 raw mode、alternate screen 和光标。正常返回、可恢复错误、panic unwind 及受处理信号走统一恢复路径，panic hook 链保留原诊断。强制终止场景记录尽力清理边界和终端恢复方法。初始化与恢复设计参考 [Ratatui panic hooks](https://ratatui.rs/recipes/apps/panic-hooks/)。

## 验收与迁移

DEV-047 至 DEV-053 覆盖终端状态恢复、窗口变化、Unicode／窄屏、慢消费者、审批与取消竞态、后台任务、活跃会话锁、三协议流和断连。保持 `saya run` 的机器输出、退出码及恢复行为可验证。

候选程序在 macOS 15+、Linux glibc 2.35+ 的 x86_64／aarch64 运行 PTY 和解包验收。版本说明以中英双语解释默认入口从文本 chat 到 TUI 的变化，并列出 `saya chat` 的兼容入口。任务与依赖见[后续开发计划](post-mvp-plan.md)。
