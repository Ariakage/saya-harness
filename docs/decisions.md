# 决策记录

记录影响产品方向或关键接口的选择。当前 CLI MVP 以 D-027 确认的[完整基线](mvp-baseline.md)为准；早期记录保留确认历史，后续细化通过影响栏追踪。

## 已确认

| 编号 | 日期 | 决策 | 依据与影响 |
| --- | --- | --- | --- |
| D-001 | 2026-11-06 | 使用 Rust 构建 Agent Runtime Harness | 工具链与依赖职责已由 D-027 B-02 确定 |
| D-002 | 2026-11-06 | MVP 交付 CLI，之后做 TUI 和 GUI | 首版与后续范围由 D-027 B-01 确定；后续界面不阻塞 MVP |
| D-003 | 2026-11-06 | Saya 自己驱动 Agent 循环 | 项目负责人选择调用模型、执行工具并继续下一轮；不以托管已有 Agent 作为 MVP 核心路径 |
| D-004 | 2026-11-06 | 同时支持编程任务与通用任务 | 两类任务的验收夹具已由 D-027 B-23 确定 |
| D-005 | 2026-11-06 | PLAN、BUILD、CODE、WORK 是四个可切换的工作模式 | 控制权与上下文见 D-010、生效时机见 D-017，选择语法见 D-027 B-03 |
| D-006 | 2026-11-06 | BUILD 负责端到端实现和交付，CODE 聚焦代码编写、修改与调试 | 具体交付标准由 D-027 B-07 确定 |
| D-007 | 2026-11-06 | PLAN 负责需求分析与规划，WORK 负责通用任务执行 | PLAN 边界见 D-009，权限见 D-021 至 D-026，输出与切换细节见 D-027 |
| D-008 | 2026-11-06 | CLI 同时支持交互会话和单次命令 | 命令语法与输入输出契约由 D-027 B-03、B-17 确定 |
| D-009 | 2026-11-06 | PLAN 只读调查并输出计划，不修改工作区文件 | 调查工具与通用命令禁用规则由 D-027 B-09、B-14 确定 |
| D-010 | 2026-11-06 | 用户手动切换工作模式，Agent 可建议；同一会话保留上下文 | 确认切换控制权与上下文规则；生效时机由 D-017 细化 |
| D-011 | 2026-11-06 | MVP 接入 OpenAI Chat Completions、OpenAI Responses API、Anthropic Messages | 输出见 D-019，适配与配置见 D-027 B-04 至 B-06；实际使用模型由配置提供 |
| D-012 | 2026-11-06 | 首批工具为本地文件读写、目录／内容搜索和命令执行 | 权限策略见 D-021 至 D-026，工具参数与限制见 D-027 B-12 至 B-16 |
| D-013 | 2026-11-06 | 本地保存会话，支持列出和恢复 | 恢复默认行为见 D-020、D-026，存储契约见 D-027 B-18 至 B-20 |
| D-014 | 2026-11-06 | Commit message 使用 Conventional Commits | 项目负责人明确仓库提交规范，已写入 AGENTS.md；不构成执行提交的指令 |
| D-015 | 2026-11-06 | 新会话默认 WORK | 项目负责人选择默认直接处理通用任务；恢复会话遵循 D-020 |
| D-016 | 2026-11-06 | 早期全局规则：工作区文件读写自动执行，命令执行前询问 | 曾由项目负责人选择；已由 D-021 至 D-024 的权限模式策略取代，不再作为统一全局策略，保留本条作为历史 |
| D-017 | 2026-11-06 | 工作模式切换在当前调用完成后、下一次调用前生效 | 项目负责人选择先记录请求，再在调用边界生效；细化 D-010 |
| D-018 | 2026-11-06 | 单次命令允许显式预授权；无法询问且未获授权时返回需要确认并退出 | 预授权语法与匹配见 D-027 B-11，退出码见 B-17 |
| D-019 | 2026-11-06 | 提供流式交互、JSON 最终结果和 JSONL 事件输出 | 具体字段与事件契约由 D-027 B-17 确定 |
| D-020 | 2026-11-06 | 恢复历史、原工作模式和配置引用后等待新输入，不自动重放工具 | 确认恢复默认行为；不把恢复会话等同于自动续跑进程 |
| D-021 | 2026-11-06 | 权限有 auto、bypass permissions、Manual Approval 三种模式 | 项目负责人补充权限模式，分别按 D-022 至 D-024 定义，取代 D-016 的统一全局策略 |
| D-022 | 2026-11-06 | auto 采用规则判断、必要时模型核查、最后用户兜底 | 规则格式、触发条件与核查配置由 D-027 B-10 确定 |
| D-023 | 2026-11-06 | 早期范围：人工确认文件写入和命令执行，读取自动放行 | 该范围已被 D-032 替代；保留为历史决定 |
| D-024 | 2026-11-06 | bypass permissions 跳过人工确认与路径限制，PLAN 仍只读 | 项目负责人明确绕过范围；工作模式的只读边界继续生效 |
| D-025 | 2026-11-06 | 默认权限模式为 auto | 项目负责人确认默认值；与默认工作模式 WORK 分别处理 |
| D-026 | 2026-11-06 | 恢复会话使用本次启动的权限设置，不从历史恢复授权 | 项目负责人确认权限恢复策略；原工作模式仍按 D-020 恢复 |
| D-027 | 2026-11-06 | 整套采用 CLI MVP 完整基线 B-01 至 B-24 | 项目负责人一次性确认全部选项；覆盖 CLI、配置、协议、权限、工具、预算、输出、存储、工程与验收，当前无待决 MVP 产品阻塞项；不代表已经实现或验证 |
| D-028 | 2026-11-06 | 后续开发计划需要覆盖 Sandbox、MCP 等新增能力 | 项目负责人澄清需要“之后开发什么”的功能规划；已另建能力路线图，具体范围和优先级属于规划方案，不改写 D-027 的 MVP 契约或宣称新增功能已实现 |
| D-029 | 2026-11-06 | 所有本地工具可显式在当前运行内后台执行，模型可继续工作并查询结果 | 项目负责人确认：后台任务不跨运行；正常结束时取消并清理未完成任务；切换 PLAN 前取消并清理后台写入与命令。更新 D-027 的 B-08、B-13 至 B-15 调度表述，既有权限、预算与恢复约束继续生效。实现与验证见开发进度，不代表功能已可用 |

