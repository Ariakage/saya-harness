# 0.1.0 之后的开发计划

**初版制订于 2026-11-07，更新于 2026-11-09。本文包含六个阶段及 DEV-038 至 DEV-089 共 52 项开发任务，实际进度见[开发进度](progress.md)。版本分组与具体接口作为规划维护。**

需求来源为项目负责人引用的《Harness功能设计》对话，历史来源标识为 `6aef3957-d280-83ee-9098-278ae10191f8`。需求已提取到本仓库的设计草案，核对时以文档、源码与决策记录为依据。0.1.0 的发行证据见[发行验证](release-validation.md)，原 P0 至 P7 和 DEV-001 至 DEV-037 定义保存在[MVP 实施任务](implementation-plan.md)。

## 需求与现有基础

| 用户需求 | 已有基础 | 交付范围与状态 | 文档 |
| --- | --- | --- | --- |
| 加入日志系统 | 五级诊断、关联 span、脱敏、轮转、筛选与跟随查询已接入 CLI | N0 已实现，各轮故障验证见进度 | [日志与追踪](observability.md)，F-12 |
| Session 导出为 Markdown 或 JSON | SQLite 一致性快照、两种导出格式及显式文件保存已接入 CLI 和 TUI | N0 已实现，各轮导出边界验证见进度 | [会话导出](session-export.md)，F-19 |
| 未来加入 `/goal` | 运行预算、会话恢复、四模式、运行内后台工具 | 目标、步骤、验收证据，随后增加受限自动续跑 | [Goal 编排](goal.md)，新增 F-20 |
| 直接输入 `saya` 进入 TUI | 默认 TUI、流式对话、人工审批、会话与工具管理已接入共享应用层 | N1 已实现，后续交互与验收按源码跟踪 | [TUI 设计](tui.md)，F-14 |

0.1.0 workspace 包含 `saya-core`、`saya-cli`。当前开发线已增加 `saya-app` 与 `saya-tui`，模型组装、会话准备、事件和人工交互桥由共享应用层提供，CLI 与 TUI 使用同一服务。`tracing`、Ratatui 和 Crossterm 已接入并由 Cargo.lock 固定。

本轮补充[Browser](browser.md)、[原生 Sandbox／AST](sandbox.md)、[上下文与长输出](context-management.md)及[saya-plugin v1 规范](saya-plugin.md)。Linux 使用项目负责人指定的 Bubblewrap + Seccomp + socat，macOS 以独立 SeatbeltBackend 承接 sandbox-exec，并通过统一后端接口预留容器／VM 实现。

## 交付分组

| 阶段 | 范围 | 主要任务 | 退出条件 |
| --- | --- | --- | --- |
| N0 共享基础 | 应用服务、事件桥接、日志、Session Export | DEV-038 至 DEV-046 | 现有 CLI 正常运行，日志及导出有离线证据 |
| N1 默认 TUI | 终端生命周期、聊天、审批、会话、入口路由 | DEV-047 至 DEV-053 | 直接运行 `saya` 打开 TUI，四平台交互验收通过 |
| N2 隔离与工具扩展 | Tool Registry、原生后端、AST、MCP、凭据 | DEV-054 至 DEV-061、DEV-075 | 实际约束和能力等级可验证，外部工具走统一调度 |
| N3 上下文与扩展包 | OutputStore、Context、Compact、检查点、Skills／插件／Hooks、全局记忆 | DEV-062 至 DEV-068、DEV-076 至 DEV-078、DEV-084 至 DEV-089 | 长输出、压缩、扩展及模型记忆编辑具有可追溯边界 |
| N4 目标编排 | Goal 数据、手动推进、持久任务、有限自动续跑与报告 | DEV-069 至 DEV-074 | 目标完成由验收证据判定，预算、恢复及授权可验证 |
| N5 Browser | 托管 Chromium、网络／资源 broker、界面及联合验收 | DEV-079 至 DEV-083 | 浏览器操作、来源、产物、权限及清理可验证 |

N0 与 N1 是 **0.2.0 的候选范围**。实际发版根据验收结果决定。N2 的接口设计可以在 N0 后开展，Sandbox／MCP 的交付优先级为高。N3、N4 按任务依赖进入开发，后续版本号在发行范围收敛时确定。

