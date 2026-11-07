# 默认 TUI 设计

**当前开发线已接入默认 TUI，能力归 F-14。DEV-047 至 DEV-053 的 0.2.0-rc.1 候选已完成四平台原生验收，实际证据见[TUI验证](tui-validation.md)。界面细节见[TUI设计说明](tui-design.md)，正式发布安排在后续代码review与其他测试之后。**

## 默认入口

| 调用 | 当前开发线行为 |
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

当前 workspace 按以下边界组织。

```text
crates/
  saya-core/   Agent 循环、协议、工具、权限、持久化
  saya-app/    模型与会话准备、运行控制、事件与应用服务
  saya-tui/    library，输入、状态、组件与终端绘制
  saya-cli/    saya 可执行入口、参数路由、文本与机器输出
```

依赖方向为 `saya-cli -> saya-app / saya-tui`、`saya-tui -> saya-app`、`saya-app -> saya-core`。`saya-core` 保持宿主界面独立，发布产物使用统一 `saya` 可执行入口。

DEV-038／039 已将模型组装、`build_runtime`、会话准备／恢复与 `events.rs` 的共享能力迁入 saya-app。会话写锁随应用会话句柄持有，UI 切换和退出由服务完成运行清理与锁释放。

## 命令、事件与背压

界面向应用服务提交发送消息、请求模式、空闲时修改权限、回复确认／澄清、取消运行、新建／恢复会话及导出等命令。服务验证 ID、运行阶段和请求版本后调用现有 core 接口。

事件包含持久业务事件、文本增量和应用状态。saya-app 提供 EventWriter／EventModel、带背压的事件 sink、确认／澄清桥和 RunCancellation，控制继续复用 core RuntimeHandle。ChannelSink 满队列投递最多等待一秒，超限返回 event_consumer_stalled，消费者关闭返回 event_consumer_closed。人工输入桥容量由调用者选择 1 至 1024，取消同时覆盖发送与回复等待。UI 侧 reducer 更新状态，渲染使用状态快照。

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

## 紧凑窗口与会话标题

当前开发线在窗口宽度低于100列或高度低于24行时使用紧凑布局，最低支持20列×6行。黑色主区外框的顶线上显示工作目录，顶部显示实际程序版本及 Ctrl+P 命令入口。Ready／运行状态行右侧显示当前权限，灰色输入区左侧带蓝色竖线。底栏显示模式、截短的模型名称、已配置的 thinking effort 及输入用量环形指示。窄屏会缩短权限标签、状态与用量文字，详细审批仍由详情区提供。

用量指示取最近一次执行模型请求的 `input_tokens`，除以执行 profile 的 `context_window_tokens`。独立安全审核和运行累计用量分别处理。每次执行请求开始时清空旧值，完成后使用服务端报告值，估算值带 `~`，缺失时显示 `—`。新建、恢复或更换 profile 后等待新的请求用量。它反映已完成请求的输入占比，当前草稿及之后的工具输出尚未计入。环形字符按占比区间显示，超出窗口时标为 `>100%`。

终端窗口标题使用 `Saya | 会话概括名`，概括名取会话首条用户任务，先进行已知凭据脱敏，合并空白后保留前80个 Unicode 字符。后续轮次沿用首条任务标题，`/new` 和 `/profile` 重置，恢复会话时重新派生。空会话为 `Saya | New session`。标题写入前移除终端控制与双向控制字符，并限制为512字节。

标题通过 OSC 2 设置，进入／退出时使用 xterm 标题栈保存／恢复。支持情况由终端或复用器决定，PTY 验收检查发送协议。重复标题不会反复写入。正常退出、错误和 panic 复用有界终端恢复路径。[xterm 控制序列](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html)