| D-030 | 2026-11-06 | 根据提供的 session、cc-extract／cc-prompts 与 harness-kit 重写 Saya 提示词；runtime prompt 用英文，不固定沟通语言 | 项目负责人要求深度定制并对照移除／新增内容。分层核心、四模式、三权限、后台和独立核查，按 Saya 契约移除来源特有能力；语言跟随用户要求或当前请求。接入及验证范围见提示词文档，不更改硬权限或承诺真实模型效果 |

| D-031 | 2026-11-06 | 未来为 Saya 规划插件系统 `soyo-plugin` | 项目负责人提出未来方向；与 F-03 对齐，具体包格式、隔离和分发契约待后续设计，不自动扩入 MVP |

| D-032 | 2026-11-06 | 权限模式命名为 Manual Approval，标识为 `manual`；每次工具调用前需人工批准 | 项目负责人明确包括读取、搜索、后台启动／状态／取消；模型请求不单独确认。单次精确命令预授权可满足对应确认。替代 D-023，历史序列化值 `review` 仅为兼容读取。 |

| D-033 | 2026-11-07 | 默认读取工作区及用户级约定指令，并发现 `.agents`、`.soyo` 下的 Skills | 项目负责人确认同时使用用户主目录与工作区，工作区指令和同名 Skill 优先。修订 B-22 和 DEV-021；发现不等于安装插件、执行脚本或授予权限。运行配置继续按 B-04 显式选择，不自动合并工作区配置。实现状态以进度记录为准。 |

| D-034 | 2026-11-07 | 注入动态宿主与工作上下文，并优化提示词前缀缓存 | 项目负责人要求时空、环境、项目、职责、偏好、记忆与安全边界，以及静态前缀缓存。个人偏好和记忆由本次调用显式提供，内容与项目材料保持低信任；Anthropic 缓存标记按端点能力显式开启。修改 B-03 的可选参数和 B-22 的提示词装配；不改变权限策略或声称已验证服务端命中。 |

| D-035 | 2026-11-07 | macOS 最低支持版本由 13 调整为 15 | 项目负责人确认 macOS 15+；修订 B-02、B-23 和 DEV-031 的部署目标与最低环境验收。Linux glibc 2.35+ 及两系统 x86_64／aarch64 目标不变；已建立的 macOS 13 候选和 CI 仅作为历史证据，新的 15.0 候选仍需按目标环境验证。 |

## 0.1.0 之后的需求方向

以下方向来自项目负责人在《Harness功能设计》中的明确需求及本轮制定计划的指令。版本范围、crate 拆分、参数和状态机在对应设计中作为草案维护。