Goal 的手动阶段可在应用服务与快照稳定后单独推进。持久自动续跑以执行隔离、检查点、上下文管理和持久调度为前提。多 Agent Goal 的后续工作归 F-09。

Browser 在原生后端、MCP 与输出引用基础具备后可独立推进。新增 DEV-075 至 DEV-089 保留已有编号，执行顺序依据依赖列。

```mermaid
flowchart TD
    V[0.1.0 已发布] --> APP[N0 共享应用服务与事件]
    APP --> OBS[五级日志与关联追踪]
    APP --> EXP[一致性快照与导出]
    APP --> TUI[N1 默认 TUI]
    EXP --> TUI
    APP --> REG[N2 工具注册与执行后端]
    REG --> BOX[Sandbox]
    BOX --> MCP[MCP 与凭据]
    OBS --> CTX[N3 Context 与 Compact]
    MCP --> EXT[Skills 插件与 Hooks]
    BOX --> CHECK[检查点与持久任务]
    APP --> GM[Goal 手动推进]
    EXP --> GM
    GM --> GA[N4 有限自动 Goal]
    CTX --> GA
    CHECK --> GA
    GA --> MULTI[后续多 Agent]
    BOX --> BR[N5 Browser]
    MCP --> BR
    EXP --> OUT[OutputStore]
    OUT --> CTX
    OUT --> BR
```

## N0 共享基础

| 任务 | 内容 | 最小依赖 | 交付与验证 |
| --- | --- | --- | --- |
| DEV-038 | 提取 `saya-app` 共享应用服务 | 0.1.0 | 迁移 profile／模型组装、会话创建／恢复与锁的生命周期。CLI 测试覆盖模式覆盖、配置变更、PLAN 数据目录与恢复行为 |
| DEV-039 | 统一应用命令、流事件、人工输入与控制桥接 | DEV-038 | 复用 EventWriter／EventModel 和 RuntimeHandle。三协议文本流、序号、审批失效、取消与消费者断开通过受控测试 |
| DEV-040 | 扩展安全日志为五级类型化记录 | 0.1.0 | 保留现有日志路径、跨进程锁和 10 MiB／5 备份边界。验证级别、字段白名单、嵌套凭据与轮转 |
| DEV-041 | 接入 tracing span 与安全 subscriber | DEV-039、DEV-040 | 关联 session／run／request／call／job，异步任务归属正确。元数据经过安全记录层，压力下资源有界 |
| DEV-042 | 日志级别及 show／tail 查询 | DEV-040、DEV-041 | 提供 `--log-level`、`SAYA_LOG_LEVEL` 和本地筛选。验证轮转跟随、退出、损坏尾行、终端转义与 JSON／JSONL stdout |
| DEV-043 | 实现只读一致性 Session Snapshot | 0.1.0 | 单次读事务读取全部集合及持久化水位。并发写入、活跃会话、旧 schema、取消与读取上限有测试 |
| DEV-044 | 版本化 JSON 导出器 | DEV-043 | 独立 `saya.session` schema、范围与内容选择、复合调用 ID、未知状态及省略字段可验证 |
| DEV-045 | Markdown 导出器 | DEV-043、DEV-044 | 复用同一快照和过滤策略。验证 Unicode、代码围栏、HTML／控制字符和长工具输出 |
| DEV-046 | CLI 导出与安全文件发布 | DEV-044、DEV-045 | `session export --file` 接入。验证原子发布、已有文件、链接、私有权限、磁盘失败、取消、PLAN 路径和无模型调用 |

## N1 默认 TUI