thinking effort 的配置及三协议对应关系见[配置说明](configuration.md#thinking-effort)。

## 模式、权限与安全呈现

PLAN、BUILD、CODE、WORK 的选择来自用户操作。UI 同时展示当前模式与待切换请求，实际生效由 core 的调用边界和后台清理结果决定。Manual Approval 继续覆盖每次 Agent 工具调用，过期确认绑定的调用／策略版本由 core 拒绝。

界面查看已取得的运行记录和 Agent 发起工具调用具有不同事件来源。工具参数与输出进入显示前经过脱敏、大小控制及终端转义。用户输入、模型输出、文件内容和工具内容分别标记来源，控制序列作为可见数据处理。

## 技术与终端恢复

DEV-047 已锁定 Ratatui0.30.2、Crossterm0.29.0 和已有 Tokio，输入／状态更新与绘制分离。Ratatui启用Crossterm与layout-cache，Crossterm启用bracketed paste和use-dev-tty。独立输入线程使用50ms level poll，向容量64的队列发送键盘与窗口事件，异步主循环消费队列。该路径修复真实PTY缩放后首次Ctrl+D可能遗失的问题，退出前停止输入线程并恢复文件描述符。依赖与特性在Cargo.lock和crate配置中固定。[Ratatui 应用模式](https://ratatui.rs/concepts/application-patterns/)、[Crossterm 0.29 features](https://docs.rs/crate/crossterm/0.29.0/features)

终端 guard 管理 raw mode、alternate screen 和光标。正常返回、可恢复错误、panic unwind 及受处理信号走统一恢复路径，panic hook 链保留原诊断。强制终止场景记录尽力清理边界和终端恢复方法。初始化与恢复设计参考 [Ratatui panic hooks](https://ratatui.rs/recipes/apps/panic-hooks/)。

单次写入对瞬时背压最多等待100ms，单帧总I/O预算250ms，持续慢速消费也受帧预算约束。终端停止消费或连接关闭时返回错误并取消所属运行。恢复先停止并等待输入线程，再还原termios和文件标志，屏幕控制序列最多尝试100ms。物理终端已经断开时，恢复结果取决于终端是否仍可访问。

## 验收与迁移

DEV-047 至 DEV-053 覆盖终端状态恢复、窗口变化、Unicode／窄屏、慢消费者、审批与取消竞态、后台任务、活跃会话锁、三协议流和断连。保持 `saya run` 的机器输出、退出码及恢复行为可验证。

候选程序在 macOS 15+、Linux glibc 2.35+ 的 x86_64／aarch64 运行 PTY 和解包验收。版本说明以中英双语解释默认入口从文本 chat 到 TUI 的变化，并列出 `saya chat` 的兼容入口。任务与依赖见[后续开发计划](post-mvp-plan.md)。

## DEV-048 显示边界

库内流式协调器通过共享EventWriter／EventModel消费运行事件，绘制仅在状态变化时进行，目标帧间隔33ms。每轮最多主动消费64条已到达事件，键盘与取消信号优先处理。异常或输入断开后取消所属运行，关闭事件接收并等待运行收尾，再退出终端guard。

输入最多64KiB，单条正文256KiB，聊天正文总量2MiB及512条，截断与驱逐均显示标记。聊天保留最近10,000视觉行，详情最多保留2MiB／10,000逻辑行，超过显示范围有可见提示。100列起采用对话与详情双栏，窄屏详情占主区域。默认界面使用深色、英文角色标签，源文件为saya-tui的editor／view／controller模块。

## DEV-049 运行控制

Ctrl+P打开命令面板，方向键选择、Enter执行、Esc关闭。/mode plan|build|code|work由用户触发，标题同时显示实际模式和core待切换模式。/permissions auto|manual|bypass在运行空闲时修改本次权限设置。/status、/cancel、/help与/exit提供运行状态和控制。

### 斜杠命令补全

当前开发线在输入框首字符为 `/`、单行且光标位于末尾时显示前缀匹配建议。候选目录共24项，包含工作模式、权限、会话与工具入口，以及导出格式和需要用户补齐参数的命令前缀。命令面板、帮助和补全共用[命令目录](../crates/saya-tui/src/commands.rs)。

↑／↓循环选择候选，Tab或末尾→将候选写入输入框，Enter执行输入框中的命令。灰色后缀表示建议文本，接受后才成为草稿。Esc隐藏建议并保留输入，Tab可以重新接受该前缀的建议，输入变化时重新筛选。无匹配的命令前缀按Tab保留原文。`//`字面量斜杠、多行和光标位于中间时使用常规编辑。

候选弹层位于输入框上方，最多显示6项、选中标记和命令说明。空间不足时保留灰色后缀与底栏提示，输入框和光标获得显示空间。待人工审批时，第一次Esc或Shift+Esc关闭建议，后续Esc沿审批交互处理。

`/profile `、`/resume `、`/resume-accept `、`/cancel-job `和导出格式补全保留末尾空格，profile名、会话／任务ID及目标路径由用户填写。命令面板选择这些前缀时填入空草稿，有普通消息草稿时提示先处理草稿。缺少导出路径会同步提示补齐并保留输入。

逐工具审核显示tool、call_id、approval_id、工作区、模式及参数，y批准、n或Esc拒绝。决策要求当前审核可见，并使用新的普通／Shift按键，重复和Ctrl／Alt／Super组合键保持编辑语义。澄清在输入框填写后Enter回复，//转义支持字面量斜杠。待审核时仍可输入/mode切换，旧reply立即失效，实际授权由core在派发前重查。人工等待的活跃预算计时沿用core pause逻辑。

## DEV-050 会话与记录入口

`/new` 创建会话并等待输入，`/sessions [PAGE]` 每页列出20条记录，`/resume ID` 恢复历史、原模式与配置引用。配置变化需使用显式 `/resume-accept ID` 接受。恢复使用本次启动的权限设置，等待新输入。`/profiles` 查看配置中的模型，`/profile NAME` 在空闲时选择并释放原会话，下一条消息创建新会话。

`/tools` 和 `/jobs` 读取当前会话的一致性快照，展示持久化水位、run_id 和 call_id，保留 unknown、错误与截断状态。面板标记 UI 查询来源，沿用现有记录读取服务。`/cancel-job ID` 向当前运行请求取消所属后台任务。

`/export json|markdown PATH` 导出对话，`/export-full json|markdown PATH` 显式包含完整范围及工具记录。路径支持空格，相对路径基于当前工作区。导出复用安全文件发布、已有文件保护和 PLAN 路径检查，回执带持久化水位。已完成发布的文件即使随后取消，界面仍报告实际回执。

会话初始化、恢复和快照／导出通过独立 UI 任务执行。Ctrl+C 可请求取消，退出时等待任务收尾。UI 任务结束后才能应用排队的模式变化，权限修改要求运行与 UI 任务均空闲。读取到的正文沿用脱敏与有限显示缓存。