| 编号 | 日期 | 决策 | 依据与影响 |
| --- | --- | --- | --- |
| D-036 | 2026-11-07 | 增强日志系统，并支持 Session 导出 Markdown／JSON | F-12 增加五级诊断与 tracing 规划，新增 F-19。基于现有安全日志和 SQLite 发展一致性快照、导出格式及文件保存，设计见日志与导出文档 |
| D-037 | 2026-11-07 | 未来提供 `/goal` 目标能力 | 新增 F-20，规划目标、步骤、验收证据及分阶段持续推进。高层编排、预算和模式控制的具体方案见 Goal 设计草案 |
| D-038 | 2026-11-07 | 后续直接运行 `saya` 进入 TUI | 细化 D-002，未来修改 B-03 的无子命令行为。TTY、共享应用层与命令兼容按 TUI 设计验证，0.1.0 已发布行为保留在原基线和发行记录中 |
| D-039 | 2026-11-08 | 细化 Browser、Sandbox／AST、Compact／长输出与 soyo-plugin 规范 | 项目负责人要求具体设计。形成 Browser、原生隔离、上下文及插件 v1 草案，包含 manifest 示例和结构 schema。具体权限、参数及限额按任务验收收敛 |
| D-040 | 2026-11-08 | Linux Sandbox 使用 Bubblewrap + Seccomp + socat | 项目负责人明确指定原生组合。network namespace、系统调用过滤、代理桥及实际出口共同验证，资源硬限额按 cgroup 能力报告，首轮实现见 Sandbox 设计 |
| D-041 | 2026-11-08 | 将 Seatbelt 的 sandbox-exec 封装为独立后端，并预留容器／VM 实现路径 | 项目负责人明确 macOS 后端边界。core 依赖 ExecutionBackend 与能力接口，Seatbelt profile 留在后端内。deprecated 状态、系统版本、资源与清理差异按平台验证 |
| D-042 | 2026-11-08 | 调整紧凑 TUI、会话窗口标题并接入 thinking effort | 项目负责人提供参考图，确认会话概括名沿用首条任务短标题，max表示thinking effort，并要求本轮接入配置、三协议适配和实际参数显示。开发线采用黑底外框、工作目录顶边、灰底蓝条输入、状态右侧权限及输入占比环形指示。profile可选五档effort，省略沿用服务默认，详见[配置](configuration.md#thinking-effort)和[TUI](tui.md#紧凑窗口与会话标题) |
| D-043 | 2026-11-08 | 所有窗口统一参考样式，并增加全屏信息侧栏 | 项目负责人指出试用程序在大窗口仍进入旧界面，并补充全屏设计。主区始终使用黑底外框与灰底蓝条输入。宽屏侧栏包含会话名、上下文分类条形图与百分比、扩展状态和工作目录。分项采用实际请求组件的明确估算，MCP／插件按当前实现状态呈现。尺寸断点与统计口径见[TUI设计](tui-design.md) |
| D-044 | 2026-11-08 | 细化TUI视觉层次，并提供空输入Tab模式循环 | 项目负责人提供OpenCode界面作为细节参考，要求改善灰度、留白和正文层次，并指定空输入时以Tab切换work／code／plan／build。开发线采用共享低对比配色、有界正文Markdown样式和WORK→CODE→PLAN→BUILD循环，模式生效与审批规则继续由现有运行时控制 |
| D-045 | 2026-11-08 | 后续实现可由模型编辑的全局记忆，参考可读的全局MEMORY.md | 项目负责人要求纳入后续路线和任务。F-07新增DEV-084至DEV-089，候选入口为 `~/.soyo/MEMORY.md`，设计包含读取检索、模型更正与遗忘、来源／作用域、人工审阅、并发发布及回滚。具体格式和默认授权在对应任务中验收收敛，实际状态见进度记录，设计见[全局记忆](memory.md) |
| D-046 | 2026-11-08 | 提供模型任务清单、可读Thinking卡片、长工具折叠与TUI内鼠标滚动 | 项目负责人要求全屏todolist及对应模型工具，Thinking可点击展开，长工具结果可折叠，滚轮控制应用内上下文。开发线新增todo_write会话元数据工具与todos上下文，PLAN允许任务元数据更新，Manual Approval逐调用批准。三协议可读推理通过独立应用层日志投影，签名和加密项按原协议处理，限额与交互见[TUI](tui.md#thinking任务清单与折叠) |
| D-047 | 2026-11-08 | 四工作模式使用专属色，输入框色条与底栏模式文字联动 | 项目负责人要求Tab切换模式时联动输入色条与模式文字。开发线采用WORK晴蓝、CODE薄荷绿、PLAN薰衣草紫、BUILD暖杏橙，按实际生效模式更新色条，待切换目标文字采用其自身颜色。配色和调用边界显示见[TUI设计](tui-design.md#四模式颜色) |
| D-048 | 2026-11-08 | Ctrl+P采用独立弹出命令面板 | 项目负责人要求Command改为弹出面板。开发线提供居中圆角浮层、命令说明、高亮选择和有界滚动窗口，键盘／鼠标由模态边界处理，关闭回到真实详情与草稿。与既有24项目录和运行服务衔接，细节见[TUI](tui.md#弹出命令面板) |

| D-049 | 2026-11-09 | 扩展Markdown、Unicode数学、Mermaid和脚注导航，保留source／render选择并提供debug预览 | 项目负责人逐项指定标题、行内样式、代码高亮、嵌套结构、表格、链接／图片描述、HTML源码、脚注及数学／图表回退，并要求render_preview展示及仅debug构建提供 `saya tui-preview`。具体支持子集、显示预算和交互见[Markdown说明](markdown-rendering.md)。本轮构建本地测试版，四平台同步验证安排在N2。 |

| D-050 | 2026-11-09 | N2默认启用原生隔离，按平台选择Seatbelt／Bubblewrap | 项目负责人明确选择默认原生隔离。自N2开发线起覆盖B-16的执行隔离默认值，后端或必需约束不可用时明确失败。B-06的工作模式、权限模式和人工批准继续独立处理，bypass仍受所选后端硬边界约束。实现和原生证据见[隔离设计](sandbox.md)与[N2验收](n2-validation.md)。 |

## 当前基线与变更管理

| 编号 | 日期 | 决策 | 依据与影响 |
| --- | --- | --- | --- |
| D-051 | 2026-11-09 | 启动完整N3与验收，并学习指定压缩／记忆材料，提示词采用纯英文结构 | 项目负责人设定持续目标，范围包含上下文、OutputStore、Compact、检查点、Skills、插件、Hooks与全局记忆的16项DEV。参考材料作为来源数据，英文提示词按Saya实际能力与权限接入。[N3验收](n3-validation.md) |
| D-052 | 2026-11-09 | 全局记忆默认启用读取与注入，auto写入及遗忘经规则与模型审核 | 项目负责人明确选择该默认策略。Manual Approval仍逐工具批准，PLAN只读，显式deny与专用记忆根继续生效。读取、自动注入及模型写入分别配置，由DEV-084至DEV-089实现与验证。[全局记忆](memory.md) |
| D-053 | 2026-11-10 | 模型输出目标优先继承声明值，缺省32000，按剩余上下文动态收缩 | 项目负责人明确要求默认采用模型声明的limit.output、未声明时32000，并允许max_output_tokens覆盖。当前声明通过显式profile.limit.output提供，后续模型目录另行接入。执行、独立核查与Compact共用优先级，最具体请求／用途显式值优先于profile显式值，再到声明值与缺省。核查／摘要省略用途覆盖时继承实际所选profile，显式覆盖可高于声明默认。保留安全余量R=ceil(C/10)+1+extra，稳定输入基准B=C-R-1用于80%／55%水位，实际输出满足1≤O≤min(desired,C-R-I_wire)，最终重计并验证，允许少量额外保守余量。覆盖B-04、B-06、B-10与B-15在新开发线的输出默认及预算规则，0.1.0历史证据保留原归属。token目标与协议／JSON／工具字节、Run预算分别约束，最终验证见N3记录。[输出配置](configuration.md#模型输出-token-目标)、[上下文](context-management.md#完整请求预算) |
| D-054 | 2026-11-11 | 模型输出目标上限调整为1M，缺省回退调整为64K | 项目负责人明确要求默认使用模型声明的 `limit.output`，并结合适配器返回的可识别上游output limit；两者都不可用时回退65536，任何输出目标不得超过1000000。显式请求／用途值和profile显式值仍优先，按完整wire输入及上下文安全余量动态收缩。该决定覆盖D-053的32000缺省，历史N3证据保留原源码归属。[输出目标配置](configuration.md#模型输出-token-目标)、[模型协议](model-protocols.md) |
| D-055 | 2026-11-11 | TUI支持运行中引导、自动压缩阈值和结构化扩展选择 | 项目负责人要求运行中消息作为补充／引导而非替换当前Run，并新增 `/cd`、`/sandbox`、`/effort`、`/thinking`、`/model`、`/resume`、`/skills`、`/mcps`、`/plugins`、`/autocompact` 等交互。引导在下一模型边界持久化；自动压缩支持精确token水位及auto／off；选择器使用弹窗和结构化渲染。权限、PLAN、审批、凭据和扩展能力授权继续由原有下游边界执行。[TUI](tui.md)、[执行契约](execution.md) |
| D-056 | 2026-11-11 | TUI用户可见文本集中到单一目录文件 | 项目负责人要求统一维护并便于后续本地化。全部 `saya-tui` 显示文本集中到 `crates/saya-tui/src/i18n.rs`；命令语法、协议字段、内部状态键及稳定错误码保持独立，CLI、运行时提示词和公开协议诊断不在本次迁移范围。[TUI](tui.md#显示文本与本地化目录) |
| D-057 | 2026-11-11 | 审批改用弹窗，工具记录结构化呈现并新增`/ps` | 项目负责人指出审批详情侧栏与工具JSON难以阅读，并要求新增当前Run后台任务入口及完整UI预览场景。审批／澄清／`/ps`在对话主体居中弹窗；exec显示argv、目录、超时、观测耗时、退出码和输出；`/ps`只读当前Run事件，持久历史继续使用`/jobs`。`tui-preview`覆盖各弹窗与侧栏。[TUI](tui.md)、[设计](tui-design.md) |
| D-058 | 2026-11-11 | TUI粘贴文件使用编号token而非路径显示 | 项目负责人要求图片和文件粘贴后显示为高亮`[Image N]`／`File: [File N]`。Editor与历史只显示token，模型输入保留链接包装的真实本地路径；识别现有文件、`file://`、引号及转义路径。该入口不读取或内嵌剪贴板图片二进制。[TUI](tui.md) |
| D-059 | 2026-11-11 | 附件token原子编辑并支持剪贴板图片二进制 | 项目负责人要求附件占位符整体移动／删除，并支持直接粘贴剪贴板图片。Editor将`[Image N]`／`File: [File N]`作为原子片段；`Ctrl+V`优先从系统剪贴板读取PNG/TIFF等图片并保存到`~/.soyo/paste_image/`，随后插入Image token，无图片时回退文本。图片读取与文件写入保持32MiB、5秒及私有权限边界。[TUI](tui.md) |
| D-060 | 2026-11-11 | profile显式控制图片、音频与视频输入，DeepSeek测试profile启用图片 | 项目负责人要求多模态模型读取视频／音频／图片，并为当前DeepSeek profile启用其支持的图片能力。profile新增`multimodal.image/audio/video`；TUI增加Audio／Video原子token；Chat与Responses映射图片、音频及视频文件输入，Messages当前支持图片。未开启或不受支持的模态在发送前明确失败。本地忽略的`.soyo/local/deepseek.toml`仅启用image，能力开关进入profile身份指纹。[配置](configuration.md#多模态输入)、[模型协议](model-protocols.md#多模态内容) |
| D-061 | 2026-11-11 | 媒体输入使用SHA-256内容身份和OutputStore持久快照 | 项目负责人要求修复有界读取竞态及Session恢复一致性。新输入在落库前从同一打开文件描述符执行有界读取，保存带Session归属、内容摘要、租约及GC引用的二进制对象；后续请求和恢复不再读取原路径。旧版已经保存的纯路径历史继续兼容，但明确保留无法追溯验证原始内容的限制。[模型协议](model-protocols.md#多模态内容)、[上下文](context-management.md#长工具输出) |
| D-062 | 2026-11-12 | 模型总时限默认关闭，响应头300秒、流间隔900秒，超时进入有限间隔重试 | 项目负责人认为180秒总时限过短，明确要求无限总时限、服务器响应头300秒和相邻流数据块900秒。`model_timeout_seconds = 0` 只关闭单次总时限；取消、Run活跃预算、次数限制、工具及Compact独立超时继续生效。响应头／流空闲超时与暂时性连接、429、可重试5xx在尚无有效响应事件时最多重试2次，使用1／2秒退避和有限抖动，耗尽后返回最后错误；已收到部分响应时不重试拼接。更新B-15／B-16及[超时配置](configuration.md#模型超时与间隔重试)。 |
| D-063 | 2026-11-12 | TUI普通文件占位符统一为`[File N]` | 项目负责人要求移除拖入文件前额外的`File: `前缀，使图片、音频、视频和普通文件都直接显示单一编号token。原子移动、删除和发送时真实路径展开保持不变。[TUI](tui.md) |
| D-064 | 2026-11-12 | N4 Goal使用独立版本化存储、显式自动授权和证据驱动完成 | 项目负责人要求完成N4 Goal编排与验收。Goal绑定专属Session与工作区，通过独立SQLite WAL、revision CAS、lease与dispatch intent编排有界Run；自动继续必须显式`start --auto`并绑定配置指纹。Step完成、Run完成和验收证据分别记录，未知副作用不自动重放，只有全部验收项具有主机证据才完成。CLI与TUI共用GoalStore，TUI自动执行当前要求auto／bypass，Manual使用CLI审批。[Goal设计](goal.md)、[N4验收](n4-validation.md) |
| D-065 | 2026-11-12 | 启动/实施N5 DEV-079至DEV-083，PLAN扩展受限URL读取但禁止主动Browser | 项目负责人本次明确要求完成N5 Browser范围与DEV-079审查修复。PLAN在只读文件／搜索和人工交互之外，允许显式配置的受限`fetch_url`／`web_search`，但`browser_open`／`close`／`navigate`等归为`BrowserWrite`，`browser_snapshot`／`wait`／`screenshot`归为`BrowserRead`，两类在PLAN均拒绝；只读运行时不得注册或执行Browser工具。Web／Browser输出、取消、生命周期、输出捕获与Goal配置指纹按本次实现收敛。该记录不声称真实Chromium／Playwright worker或平台验收已完成，N5联合验收仍按后续任务取得证据。[Browser设计](browser.md)、[执行契约](execution.md)、[配置](configuration.md) |
| D-066 | 2026-11-13 | 扩展专用VM Browser后端，保留宿主与Chromium双层sandbox | 项目负责人明确选择扩展专用VM路径，而不是放宽Chromium sandbox。原生macOS Seatbelt无法在Chromium `sandbox_apply`重初始化时保持外层限制，Linux Bubblewrap的`setgroups=deny`使Chromium内层namespace初始化失败；因此新增显式`colima` VM后端，由VM内Chromium继续使用自有sandbox，宿主只通过严格SSH通道连接。生产配置必须显式选择VM，不自动回退native／Host；VM网络在DEV-080仍不是受控Browser出口，broker由DEV-081接入。现有native组件、失败边界和`browser_sandbox_unavailable`行为保持不变。该记录确认扩展范围，不声称真实VM平台验收已完成。[Browser设计](browser.md#dev-080-vm-后端扩展)、[隔离](sandbox.md)、[配置](configuration.md) |
| D-067 | 2026-11-13 | DEV-080审查整改收紧VM输入、断连清理与sandbox证据边界 | VM后端的canonicalize结果必须重新校验空白、控制字符与引号，回环host统一为裸IP字面量（`[::1]`→`::1`）；SSH断连或worker退出通过监视与Drop触发有界远端清理，无Tokio runtime时记录unknown；启动或清理失败保留可重试句柄，外层超时不得丢失connection/workspace；身份、prepare、sandbox与cleanup调用区分派发前`cancelled`与派发后`outcome_unknown`；PREPARE创建LAUNCH使用的全部XDG目录并保持0700；Python工具严格校验canonical路径与manifest结构。Linux browser seccomp的mount flags改按`seccomp_data` args[3]（offset 40）检查。native Chromium sandbox还必须取得自身`InitializeSandbox`或等价判别证据，Bubblewrap固定赋予的NoNewPrivs／Seccomp不能单独作为验收依据。随后专用Colima VM真实入口实际通过，但只覆盖该单环境；native浏览器、N5整体与四平台验收仍未完成。[Browser设计](browser.md#dev-080-vm-后端扩展)、[隔离](sandbox.md) |
| D-068 | 2026-11-14 | 扩展托管捕获引擎与固定身份契约，必要时采用定制 Chromium | 项目负责人明确确认这一扩展范围，以补齐普通 CDP 冻结无法保证的导航、绘制与截图事务屏障。后续实现需固定补丁源码、构建输入、各架构二进制及分发身份；保留预捕获遮蔽、宿主与 Chromium 双层 sandbox、网络／凭据／PLAN／审批及失败关闭要求，不向模型开放脚本、selector 或原始调试协议。该决定不表示引擎、生产截图或四平台验收已经完成；证据不足时截图继续禁用。[Browser 引擎扩展](browser.md#托管捕获引擎扩展d-068)、[N5 验收](n5-validation.md) |
| D-069 | 2026-11-17 | 收敛代码评审整改基线：保留默认沙箱与 `0.2.0` 版本号，增量提供显式工具链预设、Anthropic 缓存与 `edit_file` | 项目负责人明确确认：(1) 保留严格默认沙箱与默认拒绝网络，不默认挂载 `/proc`，通过 `sandbox.ca_certs` 与 `sandbox.toolchain_presets`（及内部 `synthetic_etc_identity`）显式按需启用工具链只读根与最小 CA/身份视图；(2) Anthropic `prompt_cache` 默认关闭以兼顾 DeepSeek 兼容端点，通过 `[profiles.<name>].prompt_cache` 或 `--anthropic-prompt-cache` 显式开启静态 `system`、末尾工具与至多 2 处滚动历史断点，保持不发送默认 `thinking` 参数及 `max_tokens` 返回 `LimitReached` 的原有行为；(3) 新增精确原文替换工具 `edit_file`，并使 `apply_patch` 在精确上下文与版本校验下容忍 hunk 计数与行号偏移；(4) 维持包版本 `0.2.0`。同期将符号链接真实路径仅用于权限策略匹配与授权绑定而保留 `argv[0]`、支持 review 围栏 JSON 提取、统一只读批并发与依赖 HTTP 客户端至 `reqwest` 0.13。[执行契约](execution.md)、[沙箱](sandbox.md)、[配置](configuration.md)、[模型协议](model-protocols.md) |
| D-070 | 2026-11-17 | 新增 `anthropic_official_provider` 模型配置开关以按 profile 全开启 Anthropic 官方能力 | 项目负责人进一步确认在 `[profiles.<name>]` 新增可选布尔开关 `anthropic_official_provider`（并用 serde alias 同时兼容 `anthropic_offical_provider` 拼写），默认 `false`，要求 `protocol = "anthropic-messages"` 且 `api_version = "2026-10-21"`；开启后：(1) 启用 4 断点内 Prompt Caching；(2) 默认随请求附带 `thinking: {"type": "adaptive", "display": "summarized"}`（可与 `thinking_effort` 的 `output_config.effort` 并存）；(3) 当执行期响应因 `max_tokens` 截断且仅有有效文本／thinking（不含不完整工具调用）时，通过 `ModelUpdate::OutputLimitTruncated` 在同一 `Run` 模型预算内采用末尾追加 `user` 续写消息（不使用已废弃的 assistant prefill）最多自动续写 3 次，半截工具调用、Review/Compaction 或超限仍以 `output_limit`／`LimitReached` 收口。[配置](configuration.md)、[模型协议](model-protocols.md) |
| D-071 | 2026-11-17 | 新增 `saya module` 子命令管理 `browser` 与 `vm` 运行时模块开关 | 项目负责人明确确认新增 `saya module [module_name] enable|disable|status` 与 `saya module list`，将浏览器（`browser`，对应 `browser.enabled`）和专用 VM 后端（`vm`，对应 `browser.vm.enabled`）纳入统一的 `module` 开关管理；显式开关写入 `data_dir/modules/modules.sqlite3`（WAL、0700 目录与 0600 文件、禁止 symlink 与非本用户访问），不改写用户的 `config.toml`；未显式覆盖时继续继承 `config.toml` 中的 `enabled` 字段（兼容现有 TOML 与测试夹具），已设置覆盖时优先于 TOML；只读加载、`config check` 与 `PLAN` 不自动新建数据库文件；`vm enable` 落盘前预校验 `[browser.vm]`、`[browser.network]` 与 D-066 同 `[browser.worker]` 互斥约束，`vm disable` 保持永不回退 native／Host；解析后的有效浏览器配置纳入 `goal_profile_fingerprint`。[使用指南](usage.md)、[配置](configuration.md)、[Browser 设计](browser.md) |
| D-072 | 2026-11-18 | 采用 Tauri 2 + in-process 架构构建独立安装的桌面端 `soyo-desktop`（React + Vite + TypeScript，NSIS／AppImage／macOS 安装包 + Ed25519 Updater 分发） | 确立 F-14 GUI 工程与安全契约：(1) **in-process 链接**：`desktop/src-tauri`（crate `soyo-desktop`）直接链接 `soyo-app` 与 `soyo-core`，不打包 `soyo` sidecar、不引入额外 IPC 序列化层；`crates/` 不存放 GUI 代码，根 `Cargo.toml` 通过 `default-members` 保持原有六个 crate 默认构建不变，`soyo-desktop` 仅经 `cargo tauri` 或 `-p soyo-desktop` 显式构建；(2) **独立安装与共享数据**：分别以 macOS `.dmg`/`.app`、Windows NSIS（不发 MSI）和 Linux AppImage 独立交付，与 CLI/TUI 共享同一 XDG 配置、SQLite 会话库、模块存储、OutputStore、全局记忆与宿主私有凭据存储；(3) **IPC 与前端栈**：请求／响应走 `#[tauri::command]`，Run 事件流走 `tauri::ipc::Channel`（不用全局 emit），审批与澄清实现 `ApprovalHandler`／`InputHandler` 经 Channel + oneshot 往返，TypeScript 类型由 `specta`／`tauri-specta` 生成；阻塞调用走 `soyo_app::offload_boundary`；前端采用 React + Vite + TypeScript、shadcn/ui + Radix + Tailwind、zustand，流式 Markdown 使用 Streamdown + Shiki + mermaid.js 并对照 [Markdown 说明](markdown-rendering.md)；(4) **安全边界**：收紧 `tauri.conf.json` CSP、禁远程 URL 与开放 `fs` asset scope，凭据仅走 `SecretRegistry` 与宿主私有存储（禁用 keyring 插件与 `localStorage`，前端只拿脱敏元数据），本地附件与产物预览通过 `register_uri_scheme_protocol` 注册的 `soyo-attachment://`（兼容 `saya-attachment://`）自定义协议限定于 OutputStore 与当前草稿；(5) **自动更新与 CI**：使用 `tauri-plugin-updater` + Ed25519 签名、`bundle.createUpdaterArtifacts: true`，由 `tauri-action` 生成 `latest.json` 并通过 GitHub Releases 分发，签名密钥与 Apple 公证凭据仅走 CI secrets。详见[桌面端设计](desktop.md)。 |
| D-073 | 2026-11-21 | 将 `soyo-desktop` 及 CLI/TUI 的统一后端从进程内链接迁移为共享 `soyo helper` 守护进程（Unix socket + NDJSON），`soyo-desktop` 不再进程内链接 `soyo-app` | 项目负责人确认一次性切换而非双模式：(1) **拓扑**：新增 `crates/soyo-ipc`（NDJSON 帧、`ClientFrame`/`ServerFrame` 协议信封、`HelperClient`、socket/端点解析、全部 IPC DTO）与 `crates/soyo-host`（`HostService` 运行时服务、`HelperServer` socket 监听与请求派发、附件存储与预览解析、用户级 prefs/全局指令文件）；原 `runtime_bridge`/`tool_format`/`redaction`/`types` 迁入两 crate，`soyo-desktop` 只保留 Tauri 命令薄封装、`soyo-attachment://` 协议代理与 Specta 绑定，前端命令面与 `bindings.ts` 不变；(2) **协议**：本地 socket 上逐行 JSON 帧——客户端 `Request{id,method,params}`/`CancelRequest`/`Shutdown`，服务端 `Hello{version,pid}`/`Event{id,data}`/`Response`/`Error{code,message}`/`Bye`，`PROTOCOL_VERSION=1`；Run 事件流复用 `DesktopStreamEnvelope` 逐条 `Event` 推送，审批/澄清仍经请求-响应闭环，大附件负载经 `fetch_attachment` 以 base64 返回；(3) **生命周期**：`soyo helper` 前台方式运行守护进程（客户端以 `soyo helper` 分离子进程自动拉起），`soyo kill` 经 socket `Shutdown` 帧优雅关闭并可回退 pid 文件 SIGTERM；CLI（TUI/chat/resume 入口）与 desktop 在首次运行时调用 `connect_or_spawn` 自动确保 helper 在线，无侵入式锁——socket 绑定即单例，过期 socket 文件在绑定时清理，`SOYO_HELPER_DIR` 供测试与多实例隔离；`SOYO_HELPER_BIN` 可覆盖拉起的目标二进制；(4) **取代条款**：D-072 第(1)条「in-process 链接、不引入 IPC 序列化层」由本决定取代，D-072 其余各条（独立安装与共享数据、`#[tauri::command]`/`Channel` 前端契约、安全边界、Updater 与 CI）继续有效；`soyo-attachment://` 的解析逻辑移入 helper，Tauri 侧仅作 URI 协议代理；(5) **范围说明**：本阶段 desktop 全量切换到 helper 后端；CLI/TUI 各自仍保留进程内 runtime 并负责拉起 helper，二者的 run 循环迁移到 socket 属后续阶段。[桌面端设计](desktop.md)、[架构](architecture.md) |
| D-074 | 2026-11-21 | CLI/TUI 全量迁移到 helper socket（`run_prompt`/`submit_prompt` 方法），保留 text/JSON/JSONL 输出、退出码与交互契约 | `soyo run` 经 `HelperClient` 调 `run_prompt` 流式方法执行，宿主侧复用同一 `HostService` 运行时路径。为保持现有输出契约逐字节兼容：(1) **原始事件直通**：`DesktopStreamEnvelope` 新增 `RawSchemaEvent{event: Value}` 变体（`#[specta(skip)]`，不进 TS union），`run_prompt` 流推送序列化后的 schema-v1 `Event`——`--output jsonl` 打印的每一行与进程内 `StdoutSink` 完全一致（含合成 `session.created`/`session.resumed` 首行与终止 `run.finished`）；`--output json`/text 从 `run.finished` 的 `data` 还原 `RunResult` 复用 `report_json`/`report_result`；边界错误时宿主补发 `emit_failure` 终止事件；`RunCompleted` 增加 `needs_input_question` 字段覆盖 text 模式的澄清问题输出。(2) **交互**：`interactive`（`soyo run --interactive`）经 socket 走 `ApprovalNeeded`/`InputNeeded` + `reply_approval`/`reply_input` 往返，`reply_input` 支持 `null` 表示拒答；非交互时不挂交互句柄，运行时自身产出 `needs_confirmation`/`needs_input`，与进程内行为一致。(3) **边界与回退**：`--cwd`/`--profile`/`--mode`/`--permissions`/`--approval-file`/个人上下文/Anthropic prompt cache 作为 `RunPromptParams` 逐次应用、不改写共享守护配置；`--config`/`--data-dir` 无法跨 socket（helper 持有自身配置）时以及 helper 无法连接/拉起、run 未开始即被拒绝时，回退进程内路径并打印提示——保留旧版 standalone 语义而非新增硬依赖。SIGINT/SIGTERM 经 `cancel_run` 方法转发，退出码映射不变（0/1/3/4/5/130）。后续阶段已覆盖：`soyo chat`/`soyo resume` 交互循环经 `submit_prompt`/`SubmitPromptParams`（`session_id` 钉住、`raw_events` 直通、`capture_media`、`personal_context`/`anthropic_prompt_cache` 逐次覆盖）驱动；TUI 经 `controller/backend.rs` 的 `SessionBackend`/`ActiveHandle` 双态适配接入同一 helper——`RemotePrepared` 镜像 helper 会话、`RemoteHandle` 以 `RemoteRunState` 缓存模式/压缩态并把所有 `RuntimeHandle` 操作映射为 IPC，`spawn_remote_run` 把 `DesktopStreamEnvelope` 翻译回 `Active` 通道；`--config`/`--data-dir` 关闭 helper 走本地后端。[桌面端设计](desktop.md)、[架构](architecture.md) |


完整基线是当前产品行为的权威入口，文末覆盖表追踪所有已收敛主题。实现中的函数签名、内部拆分和兼容依赖版本由开发者处理，不重新提出偏好问题。

改变已确认范围、默认值或公共契约时，说明受影响的 B 编号并新增决策；保留旧记录。外部服务可用性、性能和平台支持需通过实施验证取得证据，不能用“已定案”替代验证。

## 更新规则

- 关键选择确认后，记录日期、理由和影响，并同步涉及的文档。
- 改变既有决策时保留原有记录，注明被什么新决策取代。
- 不为每个实现细节创建记录；较复杂的决策可以拆成独立文档并在此链接。
- 不将 AI 生成的建议自动标记为项目负责人已确认。