| 任务 | 内容 | 最小依赖 | 交付与验证 |
| --- | --- | --- | --- |
| DEV-047 | 建立 `saya-tui` library 与终端生命周期 | DEV-038、DEV-039 | Ratatui／Crossterm 候选经验证后锁版本。状态更新与绘制分离，退出、错误、panic unwind 和信号恢复终端 |
| DEV-048 | 流式聊天、编辑与基础内容呈现 | DEV-047 | 多行输入、滚动、Unicode／窄终端、代码块及有限文本缓存。高频增量合并后最终文本完整 |
| DEV-049 | 模式、权限、确认／澄清和运行控制 | DEV-048 | 展示当前／待切换模式，Manual Approval 覆盖全部模型工具。取消优先、过期确认、后台清理及人工等待计时复用 core |
| DEV-050 | 会话浏览、工具详情、后台状态与导出入口 | DEV-046、DEV-049 | 新建／恢复等待输入，工具结果与 unknown 原样呈现。界面查看持久化记录与 Agent 工具调用有清楚关联 |
| DEV-051 | `saya` 默认入口切换与兼容处理 | DEV-050 | TTY 无子命令进入 TUI。非 TTY、管道输入和机器输出冲突返回用法错误。run／chat／resume／session／config 命令回归通过 |
| DEV-052 | TUI 与 CLI 四平台集成验收 | DEV-051 | PTY 故障注入、慢消费者、审批取消竞态、三协议和四模式／三权限回归，验证 stdout、退出与资源清理 |
| DEV-053 | 0.2.0 候选发行检查与双语说明 | DEV-042、DEV-046、DEV-052 | 更新使用指南和迁移说明，按选定版本构建四平台候选并验收。记录默认入口变化、安装、已知限制与证据 |

## N2 隔离与 MCP

| 任务 | 内容 | 最小依赖 | 交付与验证 |
| --- | --- | --- | --- |
| DEV-054 | 工具注册与可信操作描述 | DEV-039 | 本地与外部工具共享命名、schema、策略、取消及记录。只读声明经过宿主分类，未知外部副作用按受限操作处理 |
| DEV-055 | ExecutionBackend、文件／命令与能力探测 | DEV-054 | 统一 prepare／execute／files／cleanup，区分 hard／best_effort／unsupported。独立封装 Seatbelt，预留容器／VM 的平台和路径映射 |
| DEV-056 | Seatbelt 与 Linux 原生组合后端 | DEV-055 | 实现 macOS sandbox-exec profile 及 Linux Bubblewrap + Seccomp + socat，接入受控网络 broker 和可用资源 controller |
| DEV-057 | 隔离对抗、性能与恢复验收 | DEV-056、DEV-075 | 验证文件规则表达、namespace／syscall、代理旁路、缺失资源能力、子进程、取消和清理，记录原生开销及平台证据 |
| DEV-058 | stdio MCP 连接与服务生命周期 | DEV-054、DEV-057 | 显式服务器配置、初始化协商、发现、命名、受控启动与清理。握手失败和启动权限单独验证 |
| DEV-059 | MCP 工具接入统一运行时 | DEV-058 | schema、结果大小／内容类型、权限、调用记录、取消、列表变化及失败往返通过本地服务器夹具 |
| DEV-060 | 远程连接的凭据基础 | DEV-040、DEV-055 | 实现首个受支持凭据后端、引用、撤销和最小化注入，覆盖 F-18 的远程 MCP 所需子集 |
| DEV-061 | Streamable HTTP MCP 与授权 | DEV-059、DEV-060 | 锁定 SDK／协议矩阵，验证连接、OAuth、续期、断连、输入请求与非交互路径 |

## N3 上下文与可执行扩展

| 任务 | 内容 | 最小依赖 | 交付与验证 |
| --- | --- | --- | --- |
| DEV-062 | Context Builder、占用与大输出引用 | DEV-039、DEV-043、DEV-076 | 静态前缀、动态信息、工具、协议项与输出预留计入预算，先检索和投影长输出，保留来源与水位 |
| DEV-063 | 手动／自动 Compact | DEV-062、DEV-041 | 安全调用边界压缩、保留最近未闭合工具及协议原子组，摘要写入失败可恢复。三协议多轮与约束保真测试通过 |
| DEV-064 | Diff、检查点及恢复 | DEV-055、DEV-057 | Git／普通目录的受控改动可比较，恢复前检测外部修改，记录单文件与跨文件恢复的实际保证 |
| DEV-065 | Skill 按需激活与资源加载 | DEV-062 | 复用已实现发现机制，验证同名覆盖、路径作用域、来源、停用和上下文预算 |
| DEV-066 | saya-plugin 声明式包管理 | DEV-059、DEV-065、DEV-078 | 按 v1 规范实现本地安装、启停、更新／卸载、digest 与租约，Skill／MCP 经既有入口生效 |
| DEV-067 | 受控 Hooks 生命周期 | DEV-057、DEV-041 | 定义观察／阻止事件，验证超时、递归、幂等、拒绝、取消及重启后的执行状态 |
| DEV-068 | 扩展与 Compact 联合验收 | DEV-061、DEV-063、DEV-064、DEV-066、DEV-067、DEV-077 | 扩展、摘要、输出引用、模式、权限、恢复及检查点交叉测试，记录该批次发行范围 |

## N4 Goal 编排

| 任务 | 内容 | 最小依赖 | 交付与验证 |
| --- | --- | --- | --- |
| DEV-069 | Goal／Step／验收证据模型与存储 | DEV-038、DEV-043 | 目标与 Run 关联、依赖、预算、状态、水位、迁移及排他控制通过测试 |
| DEV-070 | 手动 Goal 与 CLI／交互命令 | DEV-069、DEV-039 | 创建、查看、人工确认计划、选择模式、继续一步、暂停、取消和报告。恢复后等待新指令 |
| DEV-071 | 持久任务与有界 worker | DEV-057、DEV-064、DEV-069 | F-08 的持久队列、租约、幂等、重启及重新授权。未知副作用等待处理，后台进程状态与计划状态分别记录 |
| DEV-072 | 有限自动续跑、验证与重规划 | DEV-063、DEV-070、DEV-071、DEV-041、DEV-077 | 收敛自动授权与模式策略，验证请求／核查／压缩总预算、失败上限、证据和取消，按所选后端记录资源保证 |
| DEV-073 | Goal TUI、导出报告与追踪关联 | DEV-050、DEV-070、DEV-044 | 目标面板、步骤、证据、暂停／继续和显式自动运行入口，查询采用一致水位 |
| DEV-074 | Goal 生命周期与发行验收 | DEV-072、DEV-073 | 跨 Run 恢复、权限变更、结果不明、worker 崩溃、预算预留和虚假完成通过故障注入，整理双语版本说明 |

## 专项基础任务

| 任务 | 内容 | 最小依赖 | 交付与验证 |
| --- | --- | --- | --- |
| DEV-075 | N2 命令 AST／CST 风险事实 | DEV-054 | 直接 argv、显式 shell、包装器、重定向、动态行为、错误与分析上限，核查输入及批准版本绑定有测试 |
| DEV-076 | N3 有界 OutputStore 与检索工具 | DEV-043、DEV-054 | 接入流式捕获、脱敏、配额、首尾预览、output_ref、output_read／search，验证完整性、权限、排空、GC 和缺失对象 |
| DEV-077 | N3 Compact 协议与故障验收 | DEV-063、DEV-076 | 三协议原子组、opaque／signature、token 误差、摘要注入、事务失败、重复触发及恢复，核对缓存前缀 |
| DEV-078 | N3 插件 manifest／lock 验证器 | DEV-040、DEV-054 | 实现结构 schema 及语义、路径、SemVer、feature、digest、依赖与别名校验，验收未知字段、损坏包和能力组合 |

## N5 Browser

| 任务 | 内容 | 最小依赖 | 交付与验证 |
| --- | --- | --- | --- |
| DEV-079 | BrowserProvider、资源及观察接口 | DEV-054、DEV-057、DEV-076 | 会话／页面／文档版本、URL 读取、显式搜索服务、快照／产物及受控操作描述 |
| DEV-080 | 托管 Chromium 与 Playwright worker | DEV-079、DEV-059 | 经原生后端启动固定版本，管理受限 MCP 通道，验证 Chromium 自有 sandbox 和 worker 生命周期 |
| DEV-081 | Browser 网络、登录和文件 broker | DEV-080、DEV-060 | 验证重定向、内网／元数据、代理旁路、陈旧元素、凭据槽、上传／下载、后台取消和 unknown 提交 |
| DEV-082 | Browser CLI／TUI 与模型能力适配 | DEV-081、DEV-050 | 展示来源、动作、审批和产物，输出检索及截图按 profile 能力接入，覆盖 PLAN／权限和慢页面 |
| DEV-083 | Browser／原生隔离／Compact／插件联合验收 | DEV-068、DEV-075、DEV-077、DEV-078、DEV-082 | 实际后端测试、注入／资源／故障矩阵、来源及状态保真、原生性能证据和双语发行说明 |

## F-07 全局记忆与模型编辑专项

候选入口为 `~/.saya/MEMORY.md`，支持用户直接审阅和专用模型工具更新，具体草案见[全局记忆](memory.md)。这些任务属于N3后续范围，当前均待开发。

| 任务 | 内容 | 最小依赖 | 交付与验证 |
| --- | --- | --- | --- |
| DEV-084 | MemoryStore与可读文件格式 | DEV-040、DEV-043 | 稳定ID、global／workspace作用域、来源与版本，文件限额、外部编辑与索引重建 |
| DEV-085 | 读取／检索与上下文投影 | DEV-062、DEV-084 | 有界文本检索、引用与版本、Memory预算和缓存失效，跨项目匹配与遗忘过滤 |
| DEV-086 | 模型记忆编辑与遗忘工具 | DEV-054、DEV-084 | 明确的变更协议、专用根、三权限模式、PLAN只读、路径及注入边界，固定默认授权粒度 |
| DEV-087 | 并发发布、回滚与遗忘一致性 | DEV-086 | 内容摘要检查、排他／原子写入、重复请求、取消、失败和外部冲突，历史与缓存一致 |
| DEV-088 | CLI／TUI管理与审阅 | DEV-050、DEV-085、DEV-087 | 搜索／查看、变更差异、批准、撤销、禁用、导出、恢复及迁移说明 |
| DEV-089 | 记忆跨会话与安全验收 | DEV-063、DEV-077、DEV-088 | 注入／凭据、并发、恢复、作用域、用户更正与遗忘、Compact／缓存联合矩阵及实际平台证据 |

## 跨阶段验收

- 原有 PLAN、deny、Manual Approval、模式切换、取消、预算和历史授权规则进入持续回归集。
- `saya run` 的 JSON／JSONL、退出码和工具调用关联作为兼容入口持续验证。
- 原始历史和执行意图／结果保存在持久层，诊断日志、导出与 UI 使用明确水位和来源。
- 每个 DEV 在适用检查通过、文档与证据更新后创建独立 Conventional Commit。推送和发行依据当次用户授权执行。
- 最终版本说明提供中英双语，支持平台依赖实际程序验收结果。新增协议、后端和迁移均记录验证矩阵。

## 在实现任务中收敛的事项

| 事项 | 收敛位置 |
| --- | --- |
| 0.2.0 实际发行范围、TUI 库版本与终端支持矩阵 | DEV-047、DEV-052、DEV-053 |
| 日志格式兼容扩展、队列上限与查询参数 | DEV-040 至 DEV-042 |
| 导出 schema、快照读取上限、错误和文件提交策略 | DEV-043 至 DEV-046 |
| 第一个 Sandbox 后端与逐平台隔离能力 | DEV-055 至 DEV-057 |
| 原生能力等级、socat 网络桥和 AST 风险事实 | DEV-055 至 DEV-057、DEV-075 |
| MCP SDK、协议和授权兼容矩阵 | DEV-058 至 DEV-061 |
| Compact 触发水位、摘要模型及协议安全边界 | DEV-062、DEV-063 |
| 输出引用／捕获配额、检索与压缩恢复 | DEV-076、DEV-077 |
| saya-plugin v1 schema、语义、lock 与能力绑定 | DEV-078、DEV-066 |
| 浏览器版本、原生约束、操作与产物策略 | DEV-079 至 DEV-083 |
| Goal 自动续跑授权、模式控制、预算和租约语义 | DEV-069 至 DEV-072 |
| 全局记忆格式、模型写入授权、遗忘与检索默认值 | DEV-084 至 DEV-089 |

这些事项已有任务和验收入口，开发阶段根据证据收敛。当前实现状态由[开发进度](progress.md)维护，功能全景继续见[能力路线图](capability-roadmap.md)。
