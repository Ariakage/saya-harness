# 开发进度

**更新于2026-11-11。N3上下文、可执行扩展及全局记忆已完成四平台验收，固定源码为 `e1da373`。四端均通过完整Rust门禁、原生隔离、性能v2、MCP／OAuth、8项真实插件、34条CLI、15条TUI和历史入口回归，程序SHA、源码清单及执行前后身份一致。N2固定源码 `aaec8c5` 的四平台证据与已发布0.2.0历史证据继续按各自源码保留。**

本文件是开发状态的唯一记录入口。[路线图](roadmap.md)定义阶段，[实施任务](implementation-plan.md)定义任务，[完整基线](mvp-baseline.md)定义产品行为。状态更新需要实际证据，不根据计划或估算自动推进。

## TUI 运行控制、引导与输出边界

2026-11-11，在运行时控制和扩展选择阶段后继续加入TUI运行中引导、`/cd`、`/sandbox`、`/model`、`/effort`／`/thinking`参数补全、`/autocompact`、`/skills`／`/mcps`／`/plugins`选择器、`/sessions`弹窗恢复和结构化`/tools`卡片。运行中普通输入最多16条／累计256KiB，排队到下一模型边界持久化；Run关闭后拒绝迟到输入。MCP切换只影响后续Run，Skill／Plugin选择不绕过原有工具和启用授权，`/sandbox off`仍保留权限、deny、PLAN、凭据存储拒绝和Host能力限制。

模型输出目标改为1000000硬上限：优先显式请求／用途值、profile显式值、`limit.output`声明值和适配器可识别的上游output limit，未声明时回退65536；实际dispatch继续按完整wire输入和上下文安全余量收缩。本地忽略配置 `.saya/local/deepseek.toml` 的三个DeepSeek测试profile均设为 `max_output_tokens = 1000000`，未加入Git。

本机macOS27.2 ARM执行 `./scripts/check.sh`，格式、workspace全目标严格Clippy、1333项Rust测试、8项条件忽略测试和release构建通过；其中TUI lib 263项、core lib 436项。日志为 `target/final-check.log`。`just smoke target/release/saya` 的离线smoke、文本journey、5项审查回归和完整正常入口PTY矩阵全部通过。证据为本轮本地运行；未推送、未运行远端CI，程序哈希和平台四端候选证据未另行更新。

同日补充修正Thinking与Context呈现：工具调用展开改为字段和递归输出渲染，不再直接展示整段JSON，摘要仍由独立字段保存并保持折叠状态颜色；静态已知凭据下的Thinking使用增量安全预览，完整凭据及跨分片前缀显示为`[REDACTED]`，动态凭据轮换仍等待请求结束；会话恢复从最近RunMetrics恢复`Last run`输入计量，`/compact`与`/cd`保留此前计量直到下一次Execution，请求已开始但usage未返回时显示`Awaiting execution usage`。同类状态审计确认`/new`与`/profile`清空属于新会话语义。新增恢复、Compact、工具渲染、Thinking前缀脱敏、摘要折叠和状态回归。最终 `./scripts/check.sh` 通过格式、严格Clippy、1337项Rust测试、8项条件忽略和release构建，日志为 `target/final-check-4.log`；`just smoke` 正常入口PTY矩阵全部通过。

## 多模态图片、音频与视频输入

2026-11-11，按D-060为profile增加显式`multimodal.image/audio/video`能力。TUI将本地音频和视频分别显示为`[Audio N]`与`[Video N]`原子token；发送时图片、音频和视频从有界本地文件读取并映射为Chat／Responses content parts，Messages当前发送base64图片。能力关闭、文件不可读、格式不受支持或超限均在HTTP请求前明确失败，普通File继续作为路径文本交给模型和工具。媒体载荷使用有界Token估算，能力开关进入profile身份指纹。

本地忽略配置 `.saya/local/deepseek.toml` 的三个DeepSeek测试profile仅开启`image = true`，仍保持`max_output_tokens = 1000000`且未加入Git。`saya config check`通过；真实`deepseek-responses`单次图片请求正确识别本机生成的空白文档图标。临时图片、数据库和会话目录已清理。

本机macOS27.2 ARM执行 `RUST_TEST_THREADS=1 ./scripts/check.sh`，格式、workspace全目标严格Clippy、1368项Rust测试、8项条件忽略和release构建通过，日志为 `target/multimodal-check.log`。新增真实HTTP夹具覆盖Chat图片／音频／视频、Responses图片／音频／视频及Messages图片wire；`just test-tooling`的75项Python回归通过。本轮未推送、未运行远端CI。

同日按交互反馈将音频、视频显示token由`Audio: [Audio N]`／`Video: [Video N]`收紧为与图片一致的`[Audio N]`／`[Video N]`，保持媒体类型高亮、整token移动删除和发送时路径展开。定向附件与渲染回归通过；完整门禁再次通过1369项Rust测试、8项条件忽略、严格Clippy和release构建，日志为 `target/audio-video-token-check.log`。

## 完整模型提示词预览

2026-11-11，新增 `just prompt [work|plan|code|build|compact|memory|review|all]`，省略类型时输出 `all`。预览复用实际静态装配器、动态系统上下文、RuntimeContext、项目发现、Memory与Compact／Review提示词，并输出完整 `ModelRequest` 风格的 `instructions`、`dynamic_instructions`、有序 `history`、`combined_system`、指纹和provider工具schema。Code／Build／Work／Plan场景包含后台、Skill、Memory、MCP与Plugin工具层；当前工作区和HOME的项目上下文为实际发现，未启用扩展与已激活Skill正文使用带`fixture_notice`的明确样例，全程不调用模型或执行工具。

`just prompt code`、`build`、`compact`、`memory`及默认`all`均以JSON解析和`jq`字段断言核对模式段、动态注入、项目／Skill／Memory数据、25个工具定义及独立用途。本机执行 `RUST_TEST_THREADS=1 ./scripts/check.sh`，格式、workspace全目标严格Clippy、1369项Rust测试、8项条件忽略和release构建通过，日志为 `target/prompt-preview-check.log`。本轮未推送、未运行远端CI。

## TUI 显示文本目录

2026-11-11，按D-056将全部TUI用户可见显示文本集中到 `crates/saya-tui/src/i18n.rs`，覆盖命令说明、状态和角色、侧栏、MCP状态、选择器、工具展开、Memory、执行摘要、Markdown／数学／表格回退及终端错误。命令语法、协议字段、内部状态键和稳定错误码保持独立；CLI输出、运行时提示词和公开协议诊断不进入本次目录。窗口标题继续使用原有 `Saya | …` 契约。

最终 `./scripts/check.sh` 通过格式、workspace全目标严格Clippy、1337项Rust测试、8项条件忽略和release构建，日志为 `target/i18n-check.log`。同一release程序的 `just smoke target/release/saya` 通过离线smoke、文本journey、5项审查回归及26组TUI PTY旅程。本轮未推送、未运行远端CI。

## TUI 审批弹窗、工具呈现与 `/ps`

2026-11-11，按D-057把审批与澄清详情从普通侧栏改为对话主体内的居中圆角弹窗，保留底部草稿输入和命令补全优先级。审批参数复用工具呈现器，`exec`显示argv、工作目录、环境变量名与超时，并隐藏内部`__saya_*`分析字段；完成结果展示观测耗时、退出码、stdout、stderr、超时与取消状态。`/ps`从当前Run的后台事件重建任务列表并区分running、cancelling、completed、failed和cancelled，持久历史仍由`/jobs`提供。

`tui-preview`新增question、background、help、memory及Skills／MCP／Plugins／Sessions场景，原有approval与tool场景同步采用弹窗和结构化字段。实际生成并检查了全部弹窗／侧栏场景；新增Rust回归覆盖exec参数、实时摘要、弹窗边界、预览覆盖和当前Run后台任务状态。

最终 `./scripts/check.sh` 通过格式、workspace全目标严格Clippy、1345项Rust测试、8项条件忽略和release构建，日志为 `target/ui-review-final-2.log`。`just test-tooling`的75项Python回归通过；同一release程序的 `just smoke target/release/saya` 通过离线smoke、文本journey、5项审查回归及26组TUI PTY旅程，其中`/ps`在真实后台任务运行期间完成弹窗读取与关闭验证。本轮未推送、未运行远端CI。

## TUI 图片与文件粘贴token

2026-11-11，按D-058／D-059将终端bracketed paste、拖入和剪贴板图片改为编号显示token。图片显示蓝色`[Image N]`，其他文件显示绿色`File: [File N]`；Editor、发送后的用户消息、恢复历史、窗口标题和新增`attachments`预览均不再暴露绝对路径。发送时token展开为供模型读取的`[Image N](<PATH>)`链接形式，并支持`file://`、引号、反斜杠空格、多文件及路径redaction。附件token是Editor中的原子片段，左右键跨过整个附件，内部或边界删除移除完整token。

`Ctrl+V`优先读取系统剪贴板图片二进制，PNG不可用时尝试TIFF；图片以0600权限保存到`~/.saya/paste_image/`，目录为0700，读取上限32MiB且有5秒异步期限，无图片时回退文本。Linux实现使用Wayland `wl-paste`或X11 `xclip`。本机macOS剪贴板中的真实PNG在隔离临时HOME内完成保存与`[Image 1]`插入验证。

最终 `RUST_TEST_THREADS=1 ./scripts/check.sh` 通过格式、workspace全目标严格Clippy、1356项Rust测试、8项条件忽略和release构建，日志为 `target/atomic-clipboard-final.log`。`just test-tooling`的75项Python回归通过；同一release程序的 `just smoke target/release/saya` 通过离线smoke、文本journey、5项审查回归及27组TUI PTY旅程，其中真实粘贴双文件场景额外验证Backspace完整删除File token、Left跨过附件原子边界、Delete完整移除附件，以及模型prompt中的真实路径。本轮未推送、未运行远端CI。

## N3 上下文与可执行扩展

2026-11-09，项目负责人启动完成N3及验证验收的持续目标，起点为 `7064bb7`，启动时工作区干净。范围包含DEV-062至DEV-068、DEV-076至DEV-078、DEV-084至DEV-089共16项；2026-11-10，16项均由固定源码 `e1da373` 的四平台验收完成。[逐项验收记录](n3-validation.md#四平台最终验收)

DEV-078实现manifest／lock的离线结构、语义、SemVer、feature／能力、依赖、树摘要和工具别名校验。包文件采用有界NOFOLLOW读取并密封内容，Unicode NFC与完整大小写折叠用于碰撞检查。独立复核的两处缺口已修复，13项插件回归及本机完整门禁通过，共942项Rust测试，包含格式、严格Clippy和优化构建，日志为 `target/n3/dev078-workspace.log`。[规范与实现边界](saya-plugin.md#验收与实现状态)

2026-11-10，DEV-076核心链路及DEV-062预算基础接入共享运行入口。exec保留原有效捕获上限并继续排空，MCP在模型投影前捕获解码文本和结构化内容，二进制内容用省略描述记录。宿主引用与工具结果在同一SQLite事务登记，发布失败保留已观察结果，恢复完成发布，运行租约与会话ACL接入。output_read／output_search经过Manual Approval、PLAN及显式deny检查。[配置与上限](configuration.md#输出捕获与上下文预算)

本机macOS27.2 ARM执行 `RUST_TEST_THREADS=1 just check`，格式、全workspace全目标严格Clippy、986项Rust测试与优化构建通过。新增17项OutputStore、19项ContextBuilder、5项运行时边界、1项三协议wire计数及1项MCP长输出回归。计数测试核对真实HTTP请求体，并确认计数期间没有请求。发布故障回归核对副作用只发生一次。日志见 `target/n3/dev076-062-workspace.log`。独立复核后继续修复结果投影状态保留与单用户长任务切点，DEV-062保持验收中，自动／手动Compact和全局记忆正常入口继续开发。

DEV-084／087的GlobalMemoryStore独立组件新增可读Markdown、事实粒度来源、当前workspace过滤、纯提案、幂等提交、排他锁、WAL恢复、私有ledger及索引历史。33项定向测试通过，包含源元数据和旧updated_at在forget后经旧索引／rollback抑制的回归。独立复核确认修复，入口／投影／界面由后续任务接线。[组件格式与保证](memory.md#dev-084087-存储组件)

项目负责人提供四份压缩／续接／系统组装参考，以及三份记忆参考，要求提示词使用纯英文。已形成英文Compact与Continuation草案并记录四份压缩来源的SHA和取舍，当前源码已接入14份英文提示词，包含结构化Compact、固定前缀条件式续接与有来源的Memory Context，协议与正常运行验收继续按DEV-063／077维护。[提示词来源与接入契约](prompt/drafts/n3/README.md)

三份记忆来源进一步形成英文Memory Context与Consolidation草案，记录默认注入、渐进检索、来源／作用域／版本、更正、撤回及明确遗忘。参考中的多版本模板和二进制残留按来源身份保留，已将Saya专用记忆工具、证据选择、更正／遗忘与独立核查契约接入英文工具／审核提示词，独立整理器仍保留草案，实际入口与平台证据继续按DEV-084至DEV-089维护。[记忆来源与验收矩阵](prompt/drafts/n3/memory-sources.md)

全局记忆默认读取和注入，auto写入／遗忘通过规则与独立模型审核，由本轮明确选择D-052确认。格式、索引、工具、遗忘、界面及联合验收分别由DEV-084至DEV-089交付。

## TUI 拖尾与选文复制

2026-11-09，在 `830910c` 基线上将往返动画扩为八格、14帧循环，主方块带最近三帧的 `▓▒░` 衰减拖尾，弱化亮度并按实际模式着色。启动与折返重叠、窄窗和终态均有回归。

对话与工具／审批详情支持鼠标拖选、高亮、Ctrl+C复制及Esc清选区。复制从可见单元格提取完整Unicode字符，以视觉换行组织文本。选区外更新可继续保留高亮，内容变化、滚动和缩放会清除选区。正常TUI与debug预览共用交互，1002拖动跟踪与退出恢复已接入。OSC52复制请求复用已有Base64 0.22.1，最终文本限64KiB，终端负责按自身策略写入剪贴板。[交互与终端要求](tui-design.md#编辑与滚动)

本机macOS27.2 ARM执行 `RUST_TEST_THREADS=1 just check`，格式、workspace全目标严格Clippy、929项Rust测试及优化构建通过，`just test-tooling`的51项Python回归通过。同一固定优化程序完成26组正常入口PTY，新增场景验证中文宽字右半格、组合字符、详情复制、草稿保留、运行中复制、滚动／缩放清区和终端还原。新增Rust回归另验证复制与Esc清区保留待审批状态。四模式均观察到八格往返和三级尾迹，debug预览亦通过动态拖尾、复制请求及终端恢复，模型请求为0。独立源码复核通过。

程序SHA-256为 `17a53d8706b500042400d87d29761cce4d9b6a9989af4bd494c2a96ef65dbe8e`，前后一致，日志、坐标、复制请求和验证报告位于 `target/tui-trail-selection/`。复制验收在PTY中核对OSC52字节及UTF-8解码，实际系统剪贴板接收由用户终端支持和授权决定。本轮证据限于本机受控环境，以下保留前一版动画及输入调整的历史记录。

## TUI 方块往返动画

2026-11-09，按项目负责人补充要求，在 `e8e90f1` 基线上将运行动画调整为单格方块在六列区域内从右向左、再折返回右端。每120ms推进一帧，10帧循环。已有绘制测试覆盖完整往返序列、四模式配色和窄窗状态／权限优先，真实PTY新增有序坐标采样并验证两端及双向位移。

本机执行 `RUST_TEST_THREADS=1 just check`，格式、workspace全目标严格Clippy、924项Rust测试及优化构建通过。四模式真实PTY运行反馈场景通过，包含完成统计、未知用量、模型历史与终端还原检查。固定程序SHA-256为 `426a1463ed0aae27cd7485edfa3909262d1906f7713fd48c277a952571cba92d`，前后一致，日志与坐标报告位于 `target/tui-activity-bounce/`。首轮四线程检查的三项CLI等待超时记录保留在 `check-initial.log`，串行完整复验及本轮实际程序分别见 `check.log`、`feedback.json`。以下保留输入与运行反馈初版的验收记录。

## TUI 输入细节与运行反馈

2026-11-09，在 `c2bf78c` 基线上调整Message输入区。四模式色条使用单列占位的三分之一宽字形，保留左内边距与文字起点，灰底右侧增加一列深色间距。运行状态增加120ms、12帧、六列的模式色动画，等待人工输入／批准及结束时隐藏，窄窗优先保留状态和权限。

每次任务结束追加模式、模型、耗时、平均输出速率和总token统计。计时在事件发布端采样，速率按Execution模型区间统计，总量取最终RunResult并包含核查。缺失用量显示 `—`，估算带 `~`。统计经过动态凭据脱敏，仅作为当前TUI显示消息保留。工作模式切换、取消、失败、未知／估算用量、溢出、延迟事件消费和窄窗均有回归。[统计口径与预览](tui-design.md#运行反馈与结束统计)

本机macOS27.2 ARM／Rust1.99.1执行 `RUST_TEST_THREADS=4 just check`，格式、全workspace严格Clippy、924项Rust测试及优化构建通过。`just test-tooling`的七脚本共50项Python回归通过，系统Python3.9.6另行验证新字形占一列且中文仍占两列。新增working／completed预览，160×45与80×24实际单元格快照经本机字体绘制检查，debug预览真实PTY确认四种动画帧、场景切换、完成行和终端恢复，模型请求数为0。

同一固定优化程序完成25组真实PTY旅程，包括新增四模式运行反馈、三协议、审批、恢复、后台取消、长输出和终端还原。会话测试曾匹配到局部重绘中的旧通知，已改为等待实际Ready和完整画面后检查新会话，原会话数量与不重放断言保留，定向连续三轮及最终完整旅程通过。[程序与验证记录](tui-validation.md#2026-11-09-输入与运行反馈)

本次验证限于本机和受控服务，未推送或发布。

## Just 开发入口

2026-11-09，根目录新增 `justfile`，提供13个开发任务，包含默认任务列表、完整检查、格式、Clippy、测试、开发／优化构建、CLI运行、debug预览、离线快照、Python工具回归及显式程序smoke。`just check`复用已有 `scripts/check.sh`。构建默认使用macOS15.0部署目标和line-tables-only开发调试信息，显式环境设置优先。Agent约定与[开发指南](development.md#just-开发入口)同步采用Just入口。

本机macOS27.2 ARM／Rust1.99.1／Just1.59.0执行 `RUST_TEST_THREADS=4 just check`，格式、全workspace严格Clippy、909项Rust测试及优化构建通过。`just test-tooling`的七脚本共49项Python回归通过。CLI帮助、debug预览帮助、140×60语法场景JSON快照和精确过滤测试均经实际recipe验证，显式程序smoke通过CLI、文本会话、审查回归与24组TUI旅程。所测优化程序SHA-256为 `d2b0f84585ac0e66cf8953547bd9eac8b36cbd7e21b28024c0da469a8aa9479e`，前后一致。

20项入口边界检查覆盖零参数、选项、空格、引号、Unicode、空参数、shell字面字符、环境覆盖、子目录工作目录、失败码与停止顺序。smoke强制显式程序路径，四脚本接收同一原值，自定义Cargo输出配置可由调用者选择实际程序。独立复核通过，验证记录位于 `target/just-validation/`。

## N2 隔离与 MCP

2026-11-09，项目负责人启动完成N2及对应验收的目标。范围为DEV-054至DEV-061及DEV-075，依赖和退出条件沿用[后续开发计划](post-mvp-plan.md#n2-隔离与-mcp)。本轮起点为 `547e5fe`，工作区当时干净。

DEV-054至DEV-061及DEV-075均已完成。工具注册、可信操作描述、CST事实、独立原生后端、受控网络、MCP与凭据管理已接入正常CLI／文本会话／TUI。默认原生隔离按D-050选择Seatbelt／Bubblewrap，必需约束无法满足时明确失败，后端选择与权限模式分别处理。

最终源码 `aaec8c5b81ddb464320fd46f6dc6dd00a0a547f2` 的[四平台CI](https://github.com/Ariakage/saya-harness/actions/runs/42607697386)与[N2原生验收](https://github.com/Ariakage/saya-harness/actions/runs/42607698976)全部通过。Linux x86_64／aarch64各923项Rust、19项原生、25项MCP和24项TUI，macOS15 Intel／ARM各907项Rust、18项原生、25项MCP和24项TUI。四端完整性能v2各3样本的21项判据、源码双清单、实际平台／架构、前后程序哈希与保留副本均经下载后独立核验。Linux内核通道回归、系统别名遮罩与委派cgroup正向及四项负对照通过。

完整证据、程序SHA和测量边界见[N2最终验收](n2-validation.md#最终四平台验收)，本地索引为 `target/n2/remote-review/aaec8c5b/monitoring-complete.json`。下一阶段N3按依赖从DEV-076 OutputStore进入DEV-062 Context Builder。首个凭据后端为私有明文文件，macOS后代清理为best_effort，资源耗尽压力与外部厂商互操作保持各自未测范围。以下N2各节保留阶段历史及当时的待验范围，以本段与最终证据为当前状态。

### 验收归档后的输入夹具同步

归档提交 `6da36ac` 的CI在macOS ARM完整termios检查发现PENDIN采样竞态，原400ms回收断言通过。独立12项真实PTY对照确认子进程报告发布与后续canonical read之间的内核状态变化。夹具现先完成一次有界空读取并恢复原文件标志，再发布报告。完整状态比较与原时间边界继续执行，新增延迟读取屏障及ECHO错误配置负对照。受控移除新同步时回归失败，修复后10项input测试连续3轮通过，独立复核通过。[规范模式采样证据](n2-validation.md#归档ci的规范模式采样)

本机4线程完整门禁通过909项Rust测试、格式、workspace严格Clippy和优化构建，日志为 `target/n2/input-snapshot-workspace.log`。优化程序仍与已验收本机镜像逐字节一致，固定N2程序证据与测试夹具后续复验分别维护。

### DEV-054与DEV-075阶段历史

DEV-054的隔离源码快照在本机macOS27.2 ARM／Rust1.99.1执行完整 `MACOSX_DEPLOYMENT_TARGET=15.0 ./scripts/check.sh`，格式、workspace全目标严格Clippy、752项Rust测试及优化构建通过。六项新回归包含48组分类／模式／权限组合、离线与有界schema、身份指纹、元数据凭据拒绝、一次性派发、持久化期间目录替换及执行租约、未知副作用和认证／输入请求终止。独立复核确认的目录竞态、嵌套schema资源重定向和目录凭据边界均补入回归。证据为 `target/n2/dev054-workspace.log` 和 `target/n2/dev054-tests.log`，实现约定见[工具注册](tool-registry.md)。CLI中的MCP连接与真实后端接线由后续DEV阶段完成。

DEV-075采用Tree-sitter0.27.1／Bash0.25.1，直接argv保持字面含义，显式shell通过真实CST提取结构。一次prepare取得实际过滤环境名称，事实进入独立核查数据，批准绑定稳定版本／限额及原有完整执行身份。时间相关解析状态留在事实层。结构分析19项定向测试、5008组随机／预算探针通过，最终隔离源码快照完成格式、workspace严格Clippy、773项Rust测试及优化构建。新增实际审核凭据重构屏障回归通过，模型请求数为0。源码、预算与边界见[命令分析](command-analysis.md)，证据为 `target/dev075-probe/probe.json`、`target/n2/dev075-{workspace,credential-facts}.log`。

### DEV-055 执行组件

统一环境准备／能力／策略身份、独立原生后端、私有launcher与文件RPC、共享进程监督已完成组件开发。六种本地文件工具可经显式准备环境运行，保存意图、两次许可检查和未知观测保持在宿主。隔离源码快照完成803项Rust测试及完整门禁，日志为 `target/n2/dev055-workspace-final2.log`。本机原生结果与跨平台类型检查分别记录，正常CLI默认策略和运行期服务继续整合，详见[N2验收](n2-validation.md)。

已推送的DEV-054／075源码 `21a3f6e` 四平台CI全部通过，后续原生／MCP整合将使用自己的程序与源码证据。

### DEV-056 网络与原生约束组件

每Run受控HTTP／CONNECT broker、双栈监听器占有、DNS固定与有界解析／转发通过18项组件回归，原生组件快照完成823项Rust测试及完整门禁。macOS已收紧代理TCP与宿主进程信息边界并验证标准根symlink解析。Linux原生CI发现Tokio信号驱动的匿名stream pair被Seccomp阻止，已完成精确放行与防重定向候选，实际Linux复验继续进行。记录见[N2验收](n2-validation.md)。

### 人工审批终端背压

MCP长启动审批通过正常PTY复现终端非阻塞写入的部分完成／WouldBlock失败。终端写入现保留offset，遇背压让出执行并响应取消。独立PTY填满夹具在旧实现返回terminal_error，修复后七项CLI bin测试与全测试目标严格Clippy通过，证据为 `target/n2/approval-backpressure-{before,after}.log`。正常MCP长审批随后在固定debug程序完成两次人工批准并通过任务。

### DEV-058 MCP 连接组件

核心MCP连接、工具投影和必要认证组件已完成隔离源码门禁，873项Rust测试、workspace严格Clippy和优化构建通过。stdio使用宿主提供的受控管道，HTTP请求／结果有界且工具发送一次；私有凭据、撤销、OAuth校验与资源绑定有定向回归。公共运行时与CLI／TUI接线继续按DEV-059至061整合，阶段证据见[N2验收](n2-validation.md)。

### DEV-059 运行时与界面整合

正常 CLI、文本会话与 TUI 已接入默认原生后端、六种隔离文件工具、受控 MCP 启动、统一注册表与目录代次、共享活跃预算及动态凭据脱敏。PLAN 切换先完成本地 MCP 与相关副作用清理，恢复沿用本次配置且不重放历史工具。TUI 使用类型化宿主事件显示真实连接状态和工具数。

整合工作区在本机 macOS27.2 ARM 完成格式、workspace 严格 Clippy、904项 Rust 测试与优化构建，日志为 `target/n2/integration-workspace-5.log`。DEV-059 独立提交快照完成901项Rust测试与完整门禁，日志为 `target/n2/dev059-workspace-final.log`。实际 Linux 与 macOS15 两架构正常入口验收继续进行，此处保留平台未验收状态。

### DEV-060 凭据管理入口

新增显式 MCP 凭据导入、元数据列表、撤销与交互 OAuth 登录命令，首个后端为 0700／0600 私有明文文件。服务、精确 endpoint、scope 与 OAuth issuer／canonical resource 绑定引用，轮换和撤销复用运行期凭据集合。管理审批、URL 显示和浏览器导航前检查已知凭据及有界编码视图，导入来源在审批前加入检查。

10项 CLI bin 测试、格式及 CLI 全目标严格 Clippy 通过，日志为 `target/n2/dev060-{admin-tests,clippy}.log`。当前 Rust 源码清单与904项完整门禁及固定优化程序一致，独立复核的原始失败与修复证据保留于 `target/n2/independent-review/`。最终四平台凭据与 OAuth 证据继续按 DEV-061 验收。

### DEV-057 原生验收脚本与工作流

新增普通 CLI 原生对抗与性能脚本、Linux 显式委派子树包装及四平台固定源码验收工作流。脚本验证六种文件工具、PLAN、deny／bypass、保护路径、Skill、网络与代理旁路、宿主进程信息、取消、崩溃后恢复及资源能力。性能样本分别检查实际后端与工具成功，记录时间和RSS测量边界。Linux委派用例核查内核限制值、活动成员与自有组清理。

固定优化程序 `8f7810773f90b81b91d3ee7a4916ced17e15ef945b4050f96a3210c2d4f15585` 在本机macOS27.2 ARM通过18项原生用例及每variant三次性能样本，最终脚本记录于 `target/n2/final-local/native-reviewed-evidence.json`。8项Python回归与工作流／Shell语法检查通过。Linux委派cgroup本机未运行，四平台工作流待执行。

### DEV-061 MCP／OAuth 联合验收

新增永久正常 CLI／PTY／TUI 验收脚本，覆盖三模型协议各自的stdio／HTTP工具往返、JSONL脱敏、真实服务状态、Manual Approval、PLAN、连接／目录共享活跃预算、长结果、失效schema与目录、认证／输入请求、断连unknown、恢复单次派发，以及交互OAuth／刷新／撤销。

同一固定优化程序在本机macOS27.2 ARM通过25／25项，报告为 `target/n2/final-local/mcp/mcp-report.json`，运行前后程序SHA一致。真实TUI从Configured进入Connected／HTTP／2工具，结束后Stopped／0工具，SQLite状态同Run配对且终端恢复通过。四平台N2工作流同时执行本脚本与原生、Markdown／TUI验收，最终源码结果待运行。

### Linux 编译夹具兼容修复

最终源码 `97b40f9` 的首次四平台验收在Linux两架构复现Rust编译器动态库定位失败，BUILD／CODE夹具的工具结果为exit127。工具链可读根与`--sysroot`已配置，当前修复显式在隔离内的编译命令设置工具链`lib`路径。保留原生`/proc`策略，Linux实际复验继续进行，失败日志在 `target/n2/remote-review/linux-{arm,x86}-job-*.log`。

### Linux Rust fork路径与匿名通道修复

候选现精确允许STREAM／SEQPACKET已连接匿名Unix pair，保留协议／flags检查及命名socket、datagram限制。Linux准备先校验可确认的正式内核范围，再以私有同型监听器核对shutdown、AF_UNSPEC及peer-close后的重连接／监听语义。有限探测与已知修复身份共同约束能力，无法确认时失败关闭。

新增Rust带pre_exec的真实fork子进程回归、STREAM／SEQPACKET重定向和已有FD传递边界。当前代码在本机完成904项Rust测试和完整门禁，两Linux架构交叉全目标严格Clippy及21个内核版本样本通过，证据为 `target/n2/seqpacket-workspace.log` 与 `target/n2/research/seqpacket-*.log`。Mac优化程序仍与已验证固定程序逐字节相同，Linux实际结果待新runner验证。

### 原生依赖下载兜底

`2b353dc2` 的Linux ARM验收及源码CI的Linux任务在Bubblewrap官方下载连续HTTP504后终止，尚未进入产品测试。安装脚本保留固定0.13.1与原SHA-256，主地址失败后使用同一官方release asset API，连接／下载时长及重试均有界。真实API下载129472字节与原摘要一致，四项离线回归覆盖成功、兜底、错误摘要与双失败，CI及N2工作流均接入检查。

### Linux系统工具别名与保护路径

`2b353dc2` 的Linux x86复验进入链接器创建后返回`cc`路径ENOENT，宿主Meson已能识别同一编译器。候选将存在的 `/etc/alternatives` 纳入只读系统视图，并记录实际宿主链接链供Linux回归核对。独立源码审查还确认原系统目录独立挂载会产生额外guest别名，现将每个保护路径映射到全部固定系统根，后置遮罩去重并覆盖受保护祖先。

新增原生回归覆盖系统编译器别名、只读视图、whole-alternatives保护及 `/usr/bin/readlink`／`/bin/readlink` 双路径拒绝。本机适用core测试与严格Clippy、两Linux目标独立harness严格Clippy及2项实际映射函数测试通过，最终Linux行为待runner。证据位于 `target/n2/research/alternatives-*`。

当前候选在本机完成904项Rust测试、格式、workspace全目标严格Clippy与优化构建，日志为 `target/n2/alternatives-workspace.log`。Mac程序与既有完整本机验收程序逐字节一致，Linux源码变更继续由原生runner验证。

### x86 GNU Linux链接器夹具

`340499db` 的ARM12项acceptance已全部通过，x86已能创建cc子进程，随后默认LLD wrapper因依赖 `/proc/self/exe` 退出。编译夹具仅对Linux x86_64 GNU显式设置 `-C linker-features=-lld`，保留其他目标命令与原生proc限制。本机12项acceptance、格式与定向严格Clippy通过，Rust1.99.1实际接受目标参数。真实Linux链接结果待runner。

### 原生准备后的模型就绪同步

`340499db` 的ARM三项chat失败发生于3／4秒的模型进入信号等待阶段，尚未到取消、模式切换或后台清理断言。PTY夹具现使用有界10秒等待并持续排空终端，子进程提前退出时报告状态与记录。单次命令取消测试也采用10秒就绪等待，并在准备失败时回收子进程与保留输出，覆盖此前本机快照的同类超时。

18项chat与额外3轮54项重复通过，CLI全目标严格Clippy、独立复核与完整905项Rust门禁通过，日志为 `target/n2/research/chat-model-readiness-*` 和 `target/n2/readiness-workspace.log`。新增实际CLI提前退出诊断回归通过，Linux就绪同步仍由新runner验收。

### N2独立候选与基线证据

验收工作流为固定源码单独构建优化候选，原生／MCP／界面检查按各自依赖执行并保留逐阶段结果。完整源码门禁失败会使job失败，最终完成要求所有必需门禁成功。Linux补充实际Ubuntu22.04、glibc2.35、最大GLIBC符号和动态库检查，并与程序架构见证核对SHA。取消后收集已产生的记录与日志。

YAML、14个Shell片段、内嵌Python、失败／取消依赖场景和12项验收／安装器离线回归通过，独立只读复核无确认阻断。真实Actions编排和完整最终四平台结果继续验证。

### 四平台复验修复

`72e79a0` 的macOS15 ARM完整源码门禁及18项原生／25项MCP／24项TUI通过，Intel三个程序验收套件通过，源码结构夹具遇截止时间。Linux实际运行暴露长TMPDIR的Unix地址上限和guest PID宿主查询问题。本轮修复采用宿主目录FD短地址与FIFO现场存活／ACK／EOF见证，并收紧可写controller及Host聚合资源配置。CI调试镜像使用行表，结构夹具仅对唯一解析超时作有界重试。四平台仍等待新源码重新验收。

最新本机macOS27.2 ARM完成905项Rust测试、完整门禁及33项Python离线回归，并在固定优化程序 `fedcddbec295183a4bfcb6fac1a9d7552bba163b850ccd86a5013e0eafe19bdf` 上通过18项原生、25项MCP及24项TUI。源码检查与程序证据在 `target/n2/ci-profile-workspace.log`、`target/n2/validated-local/`，修复范围与原始远端失败见[N2验收记录](n2-validation.md#linux-长路径与资源控制修复)。Linux委派与四平台最终完成状态继续保留待验收。

### 终端预算与完整性能补证

完整性能v2本机已通过，覆盖两后端七项操作及三种代理网络路径，独立核验器与47项Python回归已接入CI。b091四平台源码CI全通过，其N2 Intel侧栏启动失败的旧raw缺口由新夹具诊断补齐。另以确定性可写PTY对照修复渲染时间误耗帧输出预算，记录与原Intel原因分别保留。

最终本机907项Rust完整门禁通过，新固定程序 `d2b0f84585ac0e66cf8953547bd9eac8b36cbd7e21b28024c0da469a8aa9479e` 通过18项原生、完整v2性能、25项MCP及24项TUI。原始与最终记录为 `target/n2/terminal-final-workspace.log`、`target/n2/terminal-final-local/`，最终四平台状态待新源码收证。[验收记录](n2-validation.md#终端输出预算与启动诊断)

### 四平台原生基线与测试同步

`a6f3d99` 的N2四平台工作流及产物独立核验全部通过。两Linux端各920项Rust／19项原生／25项MCP／24项TUI，两macOS15端各906项Rust／18项原生／25项MCP／24项TUI。独立源码CI保留Intel人工确认计时夹具的失败，后续测试以虚拟人工等待和未暂停预算负对照验证暂停契约。后台artifact夹具通过实际job完成状态同步，生命周期等待增加阶段诊断。

最新本机以4个测试线程完成906项Rust及完整门禁，日志为 `target/n2/human-budget-workspace-final.log`。生产代码与已通过的原生基线一致，新测试源码继续执行四平台CI和完整原生验收，最终状态待收证。[N2记录](n2-validation.md#人工确认与后台完成的测试同步)

### Linux 代理镜像修复

`1a932cc` 两台macOS15 runner的源码CI与N2均完整通过。两Linux架构通过MCP、TUI和委派资源正负对照，原生15／19项通过，剩余UDP判据与代理启动分别修复。UDP维持宿主零收包并增加代理端口双栈正控制。代理镜像在宿主按打开的FD验证root信任并固定为私有只读文件，guest核对精确device／inode，处理user namespace的UID映射。

修复候选通过本机906项Rust测试、完整workspace门禁、33项Python回归、两Linux目标严格模块检查及独立复核。日志为 `target/n2/proxy-final-workspace.log`，原始四平台记录位于 `target/n2/remote-review/1a932cc7/monitoring-complete.json`。实际Linux代理与最终四平台仍待新固定源码验证。

### 生命周期测试夹具竞态

N2源码 `1a932cc` 在Linux实际执行到原五项生命周期场景，旧夹具的guest PID宿主查询失败。现统一改为现场FIFO writer正控制和CLI终态后的立即EOF，保留真实CLI信号、退出码与后代清理断言，新增见证回归。本机三轮共18项测试与定向严格Clippy通过，Linux同一源码的完整门禁继续待重跑。该轮两Linux架构的25项MCP、24项TUI和委派资源正负对照已分别通过，完整N2仍有网络原生用例待修复与验收。

DEV-075门禁复现CLI测试先看到PID文件建立、再读到空内容的竞态。三处后台进程夹具现先写完整PID换行到临时文件，再原子重命名发布，清理和退出断言保留。五项生命周期测试重复30轮，150项全部通过，严格Clippy、格式与差异检查通过。首失败和重复记录位于 `target/n2/lifecycle-fixture/`。

## 完整 Markdown、数学、图表与 debug 预览

2026-11-09，本阶段完成本机开发与验证。项目负责人补充完整Markdown语法、Unicode数学、Mermaid及source／render显示要求，并指定仅debug构建提供 `saya tui-preview`。本轮构建本地测试版，四平台同步验证安排在N2阶段。

采用CommonMark／GFM事件解析、内置代码语言高亮、严格Unicode数学转换及有界Mermaid布局，统一表格和脚注语义锚。source／render命令、脚注点击／键盘往返和有界排版缓存已接入正常TUI。debug专属预览与render_preview共享27个场景，新增12组语法、数学、图表及回退场景。完整范围和保守回退条件见[Markdown说明](markdown-rendering.md)。

本机macOS27.2 ARM／Rust1.99.1执行 `MACOSX_DEPLOYMENT_TARGET=15.0 ./scripts/check.sh`，格式、全目标严格Clippy、746项Rust测试及优化构建通过。九项发行脚本测试、工具smoke、文本会话、既有审查回归及24组正常入口PTY通过。新增场景验证source／render、脚注点击／键盘返回、窗口变化、原始消息持久化及单次模型请求。验收程序与最终优化程序SHA-256同为 `5ade01b619e19d0489f2b3a4d8c6c5eaad73413df006c73110b60af5b287bb2e`。

最终debug程序通过九项独立PTY检查，包含场景切换、s／r、缩放、q／Esc／Ctrl+C及TERM／HUP退出、终端恢复和零状态文件。最终优化程序通过help隐藏与子命令拒绝门禁。10,000组随机输入在五种宽度下运行50,000次，再加40次压力验证，共50,040次渲染探针全部通过，源码前后哈希相同。探针覆盖物理行宽、10,000行预算、控制字符、脚注位置及panic边界。

引用和代码卡片主题的首次正常入口失败保留于 `target/markdown-full/tui-journey-before-theme.log`，修复后全量24组通过。完整检查、探针、debug门禁和原生程序记录位于 `target/markdown-full/`，13份彩色场景／source快照及预览画廊位于 `previews/`。`.saya/local/saya-trial` 已切换到本次debug测试程序，启动、Ctrl+P、Esc、Ctrl+D及终端恢复通过，启动检查模型运行数为0。程序声明最低部署目标15.0，本轮实际验收环境为本机。

### JSON参数规范化兼容

Mermaid依赖在构建图中启用JSON对象插入顺序保留。两项回归先复现了等价参数重排造成审批去重／拒绝记忆失配，以及嵌套对象规范化不稳定。`Operation::normalized_arguments`现显式递归排序，审批绑定、拒绝记忆和许可检查复用同一规范化入口。后台待审批枚举的大分支使用Box存储，通过新构建图下的严格Clippy。

显式启用 `serde_json/preserve_order` 后，35项审批、10项权限及四项后台生命周期测试通过，core全目标严格Clippy通过。修复前后证据为 `target/markdown-full/json-{order-before,recursive-before,order-after}.log`、`core-background-compat.log` 和 `core-compat-clippy.log`。最终Markdown与界面验收随本阶段继续记录。

## 0.2.0 后续开发中的 Markdown 显示

2026-11-09，本阶段完成本机开发与验证。按项目负责人要求，本轮构建与验收本地测试版，四平台同步测试安排在后续N2阶段。

TUI在现有富文本显示基础上增加宽度自适应管道表格、`__加粗__`和HTTP(S)链接显示。表格采用Unicode宽度、列对齐、自动换行与窄屏逐字段布局，链接同时展示标签及目标地址。流式内容按当前完整程度重排，原始消息由会话保存。实现位于 `crates/saya-tui/src/rich_text.rs`、`rich_text/table.rs` 与 `view.rs`，行为与显示预算见[TUI设计](tui.md#assistant-markdown-显示)。

本机macOS ARM执行 `MACOSX_DEPLOYMENT_TARGET=15.0 ./scripts/check.sh`，格式、workspace全目标严格Clippy、701项Rust测试及release构建通过，新增19项回归覆盖表格、链接、Unicode、流式补全、滚动及显示预算。九项Python发行脚本测试、工具smoke、文本会话、既有审查回归及23组正常入口PTY通过。新增PTY场景使用loopback模型夹具，验证80／40／20／140列重排、链接和加粗样式、鼠标滚动、持久化原文及终端恢复。独立复核的5000组Unicode／控制字符输入、2048组结构化链接及六项定向回归通过，源码哈希在探针前后相同。

20／40／80／140列离线预览、完整检查、PTY报告和本地二进制摘要位于 `target/markdown-local/`，独立探针位于 `target/markdown-review/`。`.saya/local/saya-trial` 已指向本次macOS ARM测试程序，配置、空闲启动、Ctrl+P、Esc、Ctrl+D及终端恢复检查通过，启动检查的模型运行数为0。二进制声明部署目标15.0，本轮实际验证环境为本机。正式版与历史试用程序分别保留独立产物记录。

## 0.2.0 正式发行与验收

2026-11-09，项目负责人授权的0.2.0已正式发布并设为Latest。标签 `v0.2.0` 固定到 `f579d1f1f8ec48f96e0544967bf4ffd6bbc2cbb0`。四个原生目标均通过682项Rust测试、九项发行脚本测试和22组PTY，完成格式、严格Clippy、优化构建、工具往返、文本会话、恢复及审查回归。[正式发行](https://github.com/Ariakage/saya-harness/releases/tag/v0.2.0)、[源码CI](https://github.com/Ariakage/saya-harness/actions/runs/42524461863)、[候选包验收](https://github.com/Ariakage/saya-harness/actions/runs/42524462140)、[标签CI](https://github.com/Ariakage/saya-harness/actions/runs/42525979782)

| 目标 | 实际原生验收环境 | 安装包SHA-256 |
| --- | --- | --- |
| linux-x86_64 | Ubuntu22.04／glibc2.35／x86_64 | `746e1a5675ed19b8d3364bae83b8cee0f96eec11e9bde75ef18539457587caad` |
| linux-aarch64 | Ubuntu22.04／glibc2.35／aarch64 | `1a5fc9f2b46925354096f69d42e009c89e59ac1001c98ed20f4d45a8b83c88ca` |
| macos-x86_64 | macOS15.7.9／Intel | `f37ed4277ead62a7915f53fd248023ccba406800dc7f58b8cde4e41e7b448022` |
| macos-aarch64 | macOS15.8.2／ARM | `c92a391264be9b4505d9f0dce5dba1fd6e399f24bac409fb5e0d8958b8db6742` |

四个平台包、SHA256SUMS、中英双语RELEASE-NOTES.md和release-evidence.json共七项资产均完成匿名公开下载，逐字节及SHA-256与验收对象一致。包内源码声明、README／LICENSE及程序哈希，以及验收脚本摘要经过独立核对。macOS ARM为ad-hoc签名，Intel未签名，发行流程未执行Developer ID签名或Apple公证。[正式版结构化证据](https://github.com/Ariakage/saya-harness/releases/download/v0.2.0/release-evidence.json)

正式macOS ARM二进制通过24例合成凭据探针及deepseek-flash三协议×四模式的12项真实任务。41次模型请求全部成功，35次工具调用中34次成功。Chat CODE的invalid_patch发生于事件序号15，事件序号21的补丁成功，最终任务与静态产物校验通过，失败错误码、调用ID和顺序均保留。三项PLAN工作区零修改，命令执行为0。生成源码仅静态检查，服务内部effort及网络分块未独立观察。[真实模型记录](live-model-validation.md)

本机 `.saya/local/saya-trial` 已切换到匿名下载的正式ARM程序，配置、空闲TUI、命令面板和退出恢复检查通过，模型运行数为0。原预发行和历史程序保留独立记录。完整日志、包、公开下载与验收材料位于 `target/release-0.2.0/`。

### 发行收尾的测试竞态修复

收尾文档提交 `7bd8156` 的CI在macOS ARM和Intel触发同一后台生命周期测试竞态。20ms超时可能先于文件提交发生，测试存储对取消结果发出通知后，测试误将该通知视为写入成功并读取文件，报出 `NotFound`。失败记录和实际发行源码的原生验收记录分别保留。[原始失败CI](https://github.com/Ariakage/saya-harness/actions/runs/42528160893)

本次修改限定成功结果保存屏障，在5秒真实等待保护内确认文件已提交，再暂停Tokio时钟并显式跨过60秒工具期限。测试保留结果保存前任务仍运行、PLAN延迟生效、清理完成后保留已观察成功结果及文件内容的断言。四项后台生命周期测试以四路并发重复200轮，共800次通过，日志为 `target/release-0.2.0/background-lifecycle-{fix.log,stress.json}`。本机执行 `MACOSX_DEPLOYMENT_TARGET=15.0 ./scripts/check.sh`，格式、全目标严格Clippy、682项Rust测试及release构建通过，日志为 `target/release-0.2.0/final-ci-repair-check.log`，两名Agent完成独立只读复核。修改范围为测试与本进度记录，四平台CI按本次修复提交验证。

## 0.2.0-rc.2 发行与验收

2026-11-09，项目负责人授权的0.2.0-rc.2已发布为GitHub预发行。标签 `v0.2.0-rc.2` 固定到 `daee043821529e270b876a77d5c6e8f47bed1c33`。四个原生目标均通过682项Rust测试、九项发行脚本测试及22组PTY，完成工具、文本会话、恢复与审查回归，源码／脚本／包／二进制摘要交叉核对通过。[预发行](https://github.com/Ariakage/saya-harness/releases/tag/v0.2.0-rc.2)、[源码CI](https://github.com/Ariakage/saya-harness/actions/runs/42518737234)、[候选包验收](https://github.com/Ariakage/saya-harness/actions/runs/42518738474)、[标签CI](https://github.com/Ariakage/saya-harness/actions/runs/42520065596)

| 目标 | 原生验收环境 | 安装包SHA-256 |
| --- | --- | --- |
| linux-x86_64 | Ubuntu22.04／glibc2.35／x86_64 | `e0ceba9485cd426d12b40a51fe3fafab15b0fdc6e4efe840eee864f093ceec31` |
| linux-aarch64 | Ubuntu22.04／glibc2.35／aarch64 | `097e260cfafa43642f6c13b048ac70eefbc5264747fcfbbe487ecc4b0774c733` |
| macos-x86_64 | macOS15.7.9／Intel | `810d29e182d735494c4d788b65e8e66b6d7f8045399c199258ee7c383283456c` |
| macos-aarch64 | macOS15.8.2／ARM | `e8e8544419153aea06a89d64b9c96fbff31a3197b807b9a6f02940dda0db048d` |

四个平台包、SHA256SUMS、中英双语RELEASE-NOTES.md和release-evidence.json共七项资产完成匿名公开下载，逐字节／SHA-256与验收对象一致。macOS ARM为ad-hoc签名，Intel未签名，发行流程未执行Developer ID签名或Apple公证。[结构化证据](https://github.com/Ariakage/saya-harness/releases/download/v0.2.0-rc.2/release-evidence.json)

实际发布的macOS ARM二进制通过24例合成凭据探针，以及deepseek-flash三协议×四模式共12项真实任务。42次模型请求全部成功，36次工具调用中35次成功。Chat CODE运行包含一次未成功的补丁尝试，最终产物准确且任务完成，该失败的具体错误码与两次尝试的先后顺序未留存。三个PLAN工作区保持不变，命令执行为0。生成源码仅静态检查，服务内部effort及实际网络分块未独立核实，抽样范围见[真实模型记录](live-model-validation.md)。

本机试用入口 `.saya/local/saya-trial` 更新为该公开发行的ARM程序，保留之前产物。完整本机、原生工作流、下载及探针记录位于 `target/rc2/`。

## 第二轮审查整改

以 `c93fcca` 为整改起点，逐项核实[第二轮报告](review-2026-11-09_02.md)。报告中的发现与建议按既定契约和实际复现判定，确认问题已修复，各阶段完成适用检查后独立提交。契约保留项与最终证据见报告第7节。

- R2-01已复现并修复共享检测／脱敏不对称。残留编码凭据使脱敏失败关闭，五项redaction测试及SQLite提交前拒绝回归通过，日志为 `target/r2-01-before.log` 和 `target/r2-01-after.log`。
- R2-15的同模式请求已修复。撤回异构待切换请求会清空pending并更新revision，原模式可继续开始共享调用。两项新增回归的失败／通过证据为 `target/r2-15-mode-before.log` 与 `target/r2-15-mode-after.log`，core全目标Clippy通过。其余后台模式及超时问题继续核实。
- R2-02已复现并修复环境覆盖沿用argv／cwd授权的问题。非空env由完整参数策略核查，版本1预授权保持未消费，静态auto allow进入模型核查。权限7项、审批32项和审核8项测试及core全目标Clippy通过，日志为 `target/r2-02-{before,after,clippy}.log`。
- R2-07至11及R2-20的TUI部分已完成组件修复。覆盖可恢复错误、UI取消草稿、显式恢复覆盖项、单簇／视觉行字节上限、归一化后标题脱敏，以及粘贴、profile状态、详情覆盖和退出绘制。152项lib、8项input、7项终端测试与全目标Clippy通过，日志为 `target/r02-fix/tui-repairs-{tests,clippy}.log`。首条配置错误与HUP断开场景保留为最终正常入口PTY验收项。
- R2-12已修复合法WAL回滚尾帧的误报。无效帧停止扫描并采用最后有效提交前缀，头校验失败继续报错。真实SQLite溢写／回滚／短提交夹具与无shm原生恢复结果一致，源文件保持只读，六项WAL测试及core Clippy通过，日志为 `target/r02-fix/wal-tail-{tests,clippy}.log`。
- R2-04／05已完成CLI修复。文本会话任何退出路径先取消并等待运行／后台清理，再释放会话锁，已消费的JoinHandle独立收尾。run、chat及导出统一监听INT／TERM／HUP，session文本查询经过终端清洗。112项CLI测试、严格Clippy通过，包含真实后台进程信号／TTY丢失及锁时序回归，日志为 `target/r204-05-{cli-final,clippy}.log`。
- R2-03已修复path_glob静态前缀的符号链接失配。规则与目标均使用真实目录基准，覆盖未来目录、特殊字面目录及bypass下deny，权限9项、审批32项和core全目标Clippy通过，日志为 `target/r2-03-{before,after,clippy}.log`。
- R2-06已复现并修复上下文子集误拒和后台失效误杀。已提供来源按路径／哈希核对，背景守卫只读前台选择，失效后台调用保存配对的结构化失败。两项旧实现反例失败，43项runtime测试及core Clippy通过，日志为 `target/r2-06-*.log`。
- R2-18审批预写部分已修复。预写前和提交后统一核对取消、版本、拒绝及去重状态，提交期间过期可保留实际审计请求，但返回失效且无法发许可。两项旧实现反例失败，61项权限／审批／审核／Todo测试与core全目标Clippy通过，日志为 `target/r2-18-{before,after,clippy}.log`。模式事件发射另行核实。
- R2-13／19的Anthropic部分已修复空文本回放、空缓存system块和用量解析，缓存创建／读取计入总输入，非法或溢出计数拒绝，strict=true按官方能力保留。13项协议及2项thinking_effort测试、core Clippy通过，日志为 `target/r02-fix/anthropic-compat-{tests,clippy}.log`。
- R2-13的Responses部分已补充queued、增量身份及完成事件校验、最终明文反向覆盖、空summary加opaque兼容，保留完成事件独立提供完整输出的合法路径。12项协议测试和core Clippy通过，日志为 `target/r02-fix/responses-compat-{tests,clippy}.log`。报告中的reasoning_part事件名缺少官方定义依据，按现有标准事件集合处理。
- R2-16的CLI／app部分已修复。JSONL存储故障在已预留序号内输出唯一终态并保持会话／运行身份，恢复后单调递增。日志补齐Bidi过滤，相对导出统一cwd，PLAN同时保护当前及保存工作区。流式stdout断管改为显式IO错误退出1。146项CLI／app测试、严格Clippy及21项TUI服务测试通过，日志为 `target/r216-{final-tests,clippy,tui-export}.log`。
- R2-14的工具落库／环境快照部分已修复。写入和命令在保存前共享净化意图与结果，自定义／内存Store及失败返回的观测结果同样覆盖，保留实际副作用状态。命令继承／引用和HTTP额外头使用配置环境快照，缺值沿配置错误处理，Debug仅显示变量计数。41项配置／exec／transport／write测试及core全目标Clippy通过，日志为 `target/r2-14-tools-transport-after.log` 与 `target/r2-14-clippy.log`。其他混合转义与占位符边界继续核实。
- 权限环境匹配、路径规则、CLI清理、TUI输入与渲染、上下文及协议／存储相关问题继续分组整改，最终验收使用全部修复合入后的程序。

- R2-19补丁路径已修复。统一`.`组件并仅成对剥离a／b标签，保留真实a／b目录语义，完整新文件上限继续遵循B-15。10项write测试及core全目标Clippy通过，日志为 `target/r2-19-{after,clippy}.log`。

- R2-15后台调度／超时与R2-18模式事件已修复。接纳后台任务时持共享租约，PLAN取消先于生效，非exec超时等待实际收尾并保留已观察结果。模式请求和实际变化随会话原子落库，提交后确认日志前缀，有界队列满时拒绝新请求。346项core测试及Clippy通过，独立复核25项模式测试和4项后台生命周期测试通过，日志为 `target/r215-core-final-{tests,clippy}.log` 和 `target/r2-15-18-independent-review.log`。

- R2-14共享脱敏边界已修复。逐字符混合转义匹配覆盖字面反斜杠、短转义及UTF-16，纯文本不误扣解码预算，无凭据歧义JSON逐字保留，凭据与占位符重合时安全替换。七项redaction测试通过，独立合成矩阵2456例零漏检，另验歧义JSON、65MiB纯文本和深度上限，日志为 `target/r2-14-redaction.log` 与 `target/r2-14-redaction-matrix-review.log`。

- R2-17及R2-20脚本部分已修复。Git忽略私有测试目录，macOS候选验收强制核对源码提交，PTY快照只写显式调试目录并扩大外层期限，live脚本支持可选二进制／平台和旧token字段。九项Python测试与两个工作流actionlint通过，日志为 `target/r2-script-tests.log` 与 `target/r2-workflow-actionlint.log`。

- R2-16日志收尾及R2-19跟随查询已修复。析构先关闭生产者再有界等待优先队列排空，日志读写共用打开后的目录身份校验，follow保留末尾半行且不计为损坏。18项logging测试及core Clippy通过，日志为 `target/r02-fix/logging-boundaries-{tests,clippy}.log`。

- R2-20恢复提示与终端字符边界已修复。恢复超大明文thinking显示可见扣留提示，恰好16MiB保留，opaque字段不投影。core提供共享控制字符谓词，CLI、TUI及Markdown保留各自换行／转义规则。318项app／CLI／TUI测试、七项core字符相关测试及四crate严格Clippy通过，日志为 `target/r220-{interface-tests,clippy}.log`。

- R2-19运行时边界已修复。取消后的Store历史回退返回storage_error并保留本地历史，结果提取使用受检切片。PLAN后台schema仅列四项只读工具，PID转换显式检查正i32范围。58项runtime／exec测试、core全目标Clippy及三项独立复核回归通过，日志为 `target/r2-19-runtime-{before,after,clippy}.log` 和 `target/r02-fix/runtime-guard-independent.log`。

- R2-07／20正常入口已补充PTY回归。首条任务校验失败保留草稿且可准确重试一次。真实HUP并关闭终端时，绘制失败进入所属任务收尾，保留信号退出码130，验证进程终止、cancelled落库及会话锁释放。169项TUI测试、严格Clippy及四项脚本helper测试通过，新增两个PTY场景通过，日志为 `target/r2-{new-pty,pty-tui-tests,pty-tui-clippy,pty-helper-tests}.log`。

- R2-19导出与目录权限已修复。身份字段拒绝截断，失败脱敏调用仅在持久记录与历史记录身份两侧唯一时补run关联，旧历史歧义保持null。新建data／locks目录使用0700，已有目录权限保留。38项初验及补充独立复核50项export／SQLite／logging测试、core严格Clippy通过，日志为 `target/r02-fix/export-directory-{tests,clippy}.log` 与 `target/r2-19-{export-storage-review,logging-independent-review,export-fallback-clippy}.log`。

- R2-13相邻CLI完成输出已修复。逐次模型请求记录已显示前缀，合法完成事件补齐缺失正文，常规delta不重复。完成顺序与增量交错不一致时换行展示完整已校验回答。十项text_stream／lifecycle正常入口测试及CLI严格Clippy通过，包含断管后真实后台清理，日志为 `target/completed-text-{before,final-tests,clippy}.log`。

### 最终同产物验收

修复源码为 `6d0c609fc9f02a71d74ea2b1afc79fad397ac51e`，本机macOS27.2 ARM／Rust1.99.1执行 `MACOSX_DEPLOYMENT_TARGET=15.0 ./scripts/check.sh`，格式、workspace全目标严格Clippy、682项Rust测试和release构建通过。同一二进制完成九项发行脚本测试、smoke、文本journey、既有审查回归与22组正常入口PTY。24例安全探针通过，其中16例个人上下文阻断为0HTTP，其他受控工具流程共11次执行请求。completed-only的三种输出另经同产物各一次请求验证，明文完整且thinking／opaque标记不外泄。

二进制SHA-256为 `31391a832815471d3d2ab5dfbb7ab45dfefcf13d05ab97d26ced8057661ec1a0`，155份源码／脚本／构建文件与该提交逐字核对。219个文件及本轮提交差异的实际配置凭据扫描零命中。独立编码矩阵2456例零漏检，70份Markdown、706条相对链接、25条锚点及16份提示词来源校验通过。最终日志、哈希、源码清单和探针报告位于 `target/review-r2-final/`。

本机试用程序更新到 `target/local-trial/6d0c609-macos-arm64/`，稳定启动脚本为 `.saya/local/saya-trial`。配置检查、空闲TUI、Ctrl+P、Esc、Ctrl+D和终端恢复通过，模型运行数为0。实际Mach-O声明最低15.0。当前源码的四平台CI、原生macOS15／Linux及真实模型测试继续待验，历史证据保留原提交归属。

## 当前状态

- 产品基线：D-027，经 D-029 补充运行内后台工具调用，D-030 确定提示词定制与语言，D-032 明确 Manual Approval，D-033 纳入用户级／工作区指令与 Skill 发现，D-034 补充动态上下文与前缀缓存，D-035 将最低支持版本调整为 macOS 15。
- 仓库基础文件：MIT、.gitignore、Rust 1.99.1 workspace、saya-core／saya-app／saya-tui／saya-cli 四个 crate 与 Cargo.lock 已建立。
- 开发计划：已准备，包含 P0 至 P7、DEV-001 至 DEV-037；工作量估算不是发布日期承诺。
- core 运行协调器、三协议适配、本地持久会话与恢复已接入 CLI。工具往返、取消、超时、预算、权限、模式切换、结构化输出和四类任务通过受控验证，指定模型三协议文本与工具往返完成小规模联调。正式源码 `eaade53` 的[四平台 CI](https://github.com/Ariakage/saya-harness/actions/runs/42383106399)与[原生发行验收](https://github.com/Ariakage/saya-harness/actions/runs/42383135600)均通过。
- [v0.1.0 已发布](https://github.com/Ariakage/saya-harness/releases/tag/v0.1.0)，四个平台安装包、校验和、验证记录及双语说明已上传，七项资产的公开下载内容均与验收产物一致。
- 后续规划包含 F-01 至 F-20，N0 至 N5／DEV-038 至 DEV-089 的依赖与验收见[开发计划](post-mvp-plan.md)。本轮细化 Browser、原生 Sandbox／AST、上下文／长输出和插件规范，运行内后台工具已有 D-029 的 MVP 实现。
- DEV-038至DEV-053已完成并随0.2.0发行，代码审查和发行证据见对应历史记录。当前N2的DEV-054至DEV-061、DEV-075已完成四平台验收，下一阶段为N3上下文与扩展。

## 2026-11-09 风险优先代码审查

起始 HEAD 为干净的 `c14982a`，覆盖 v0.1.0 至该基线并优先审查 RC1 后增量。六项原复现已完成首轮修复和 Agent 交叉复核。提交为 Todo `294e2fa`、Responses `fe408d1`、CLI `d32c48b` 和 TUI `727832b`。后续独立复核发现的R-02缺口已完成补修，等级、触发条件、回归和处置见[审查报告](review-2026-11-09.md)。

macOS 27.2 ARM／Rust 1.99.1、部署目标 15.0 的完整检查通过格式、workspace 全目标严格 Clippy、597 项 Rust 测试和优化构建。同一程序通过 5 项发行工具测试、smoke、journey、既有审查回归及实际 20 组正常入口 PTY。程序 SHA-256 为 `80cb4093bddb9db55f000ca691ff29cefc2a9e5c87028c272a7a55d6db91d417`，代码源码固定为 `727832b`。日志和脚本／程序／源码哈希位于 `target/review-20261109/`，原始 PTY 报告为 `target/review-tui-journey.json`。

补充检查以 `ee742b3` 为基线，核对源码／脚本／程序摘要与原验收记录一致，独立重跑通过59项CLI、142项TUI lib、19项Todo／Responses测试及20组正常入口PTY。当时的个人上下文探针每轮六例，其中Markdown或普通正文包裹的Unicode JSON在两入口均放行，四个本地请求保留了可解码的已知合成凭据。该轮将R-02重新标为待修复，证据保留在 `target/review-20261109/personal-markdown-credential-*` 和 `repro-personal-markdown-credential.py`。

### R-02 正文编码凭据补修

以 `15c7408` 为基线补修共享凭据检测，通过有界JSON转义视图识别Markdown／普通正文中的Unicode、混合及嵌套编码。原文和完整JSON检查保留，最多256层及累计64 MiB扫描，预算耗尽失败关闭。旧实现新增回归失败，补修后60项CLI、4项redaction回归和17项独立边界探针通过，合法偏好／记忆Markdown逐字发送。

本机macOS27.2 ARM／Rust1.99.1、部署目标15.0的完整格式、workspace全目标Clippy、600项Rust测试与优化构建通过。同一最终程序完成5项发行工具测试、smoke、文本journey、既有审查回归和20组正常入口PTY。原六例绕过全部退出2且零模型请求。程序SHA-256为 `266e0363345042024bb83c326d21ab37e846b2c571e6295835108c624065eeb9`，实际Mach-O最低部署版本15.0。源码、程序和各项证据索引为 `target/r02-fix/validation.json`，完整日志为同目录 `check.log`。

R-02已修复并完成独立复核，本地稳定试用程序使用本次最终产物。后续补充当前开发线的其他平台及真实服务验收。历史 P7 与 RC1 `a6c172d` 的四平台证据保持各自源码归属，新增功能的当前验证范围为本机和受控模型，正式发布按适用指令安排。

## 后续开发状态

开发计划与专项设计按下列任务跟踪，新增功能实际状态如下。0.2.0已交付N0／N1，N2当前源码验收完成，后续版本范围在发行准备时确定。

| 阶段 | 任务 | 实际状态 | 已有基础与下一入口 |
| --- | --- | --- | --- |
| N0 共享基础 | DEV-038 至 DEV-046 | 完成（本机验收） | 共享应用、日志／追踪与会话导出均通过正常 CLI 入口及故障回归，见下表 |
| N1 默认 TUI | DEV-047 至 DEV-053 | 完成（四平台候选验收） | 默认入口、对话／审批／会话／后台／导出已接入共享服务，候选与本机试用程序通过验证，设计与双语说明已交付 |
| N2 隔离与 MCP | DEV-054 至 DEV-061、DEV-075 | 完成（四平台验收） | 原生隔离、受控网络、命令结构事实、MCP与凭据通过最终源码CI、原生／性能／MCP／Markdown与TUI验收，见[N2记录](n2-validation.md#最终四平台验收) |
| N3 上下文与扩展 | DEV-062 至 DEV-068、DEV-076 至 DEV-078、DEV-084 至 DEV-089 | 完成（四平台验收） | 16项共享运行入口通过四端Rust、原生、性能、MCP、插件、CLI与TUI联合矩阵，固定源码 `e1da373`，见[N3最终验收](n3-validation.md#四平台最终验收) |
| N4 Goal 编排 | DEV-069 至 DEV-074 | 待开始 | 先做目标数据和手动步骤，后续自动阶段依赖持久调度等能力 |
| N5 Browser | DEV-079 至 DEV-083 | 待开始 | 托管 Chromium／Playwright worker、网络及文件 broker、界面和联合验收待开发 |

### 全局记忆规划补充

2026-11-08，按 D-045 将F-07细化为模型可读取、检索、编辑和遗忘的全局记忆。候选入口为 `~/.saya/MEMORY.md`，主题资料放入 `~/.saya/memory/`。新增DEV-084至DEV-089六项N3任务，覆盖可读文件、来源与作用域、上下文预算、三权限模式、并发发布、回滚、人工审阅和跨会话验收。后续计划共52项任务。

[设计草案](memory.md)与路线图、任务表已同步，任务编号、依赖无环及仓库内Markdown链接已检查。上述能力均待开发，当前已有的 `--memory-file` 显式注入见使用指南。

### N1 实施记录

| 任务 | 状态 | 阶段提交 | 证据 |
| --- | --- | --- | --- |
| DEV-053 | 完成（候选验收） | 7b7f2df，验收源码a6c172d | [候选工作流42407739653](https://github.com/Ariakage/saya-harness/actions/runs/42407739653) 的四原生目标全部通过，下载的归档／二进制／源码声明／脚本摘要／权限／凭据检查一致。0.2.0-rc.1的双语说明、迁移、界面设计及本机试用程序已交付。详细环境与SHA-256见[TUI验证](tui-validation.md)，正式发布安排在后续代码review与其他测试之后 |
| DEV-052 | 完成（四平台原生验收） | 87feea8、e098e9a、a6c172d、131ca6b | [CI 42407739097](https://github.com/Ariakage/saya-harness/actions/runs/42407739097) 的Ubuntu22.04 ARM／x86_64和macOS15 ARM／Intel全部通过，每个平台执行445项Rust测试、优化构建、工具smoke、文本会话、5项审查回归及13组真实PTY场景。包含三协议、多行Unicode／粘贴、4模式×3权限、逐工具审批、模式失效、会话／导出、后台／PLAN、unknown、慢消费者和信号。8项输入／6项终端测试含36轮panic与250ms帧预算，实际证据见[TUI验证](tui-validation.md) |
| DEV-051 | 完成（本机正常入口） | 16633ac | 无子命令仅在 stdin／stdout 均为 TTY 时进入 TUI，非 TTY／管道及 JSON／JSONL 冲突提前返回2且 stdout为空。3 项新路由回归和 105 项 CLI 测试通过，原 chat／run／resume／session／config 继续使用显式入口，workspace Clippy通过。日志为 target/n1-dev051-*.log，完整本机PTY矩阵结果见 DEV-052 |
| DEV-050 | 完成（组件与服务验收） | 7e9b2f6 | 新建／浏览／恢复会话、profile 选择、工具／后台快照、取消与 JSON／Markdown 导出复用 saya-app。11 项存储服务回归、36 项 TUI lib 测试和全目标 Clippy 通过，覆盖锁释放、unknown／截断及 run_id＋call_id、PLAN 路径、取消／退出后的发布回执、脱敏、SQLite 初始化不阻塞界面，以及工具轮次后的最终回答位置。详情缓存限制为 2 MiB／10,000 逻辑行。日志为 target/n1-dev050-*.log，正常入口在 DEV-051／052 验收 |
| DEV-049 | 完成 | 3b6da25 | 复用core RuntimeHandle和输入桥，支持实际／待切换模式、空闲权限、逐工具批准、澄清、命令面板和取消。19项lib测试及Clippy通过，含10项真实受控运行／人机输入边界回归，阻断重复键、组合键与隐藏面板误批准 |
| DEV-048 | 完成 | 152ff2d | 有界UTF-8/grapheme编辑、流事件聊天协调器、安全折行／代码块、滚动与窄屏呈现已实现。9项lib组件／草稿回归和Clippy通过，重绘与事件消费有界调度；正常入口集成按DEV-051／052验收 |
| DEV-047 | 完成 | d7a1489 | saya-tui library、Ratatui0.30.2／Crossterm0.29.0已锁定。3项真实PTY生命周期测试、Clippy及格式检查通过，覆盖normal/error/panic/重复进入及termios与屏幕恢复。日志为target/n1-dev047-*.log |


### RC1之后的TUI交互完善

2026-11-08，开发线加入斜杠命令补全与输入建议。24项静态命令／参数前缀与命令面板、帮助共用，支持灰色后缀、候选说明、方向键选择、Tab／末尾→接受及Esc隐藏。Enter执行完整草稿，profile名、会话／任务ID和目标路径由用户补齐。

本机macOS27.2 ARM通过格式、全目标Clippy、466项Rust测试、优化构建、5项发行工具测试和14组真实PTY验收。新增场景覆盖补全时的零副作用、参数前缀、草稿／光标／多行／字面量斜杠、命令面板及人工审批优先级。Shift+Esc关闭建议和缺少导出路径时保留草稿另有针对性回归。日志为 `target/tui-completion-check.log`，完整正常入口报告为 `target/tui-completion-release-matrix.json`。本节证据来自本机，RC1的四平台候选证据对应上方已固定源码。

#### 紧凑界面、会话标题与 thinking effort

2026-11-08，按 D-042 初版实现参考图对应的紧凑窗口。初版将宽度低于100列或高度低于24行的窗口设为黑底外框、顶边工作目录、状态右侧权限、灰色输入与蓝色竖线。底栏显示模式、模型、thinking effort及带环形字符的输入占比，最小20×6仍可编辑。长模型名为effort与用量留出空间。窗口标题从首条用户任务派生为 `Saya | 简短标题`，已知凭据先脱敏，随后合并空白、保留80字符并移除终端控制。新建、profile选择和恢复同步标题，OSC 2与xterm标题栈走有界恢复。

profile新增可选 `thinking_effort`，支持low／medium／high／xhigh／max，三协议分别传入对应effort字段。未配置时保留服务默认及原配置指纹，独立核查按其目标profile取值。界面显示实际请求值，服务内部映射依模型确定。Chat兼容服务的 `reasoning_content` 按assistant轮次以严格协议项持久保存和回传，完整JSON／SSE均受16MiB聚合限额约束，并覆盖恢复、正文／导出隔离及敏感数据事务拒绝。参数及协议边界见[配置](configuration.md#thinking-effort)与[模型接入](model-protocols.md#chat-completions)。

输入占比使用最近一次执行请求的报告用量，独立核查与运行累计值分开。开始新请求及新建／恢复／切换profile时清空旧值，缺失显示 `—`，估算带 `~`。独立复核发现并修复人工审批待清理和最终队列排空时可能沿用旧值的两个边界。

本机macOS27.2 ARM／Rust1.99.1的 `MACOSX_DEPLOYMENT_TARGET=15.0 ./scripts/check.sh` 完整通过格式、workspace全目标Clippy、488项Rust测试和优化构建。该程序通过15组真实PTY、5项发行工具测试、工具smoke、文本会话journey与5项既有审查回归，私有模型配置校验通过。三协议max均在正常TUI入口核对请求字段与显示值，compact覆盖80×24／60×18／20×6、审批／补全、用量、标题注入和恢复。隐藏reasoning通过正常工具调用、SQLite保存、后续请求、终端输出和完整导出的联合验证。验证使用受控本地服务，环境和程序摘要见下方记录。

- 完整检查为 `target/tui-compact-check-final.log`，正常入口报告为 `target/tui-compact-release-matrix.json`。
- 该次程序SHA-256为 `7a71c6aa37f128cff2663ae8a1ca3f05c5b45639e0bd99aa9029f4e11b481a18`，Mach-O部署版本为15.0。
- 视图和交互说明见[TUI设计](tui-design.md)与[TUI接口](tui.md#紧凑窗口与会话标题)，本次新增功能的跨平台及真实服务证据按对应环境后续验收维护。

#### 统一参考布局与全屏信息侧栏

2026-11-08，用户实测指出 `c09fc39` 在大窗口进入旧样式。已删除旧宽屏绘制路径，按 D-043 将主区参考样式应用到全部尺寸，灰色输入区默认4行并采用实心蓝色左条。达到120列×24行时显示会话名、上下文分项、扩展状态和工作目录侧栏。20×6详情保留可滚动内容，翻页依据实际可见高度。

新增共享应用层ContextModel，在执行请求边界推送仅含固定标签、计数和Skill数量的最新快照。System／Instructions／Skills／Preferences／Memory／Conversation／Files／Tools／Protocol九类采用明确标注的字符启发式估算，侧栏支持小数百分比和部分单元格条形。宿主前缀按位置和类型验证，后续同形用户JSON归入对话，原始协议内容按实际匹配关系去重。Review保持独立，新建／恢复／换profile清空旧快照，最终关闭通道保留末次统计。MCP与插件显示当前Not implemented，Skill数量来自实际注入的目录元数据。

本机macOS27.2 ARM／Rust1.99.1通过完整 `MACOSX_DEPLOYMENT_TARGET=15.0 ./scripts/check.sh`，包含格式、全目标Clippy、505项Rust测试和优化构建。独立复核通过40项controller和23项view测试。优化程序通过15组真实PTY，新增110／120／160列、119↔120和23↔24阈值往返、上下文与Skill真实来源、Files结果增长、实心蓝条、极小窗口逐页核查及既有三协议／模式／权限／会话／取消链路。

检查日志为 `target/tui-reference-check.log`，正常入口报告为 `target/tui-reference-release-matrix.json`，程序SHA-256为 `a53f3a266f14e96f270527d55162eef2b6c9d4ed09f17d124aacbe75592a441f`。两组生产组件TestBackend布局PNG位于 `target/tui-reference-preview-110x32.png` 和 `target/tui-reference-preview-160x40.png`，预览使用合成数据。本节验收为本机受控服务和PTY，原生iTerm2截图访问受到工具限制。

#### 视觉层次与空输入Tab模式循环

2026-11-08，按 D-044 参考OpenCode细化配色与正文层次。共享低对比主题区分主区、侧栏、输入区和代码／工具卡片，角色标签与元数据分别着色，宽正文增加左右留白。Assistant呈现基础Markdown标题、列表、引用、强调、行内代码和代码卡片，流式未闭合标记保留字面内容，工具与审批维持安全原文。富文本逐逻辑行生成，片段与嵌套有界，跨样式grapheme折行沿用总行数和10,000行尾缓存。URL／HTML内容只作可见文本，原始会话内容继续保存。

空输入Tab按WORK→CODE→PLAN→BUILD→WORK循环，复用模式命令的检查、持久化与审批失效路径。运行中和UI任务收尾期间从最新请求的模式继续循环，模式应用仍由core执行。普通草稿、空格、多行和命令前缀维持编辑／补全，命令面板独立；空输入的重复／释放事件不会插入Tab或重复切换，常规切换不生成额外对话消息。

本机macOS27.2 ARM的最终完整检查通过格式、全目标Clippy、524项Rust测试和优化构建。优化程序通过16组真实PTY，新增空Tab循环、无模型／存储副作用、编辑与面板边界、活跃模式持久化、人工审批失效、三协议正文样式及原文保存。5项发行工具测试、工具smoke、文本journey和5项既有审查回归均通过。检查为 `target/tui-polish-check-final.log`，PTY报告为 `target/tui-polish-release-matrix.json`，程序SHA-256为 `1a01f8512cd191710c922da49ba23ae64e8e4bf85e689040586907af8fb8f812`。

生产组件的合成状态预览为 `target/tui-polish-preview-160x40.png`、`target/tui-polish-preview-110x36.png` 和 `target/tui-polish-preview-idle.png`。模型服务验证使用本地可控夹具，平台范围为上述本机环境。

#### 任务清单、Thinking与可折叠工具

2026-11-08，按D-046加入模型可维护的 `todo_write` 会话任务清单。成功快照通过调用／结果历史重建，运行上下文提供最新列表，TUI侧栏显示进行中及完成数。参数、64项／64KiB限额、一次性许可、三权限模式、PLAN元数据、失败／取消／存储错误及SQLite恢复有回归，工具与PLAN英文提示词及文档快照同步。

三协议为应用层提供可读Thinking投影，审核、签名、redacted和加密内容保持隔离。watch日志按轮次有序显示，单轮256KiB、合计2MiB／64轮，完整脱敏后截断，已知凭据配置下等待该请求结束后发布正文。Thinking默认折叠，长工具结果超过6行或512字节也默认折叠，可点标题或Ctrl+O展开。同一工具调用复用执行中／完成卡片，鼠标滚轮在指针所在对话／详情内滚动，启停及异常退出恢复鼠标报告。

独立复核补齐最终历史重建Tasks、空回答惰性卡片、日志驱逐后的结束提示、最终事件排空，以及JoinError已消费句柄后的后台清理等待。`RuntimeHandle::shutdown_background`复用现有取消与清理，真实exec在模型panic／任务abort后均等待到PID消失，重复清理有回归。

本机macOS27.2 ARM／Rust1.99.1的 `MACOSX_DEPLOYMENT_TARGET=15.0 ./scripts/check.sh` 完整通过格式、workspace全目标Clippy、562项Rust测试与优化构建。该程序通过18组正常入口PTY、5项发行工具测试、工具smoke、文本会话和5项审查回归，私有配置离线校验通过。PTY使用本地受控服务，覆盖三协议可读推理展开与opaque隔离、任务逐次审批／PLAN拒写／恢复、长工具与伪标题点击、左右分区滚轮、短侧栏、终端恢复及既有流程。观察器为完整差分帧和有界排空补了回归门槛。

日志为 `target/tui-interactive-check-final.log`，正常入口报告为 `target/tui-interactive-release-matrix.json`，程序SHA-256为 `9050acb2510c02e698f199a7aaf3dbb17a4151a46b12e9e60193ed91fb2ea76b`，Mach-O最低部署版本为15.0。`target/tui-interactive-thinking-tasks-160x40.png` 与 `target/tui-interactive-expanded-tool-110x36.png` 来自该程序的受控PTY单元格快照。当前证据为本机离线验收，四平台候选证据仍对应上方固定RC1源码。

#### 输入区与摘要卡片美化

2026-11-08，开发线进一步细化输入区、工具卡片与侧栏。Message采用Strong前景与加粗，发送／换行／折叠提示使用键帽样式。空草稿显示斜体 `Describe a task…` 渲染占位，审批／澄清、补全和粘贴超限按实际状态显示，底部留白加入浅色收边。真实光标、Unicode多行和极小窗口有回归。

可识别的完整结构化工具结果默认收成摘要卡片，展示实际Todo计数、文件行范围／字节数、搜索／目录项数、退出码和错误信息，展开查看原始结果。摘要严格校验数据，输出限制512字节／120个grapheme，覆盖巨型组合簇、控制字符、重复字段、未知／矛盾／不完整结果。Thinking采用精简状态标题，侧栏加入弱分隔、强标题、非零数值层次和任务进行中强调。

本机macOS27.2 ARM／Rust1.99.1通过部署目标15.0的完整格式、workspace全目标Clippy、577项Rust测试及优化构建。正常入口19组PTY全部通过，新增短工具摘要、失败与截断状态、逐卡展开结果与模型所见JSON相等、同名真实占位文本、66KiB粘贴、输入键帽和底部收边等验证。5项发行工具测试、工具smoke、文本journey、5项审查回归及私有配置离线校验通过。

检查日志为 `target/tui-beautify-check-final.log`，PTY报告为 `target/tui-beautify-release-matrix.json`。程序SHA-256为 `aef46edff56e04056f79cf0f2a58636e5ddf349f9fead6a82ce6d1b54865dade`，Mach-O最低部署版本为15.0。`target/tui-beautify-refined-160x40.png`、`target/tui-beautify-refined-110x36.png` 和 `target/tui-beautify-composer.png` 为生产组件合成状态预览，含颜色与字重投影。本轮正常入口PTY另保存4组单元格快照，实际模型服务使用本地受控夹具。

#### 四模式主题色联动

2026-11-08，按D-047为WORK／CODE／PLAN／BUILD分配晴蓝 `#58A6FF`、薄荷绿 `#7DCB9B`、薰衣草紫 `#BB9AF7` 与暖杏橙 `#F6AD55`。输入框实心色条和底栏实际模式共用 `mode_color`，待切换文字分别使用当前／目标色，箭头为Muted，色条按实际模式生效更新。模型名称使用显式正文前景提升可读性，未知模式回退Muted。

部署目标15.0的完整格式、workspace全目标Clippy、579项Rust测试与优化构建通过，独立复核通过128项TUI lib测试。新增回归覆盖四模式与未知值、5种尺寸、极小详情单行输入、待切换与实际生效、模型字段前景。最新程序的19组正常入口PTY全部通过，包含12种启动组合、Tab四色循环、运行中pending及边界后的同步着色。5项发行工具测试、工具smoke、文本journey、5项审查回归与私有配置离线校验通过。

环境为本机macOS27.2 ARM／Rust1.99.1，日志为 `target/tui-mode-colors-check.log`，PTY报告为 `target/tui-mode-colors-release-matrix.json`，程序SHA-256为 `d89d5ca61db05da5d564fff28b51957b9090aa4052e9138365d1c09316cb289a`，Mach-O最低部署版本15.0。四模式与pending单元格快照保存为 `target/tui-mode-colors-*-110x32-cells.json`，局部合成图 `target/tui-mode-colors-composers.png` 根据本轮受控PTY数据渲染。

#### Ctrl+P 弹出命令面板

2026-11-08，按D-048将Ctrl+P改为独立的居中圆角浮层，最多88列×14行。24项共用命令目录显示选中高亮、命令说明与选择计数，方向键／候选行滚轮浏览，鼠标点击选中，Enter执行高亮项。可见窗口保持行位置，缩放后跟随选择，20×6保留三项候选和关闭提示。

浮层与真实详情分别保存，Esc／Ctrl+P关闭保留详情、滚动位置和草稿。模态期间编辑、粘贴及背景折叠／滚动由面板接管，带参数命令填入空草稿，既有斜杠草稿保持原文。提交与关闭要求新的Press。独立复核修复异步详情遮盖澄清请求时可能提交隐藏草稿的边界，审批与澄清统一先核对可见请求，再接受回复。

本机macOS27.2 ARM／Rust1.99.1，部署目标15.0的完整格式、workspace全目标Clippy、590项Rust测试及优化构建通过，独立139项TUI lib复核通过。最终程序完成20组正常入口PTY，新增模态组覆盖选择／执行、草稿、粘贴、鼠标边界、24项滚动、普通／宽屏／极小窗口及缩放、详情恢复、人工审批隔离和Ctrl+D清理。5项发行工具测试、工具smoke、文本会话、5项审查回归与私有配置离线校验通过。

完整检查为 `target/tui-command-palette-check.log`，PTY报告为 `target/tui-command-palette-release-matrix.json`，程序SHA-256为 `094562477a5e9643727213a7276861dd884acb79850f53b745ea7fa8d9d08421`，实际Mach-O最低部署版本15.0。本轮证据来自本机及本地受控协议服务，RC1四平台证据按其固定源码记录。

### N0 实施记录

| 任务 | 状态 | 阶段提交 | 证据 |
| --- | --- | --- | --- |
| DEV-038 | 完成 | `b8c84af` | 新 saya-app 迁移模型组装与会话生命周期，CLI 正常入口回归通过，独立审查通过。`./scripts/check.sh` 的格式、Clippy、全套离线测试及 release 构建通过，日志为本机 target/n0-dev038-check.log |
| DEV-039 | 完成 | `88014a9` | 共享 EventWriter／EventModel、1秒有界 EventSink、人工输入桥与取消句柄已接入 CLI。3项桥接测试、三协议SSE至JSONL参数化正常入口回归、CLI完整回归及 app／CLI Clippy 通过，覆盖队列背压／断开／过期回复，日志为 target/n0-dev039-*.log |
| DEV-040 | 完成 | `97e92b7` | 五级 LogLevel、类型化白名单记录、旧行解码和安全文件边界已实现。日志9项测试及 core Clippy 通过 |
| DEV-041 | 完成 | `b01513a` | tracing安全subscriber、有界普通／优先队列、退出排空及执行／核查／工具／后台关联已实现，CLI安装全局订阅。日志12项、runtime40项及 CLI Clippy 通过 |
| DEV-042 | 完成 | `838bf00` | --log-level／SAYA_LOG_LEVEL优先级、show分页与tail轮转跟随已接入，查询只读且支持JSON／JSONL。查询4项、CLI5项、日志全16项和workspace Clippy通过，覆盖慢pipe中断、文件代际、私有权限、时间与终端控制 |
| DEV-043 | 完成 | `3cab1ae` | 单只读事务快照与 WAL image 校验，包含全集合及真实持久水位。7项快照测试（含合法JSON篡改与可选字段兼容）、5项 WAL 回归（含重建限额、SQLite VM 中断及旧逐文件限额兼容）和 core Clippy 通过，源128MiB／记录32MiB／100k行／5秒默认限额已固定 |
| DEV-044 | 完成 | `ca7f77d` | JSON导出 saya.session schema v1，范围／工具正文／内部态省略／复合调用关联／脱敏截断已实现。4项导出测试及core Clippy通过 |
| DEV-045 | 完成 | `ecf9c0b` | Markdown复用同一脱敏投影，动态围栏、HTML／ANSI／双向控制处理和总限额已实现。6项导出测试及 core Clippy通过 |
| DEV-046 | 完成 | `a463eba` | CLI离线导出和0600排他文件发布已实现，父路径身份复查、私有staging、摘要核验及机器错误码完整。7项发布测试、5项CLI验收和错误确认单测通过，涵盖RLIMIT写失败、路径替换、取消及已有目标保护 |

### N0 最终验收（2026-11-08）

验收源码为 `f1f48e4`，环境为 macOS 27.2 ARM、Rust 1.99.1。`./scripts/check.sh` 完整通过格式检查、workspace Clippy（warnings 为错误）、392项离线 Rust 测试及 release 构建，记录为本机 `target/n0-acceptance-final.log`。

| 验收要求 | 实际证据 |
| --- | --- |
| 应用服务与恢复契约 | CLI完整回归，模式覆盖、配置变更、PLAN目录、锁与恢复行为，以及独立迁移审查 |
| 流事件与输入控制 | Chat／Responses／Anthropic三协议SSE至JSONL正常入口，单一终态、关联ID与递增序号；队列、断开、晚回复、取消及审批失效回归 |
| 日志与追踪 | 日志16项（含查询）、runtime40项及CLI日志5项，覆盖字段白名单、秘密、轮转、优先队列、过载、丢弃、排空与慢pipe取消 |
| 一致性快照 | Snapshot7项及WAL5项，覆盖并发提交、真实水位、只读源、合法JSON损坏、旧schema、限额、VM取消和旧恢复每文件512MiB兼容 |
| 导出与文件发布 | Export6项、发布7项、CLI导出5项及错误确认单测，覆盖两格式、选择／省略／脱敏／截断、路径替换、RLIMIT写失败、取消及目标保护 |
| 原生release程序 | smoke、journey、review_regressions三组离线演练均通过；新日志查询及两种导出由同一个release程序实测，收据哈希与0600权限核对通过 |
| 文档与凭据 | 64个Markdown文件456个相对链接有效，既有私有凭据未出现在待提交文件中 |

新功能release演练报告为本机 `target/n0-release-features.json`，查询与导出的模型请求数为0。该次程序 SHA256 为 `d6a23962d167303bd0c4761e8b1fcff3ec0d5bd855ea625be40b2718b776ffc1`。跨平台记录继续分别依据实际环境验收维护，本次N0证据来自上述本机环境。

阶段提交见上表。独立审查追加 `b51787f`、`758dced`、`31d1cd6` 修复快照边界，`41a1e9f` 补齐三协议直接证据。实现时固定了快照128MiB源总量／32MiB记录／100k行／5秒、每导出32MiB、每字段64KiB等默认限额，具体语义见[导出说明](session-export.md)。

### 2026-11-08 专项设计细化

新增 Browser、原生 Sandbox 和上下文／长输出设计，将 saya-plugin 细化为 v1 草案并提供 JSON Schema 与 TOML 示例。Linux 组合和 Seatbelt 独立后端按 D-040／041 记录，容器／VM 作为统一接口的后续实现路径。该次设计交付时，后续功能 DEV 均为待开始，之后的实现状态见上方 N0 记录。

本机只读检查确认 macOS 27.2 提供 sandbox-exec，系统手册标记 deprecated。原生后端、macOS 15、Linux namespace／Seccomp／代理以及性能尚待对应 DEV 实测。

本轮文档验证通过 64 个 Markdown 文件的 450 个相对链接、46 项后续任务的无环依赖和 20 个能力编号检查。插件 JSON Schema 自校验、TOML 示例及 6 个有效／25 个拒绝案例通过，JSON 示例和已知测试凭据泄露检查通过。验证范围为文档、结构规范与示例，报告位于本机 `target/runtime-design-validation.json`。

### 2026-11-07 初版计划验证

初版规划核对了引用对话、workspace、日志、CLI 路由、流事件和存储查询接口。`./scripts/check.sh` 通过格式检查、Clippy、342 项 Rust 测试和 release 构建。61 个 Markdown 文件的 418 个相对链接、37 项后续 DEV 的无环依赖、20 个能力编号及 JSON 示例检查通过。变更范围为文档及 core 模块说明注释。

## 阶段状态

| 阶段 | 状态 | 退出证据 |
| --- | --- | --- |
| P0 工程基础 | 完成 | DEV-001、DEV-002 完成；四平台 CI 检查通过，见下方记录 |
| P1 核心循环 | 完成（受控验证） | DEV-003 至 DEV-006 与 DEV-037 完成；可复用 core 循环及工具、权限、后台、预算在本地模型替身与受控进程上验证 |
| P2 工具与权限 | 完成（受控验证） | DEV-007 至 DEV-012、七工具派发及 12 组模式验证通过；运行时接线、CLI 与持久化的状态见 P1、P4、P5 |
| P3 模型协议 | 完成（受控验证与指定模型抽样） | DEV-013 至 DEV-017、DEV-034 的本地夹具通过；指定模型三协议文本和工具往返抽样通过，完整证据见下方 |
| P4 CLI 与交互 | 完成（只读与受控验证） | DEV-018 至 DEV-021 完成；连续对话、单次任务、人工输入、终端控制与项目上下文证据见下方 |
| P5 持久化与恢复 | 完成（本机受控验证） | DEV-022 至 DEV-025、DEV-035 和项目指令来源记录已验证 |
| P6 输出与集成 | 完成（本机受控验证） | DEV-026 至 DEV-029 完成；AC-01 至 AC-23 已索引，发行验收归 P7 |
| P7 发行准备 | 完成 | DEV-030 至 DEV-032 完成；四原生目标最低环境、解包演练与审查回归通过 |

## 任务状态

| 任务 | 阶段 | 状态 | 证据／阻碍 |
| --- | --- | --- | --- |
| DEV-001 | P0 | 完成 | workspace、固定工具链与锁文件已建立；本机检查与构建通过，见下方记录 |
| DEV-002 | P0 | 完成 | 本地测试和四平台 CI 检查通过，见下方记录 |
| DEV-003 | P1 | 完成 | 核心数据、schema v1 和适配接口已建立；本机契约测试通过 |
| DEV-004 | P1 | 完成 | 测试专用假循环、内存存储和执行前保存验证通过；不接真实工具 |
| DEV-005 | P1 | 完成 | 原型的取消、超时、预算、重试和结果映射已验证；core 接入见 DEV-037 |
| DEV-006 | P1 | 完成 | 原型模式切换、取消优先与 PLAN 工具重查已验证；core 接入见 DEV-037，CLI 交互待 P4 |
| DEV-007 | P2 | 完成 | 四项只读工具、根目录校验、忽略规则及上限已实现，本地测试通过 |
| DEV-008 | P2 | 完成 | 单文件写入、补丁、意图存储及结果保留已通过本地受控测试 |
| DEV-009 | P2 | 完成 | argv、受控环境、输出限额、超时与进程组清理已在受控进程上验证 |
| DEV-010 | P2 | 完成 | 模式矩阵、路径限制和 TOML 规则优先级本地验证通过；需核查／确认的工具门禁默认拒绝 |
| DEV-011 | P2 | 完成 | 独立无工具核查、人工兜底和共享请求预算经替身测试；三协议 profile 接线见 DEV-017 |
| DEV-012 | P2 | 完成 | 一次性许可、预授权和失效经受控验证；core 调度接线见 DEV-037，CLI 待 P4 |
| DEV-013 | P3 | 完成（受控验证） | HTTP／SSE、鉴权、超时、取消及系统代理通过本地夹具验证 |
| DEV-014 | P3 | 完成（受控验证） | Chat Completions 流、完整响应、工具回传与本地运行时闭环已验证 |
| DEV-015 | P3 | 完成（受控验证） | Responses typed items、无状态历史回传和本地运行时闭环已验证 |
| DEV-016 | P3 | 完成（受控验证） | 两版本 Messages、签名状态、工具回传及本地运行时闭环已验证 |
| DEV-017 | P3 | 完成（受控验证） | 三协议独立核查、默认／指定 profile、预算与人工兜底已验证 |
| DEV-018 | P4 | 完成（core 配置组件） | 严格加载、XDG、权限规则、环境引用与优先级通过本地测试；CLI 接线属 DEV-019 |
| DEV-019 | P4 | 完成（只读 CLI 入口） | run／chat、配置命令、三种输入、预授权文件和控制终端确认经本地夹具验证；真实副作用已在 DEV-035 接通 |
| DEV-020 | P4 | 完成（受控验证） | 伪终端与模型替身覆盖连续交互、确认／澄清、模式与权限切换、取消和信号；人工等待暂停活跃计时 |
| DEV-021 | P4 | 完成（运行时与 CLI 受控验证） | 用户级／工作区指令与 Skill 发现、作用域、哈希和刷新通过本地测试；P5 已补充来源持久化 |
| DEV-022 | P5 | 完成（独立存储与受控验证） | SQLite WAL／FULL／外键、事务、历史、意图／结果及事件序号通过本地验证；CLI 接线属 DEV-023 至 DEV-025 |
| DEV-023 | P5 | 完成（正常会话受控验证） | 列表、详情、交互恢复、再次运行、配置指纹和工作区关联已接入；崩溃中断恢复归 DEV-025 |
| DEV-024 | P5 | 完成（受控验证） | 跨进程会话锁、并行查询、初始化竞争、迁移备份及损坏／高版本拒绝已验证 |
| DEV-025 | P5 | 完成（受控验证） | 事务恢复、unknown／悬空消息、结果保存故障与 PLAN 目录边界已验证；后台任务关联归 DEV-035 |
| DEV-026 | P6 | 完成（本机受控验证） | JSON／JSONL、持久事件、控制终端、退出码与恢复序号已验证 |
| DEV-027 | P6 | 完成（本机受控验证） | 已知凭据遮蔽、日志轮转、执行／审核用量与已确认产物已验证 |
| DEV-028 | P6 | 完成（本机受控验证） | 四类任务、三协议、12 组模式／权限与 AC-01 至 AC-23 证据已索引 |
| DEV-029 | P6 | 完成（文档与本机验证） | 使用指南、无凭据配置和精确命令预授权示例已提供；示例通过 CLI 测试 |
| DEV-030 | P7 | 完成（真实服务小规模联调） | 指定模型三协议直接回答、读取工具及回传通过；见真实模型联调记录 |
| DEV-031 | P7 | 完成 | 原生候选 2840cbb 与正式源码 eaade53 的四目标构建及最低环境解包验收通过，见发行记录 |
| DEV-032 | P7 | 完成 | 24 组基线与 24 项验收证据齐备，双语版本说明及已知限制已整理 |
| DEV-033 | P1／P2 | 完成（受控验证） | 运行内后台管理、七工具、确认、取消、共享预算和假模型续跑经本机测试 |
| DEV-034 | P1／P3 | 完成（受控验证） | 三协议后台调用／结果映射、共享预算、清理与运行内事件快照已验证 |
| DEV-035 | P4／P5 | 完成（受控验证） | CLI 后台控制、持久关联、故障恢复与真实工具经本机受控验证 |
| DEV-036 | P1／P2 | 完成（组件与受控验证） | 英文提示词分层、装配、独立审核、来源对照与离线场景；真实模型效果未评测 |
| DEV-037 | P1 | 完成（受控验证） | 非测试 core 循环、权限及后台接线、取消与持久化故障边界已验证 |

## 0.1.0 正式发布

- 标签 `v0.1.0` 固定到 `eaade53649ffffa013e2503b894825e1b72f3dc1`。其运行时代码、提示词与 Cargo 依赖同审查修复 `854be1b` 一致，真实模型复测记录继续适用。
- [正式构建 42383135600](https://github.com/Ariakage/saya-harness/actions/runs/42383135600) 在 Ubuntu 22.04 和 macOS 15 的两个原生架构分别通过 342 项 Rust 测试、四项发行工具测试及三项解包演练。最终包的源码、二进制、脚本哈希和环境记录已交叉核对。
- [GitHub Release](https://github.com/Ariakage/saya-harness/releases/tag/v0.1.0) 已公开发布并设为 Latest，正文和 `RELEASE-NOTES.md` 提供英文及简体中文。四包 SHA256SUMS 校验通过，七项资产全部回下载并逐字节比较通过，公开页面 HTTP 200。
- 回下载的两个 macOS 程序在 macOS 27.2 ARM 主机再次通过版本检查和三项演练，Intel 包经 Rosetta 执行。最低 macOS 15 的证据来自上述原生工作流。包哈希与已知限制见[发行验证记录](release-validation.md)及[版本说明](release-notes-0.1.0.md)。

## P7 最终验收

- 手动原生构建 [42381963924](https://github.com/Ariakage/saya-harness/actions/runs/42381963924) 的 Ubuntu 22.04 ARM／Intel、macOS 15 ARM／Intel 四项全部通过。每个平台通过 342 个 Rust 测试、发行工具测试及对解包二进制的三项离线演练。
- 四包下载后核对源码、包与二进制哈希、测试脚本哈希及实际系统证据，全部一致。macOS 实测版本分别为 Intel 15.7.9、ARM 15.8.2，Linux glibc 2.35。审查后的三协议真实模型复测六项通过。
- DEV-031、DEV-032 和 P7 已完成。正式发布进一步从含最终双语说明的 `eaade53` 重新构建并通过相同验收，发布文件和标签核对结果见上方记录。

## 0.1.0 授权发行准备

- 项目负责人已授权完成 P7 验收、构建四平台发布包、编写版本说明并推送发布 0.1.0。
- 增加手动四平台原生构建与候选包验收流程。候选来源、包和二进制哈希、实际系统及三项离线演练结果按目标保存，上传 Actions artifacts 供本次授权发行核验。
- 工作流、审查后真实模型联调和最终产物验收均已取得证据，见上方正式发布与 P7 验收记录。

## 2026-11-07 风险优先审查

- 基线为干净的 `1f223cb`，原有仓库检查及 337 个测试通过。审查已复现并修复恢复时忽略显式 PLAN、嵌套凭据进入核查／确认、配置写入上限未传递、Responses 混合输出历史失败，以及无效后台请求直接终止运行五项问题。
- 修复后 fmt、Clippy `-D warnings`、341 个测试、release 构建、两组发行工具 Python 测试和 350 个本地文档链接检查通过。回归复现、修复入口、复核范围与未完成项见[审查记录](review-2026-11-07.md)。候选重建证据将随实际结果补充，P7、DEV-031 和 DEV-032 继续待验证。
- 第一次独立复核在 `15a4ac6` 中复现较深／Unicode 转义凭据检测遗漏与无效 cwd 分类遗漏。追加修复 `854be1b` 已通过独立复核、完整检查的 342 个测试及实际 release 程序的五项回归。中间提交 `15a4ac6` 的构建产物保留为审查过程证据。
- 已从 `854be1b` 重新构建四目标候选，校验包、源码声明与二进制哈希，并对四个解包程序执行 smoke、交互演练及五项审查回归，全部通过。Linux Intel 使用 ARM 交叉构建并经 QEMU 运行，macOS Intel 经 Rosetta 运行。候选哈希与最低环境缺口见[发行记录](release-validation.md)。

## D-035：最低支持版本调整（历史记录）

- 项目负责人将 macOS 最低支持版本调整至 15；Linux 基线与四个架构目标不变。CI 构建目标和 macOS 最低环境验证脚本同步调整。
- macOS ARM／Intel 以 `MACOSX_DEPLOYMENT_TARGET=15.0` 重新构建；在 macOS 27 ARM（Intel 使用 Rosetta）检查 Mach-O 声明、离线工具往返与交互恢复并通过。新候选复用先前 Linux 二进制，校验和和验证边界见[候选记录](release-validation.md)。
- [GitHub Actions 42373322880](https://github.com/Ariakage/saya-harness/actions/runs/42373322880) 在 Ubuntu 22.04 ARM／Intel 与 macOS 15 ARM／Intel 全部通过；macOS job 实际检查 15.0 部署目标并执行离线工具往返与交互会话 smoke。CI 构建的二进制与此前本地候选不同；CI 不自动上传产物，尚不能把其结果归因于本地候选。DEV-031／032 与 P7 保持待验证。

## DEV-032 发行检查草案（历史记录）

- [发行检查草案](release-checklist.md)逐项索引 B-01 至 B-24、AC-01 至 AC-24 的现有证据与缺口；[候选版本说明](release-notes-0.1.0.md)记录功能、迁移和平台限制。
- 草案不代替已核实的最低环境证据。DEV-031 和 AC-24 尚未完成，不能将 DEV-032 或 P7 标为完成，也没有授权外部发布。

## DEV-031 候选准备记录（D-035 前的历史）

- 从源码 `a1b1b02` 构建四目标候选包并生成 SHA256；两种 Linux 架构在 Ubuntu 22.04／glibc 2.35 通过 smoke，Intel 使用模拟；两个 macOS 目标在 macOS 27.2 ARM 通过 smoke，Intel 使用 Rosetta，声明最低部署版本均为 13.0。
- 离线工具往返及干净配置、人工批准、退出、列出、恢复、继续输入演练均在四个候选二进制上通过；候选包校验和及内容哈希已核对。详见[发行候选验证记录](release-validation.md)。
- 当时尚未在 macOS 13 运行两个候选，DEV-031 因缺少最低环境证据而未完成；项目负责人确认当时没有 macOS 13 环境。本地候选未签名或发布。D-035 已将最低支持版本调整至 macOS 15，当前验证要求见上方记录。

## DEV-030 交付记录

- 2026-11-07 以用户提供的 `deepseek-flash` 服务运行三协议 CLI 联调，各自完成流式直接回答和 PLAN 读取真实临时文件后回传；临时工作区保持不变。完整协议、请求计数、模型版本和验证限制见[真实模型联调记录](live-model-validation.md)。
- 提供显式运行的 [live_protocol_smoke.py](../scripts/live_protocol_smoke.py)，仅输出状态与计数；凭据通过环境变量提供，测试不会写入项目仓库。服务未提供可独立核实的模型快照版本，本轮不推断模型版本或其他模型的能力。
- 同一模型的 Chat Completions 四模式真实任务经修正夹具后通过：PLAN 不修改工作区，BUILD／CODE 产物经受限容器独立编译运行，WORK 完成三份材料汇总。初次输出上限、缺少父目录和测试容器不可执行挂载导致的失败及重测条件均见联调记录；不是所有协议的四模式质量评测。

## DEV-029 交付记录

- 新增[使用指南](usage.md)、[配置示例](../examples/config.example.toml)与[预授权示例](../examples/approval.example.json)，覆盖构建、三协议配置、会话与权限、迁移恢复、故障处理、已知限制；更新 README、文档索引与路线图的实际状态。验收入口见[验收证据](acceptance.md)。
- 验证：macOS aarch64／Rust 1.99.1 上 `./scripts/check.sh` 通过格式、Clippy、337 项测试与 release 构建。新增真实 CLI 测试验证示例配置检查、新建配置拒绝覆盖和示例预授权的受控命令；36 份 Markdown 的本地链接检查通过。模型使用本地替身，配置检查不连接服务。
- P6 的 DEV-026 至 DEV-029 已完成本机受控验证；P6 提交未在其他平台或远端 CI 上验证。真实模型的完整任务观察、四目标发行构建和最低环境 smoke 属 P7，不以本机替身或较早 CI 代替。

## DEV-028 交付记录

- 新增四类真实 CLI 任务夹具：PLAN 调查 Rust 工程并核对工作区不变；BUILD 实现并编译验证文本统计程序；CODE 先复现错误再修复和运行；WORK 读取三份材料生成报告。三协议的直接回答／工具往返、12 组模式／权限、审核兜底、bypass／deny、恢复权限和失败预算也有端到端测试。
- 新增交互测试验证模型建议不会切换模式、BUILD 运行中切到 PLAN 后写入被拒绝；[验收证据索引](acceptance.md)逐条映射 AC-01 至 AC-23，并区分 CLI、core 与存储证据。
- 验证：macOS aarch64／Rust 1.99.1 的 `./scripts/check.sh` 通过格式、Clippy、335 项测试和 release 构建。模型使用本地替身；本次提交未运行远端 CI、其他平台或真实模型任务验收。

## DEV-027 交付记录

- 已知配置凭据和请求头值在项目上下文、工具结果、会话、事件、日志及 CLI 输出中遮蔽；不能安全修改的结构标识、对象键冲突与不透明签名状态失败关闭。不会识别任意未知秘密或追溯清理旧记录。
- 结构化 warn／error 日志写入本地状态目录，10 MiB 轮转保留 5 个备份；使用跨进程锁并拒绝不安全的日志文件。记录执行／审核请求、工具调用、耗时与可用 token 用量；未报告的用量保持 null。仅在用户配置两种费率且用量完整时给出标记为估算的成本；已确认写入结果才列入 artifacts。
- 验证：macOS aarch64／Rust 1.99.1 的 `./scripts/check.sh` 通过格式、Clippy、321 项测试和 release 构建。测试覆盖多进程日志、轮转及链接拒绝、嵌套和流片段凭据、SQLite／CLI／项目上下文遮蔽、签名状态失败关闭、执行与审核用量、显式费率和前台／后台产物。其他平台与本次远端 CI 未验证。

## DEV-026 交付记录

- `run` 支持 schema v1 JSON 结果与 JSONL 事件；查询使用独立 JSON 信封，控制终端处理确认和澄清，不污染标准输出。模型、工具、权限、输入和终结事件由实际运行产生，业务事件与对应持久状态同事务提交后输出。
- 事件序号按会话持久预留，允许崩溃后留空但不重复；文本 delta 不逐 token 写库。JSONL 在配置有已知凭据时暂存本次可见流直至模型消息完成再脱敏，避免跨片段泄漏；因此该情况下 delta 不保证实时。最终结果保存失败时输出未持久化的失败结果并保留已观察工具结果。
- 验证：macOS aarch64／Rust 1.99.1 的 `./scripts/check.sh` 通过格式、Clippy、307 项测试和 release 构建；真实 CLI 测试覆盖普通结果、执行／恢复序号、流式输出、审批与澄清、非交互退出码、取消、故障注入及分片凭据。全路径脱敏、聚合用量、产物与日志属 DEV-027。

## CI 崩溃恢复测试修复

- GitHub Actions 运行 42342671205 在 Ubuntu x86_64 和 aarch64 的后台进程崩溃恢复测试失败；macOS aarch64 和 x86_64 通过。测试主动终止客户端后，HTTP 测试替身在响应写入失败时退出，导致恢复请求连接失败。
- 动态 HTTP 测试替身在响应期间遇到客户端断开时继续接受下一个请求；新增真实 TCP 重置回归测试，并保留崩溃恢复及不重放验证。本次修复仅改变测试基础设施。
- macOS aarch64／Rust 1.99.1 的 `./scripts/check.sh` 通过格式、Clippy、294 项测试和 release 构建；新增测试对原版替身失败，对修复版通过。修复提交 f3d1f49 的 GitHub Actions 运行 42343813523 在 Ubuntu x86_64／aarch64 和 macOS x86_64／aarch64 四项均通过。

## P5 收尾：项目指令来源持久化

- 每次执行模型请求前保存适用来源和发现的 Skill 路径、类型、作用域、哈希与请求 ID；持久化失败时不发送请求。来源记录不保存指令正文，会话恢复仍读取当前文件而不沿用旧授权。
- 验证：macOS aarch64／Rust 1.99.1 下 `./scripts/check.sh` 通过格式、Clippy、293 项测试及 release 构建；34 份 Markdown 文件的本地链接检查通过。受控 CLI 测试覆盖同一会话跨运行的文件变化、来源与哈希变化、正文不出现在会话详情，以及 SQLite 故障阻断模型请求。旧会话在升级前可能没有来源记录；恢复后新请求开始记录。
- P5 各任务已完成；本机验证不等同其他平台或远端 CI 的验收。运行 JSON／JSONL、全路径脱敏和完整集成验收属于 P6。

## DEV-035 交付记录

- 后台启动意图在接受调度前保存；父调用与子工具通过关联标识连接。会话详情展示持久后台状态；CLI 可在运行中查看、批准和取消任务，正常结束会等待清理并提示。普通 CLI 现在可经权限和持久化调度执行真实文件写入与命令。
- 验证：macOS aarch64／Rust 1.99.1 上 `./scripts/check.sh` 通过格式、Clippy、291 项测试和 release 构建。本机受控测试覆盖后台审批、查看与取消、三种权限下 PLAN 写入／命令拒绝、精确命令预授权及跨运行失效、意图保存失败阻止执行、结果保存失败和强制终止后恢复不重放。后台进程在 Saya 被强制终止后可能仍运行；未知结果不表示进程已停止，也不能自动重放。
- 范围：JSON／JSONL 运行输出与全路径脱敏属 P6；项目指令来源持久化由后续收尾提交完成。此次未运行远端 CI 或其他平台验收。

## DEV-025 交付记录

- 日期：2026-11-07。中断恢复在会话锁内原子修复运行和工具记录，区分未执行、待确认、输入中断与结果不明，不恢复授权或重放。会话位置安全检查点先于数据库元数据更新；旧会话的遗留 WAL 使用校验过的只读内存快照，检查点缺失时数据库文件和 WAL 各限 512 MiB。保存结果失败时，CLI 展示脱敏后的已观察结果并停止交互会话。
- 验证：macOS aarch64／Rust 1.99.1 下完整检查通过格式、Clippy、281 项测试与 release 构建；本地真实进程强制终止验证遗留 WAL、未知结果和无重放，SQLite 触发器验证恢复回滚与结果保存失败，伪终端验证交互会话停止。受控三协议请求验证工具结果闭合；PLAN 的三个存储根冲突与恢复前只读检查均有测试。
- 范围：后台启动回执与子任务关联、持久状态和 CLI 控制归 DEV-035；尚未开放 CLI 真实写入／命令、验证其他平台或远端 CI。

## DEV-023／DEV-024 交付记录

- 日期：2026-11-07。会话恢复在只读预检后取得跨进程锁，再重新验证模式和工作区；活跃 WAL 通过 SQLite 只读共享内存预检。数据库并发初始化使用独立锁；schema 1 升级到 2 前创建、校验并同步 SQLite 在线备份，迁移事务失败不覆盖原数据。
- 验证：macOS aarch64／Rust 1.99.1 的 `./scripts/check.sh` 通过格式、Clippy、265 项测试及 release 构建。真实两个进程验证同会话竞争、不同会话恢复、查询和强制终止后的锁释放；多进程验证同时初始化。迁移成功、迁移 SQL 失败、缺表、损坏和高版本拒绝均有测试；伪终端验证交互恢复等待输入、原模式及本次权限。
- 范围：崩溃后无活跃持有者的 WAL 预检及未完成运行恢复仍属于 DEV-025；尚未验证其他平台或远端 CI，普通 CLI 的写入和命令继续关闭。

## 会话持久化预览

- 日期：2026-11-07。CLI 接入本地会话创建、列表、详情、交互恢复和 `run --session`；恢复比较无密钥 profile 指纹，沿用工作模式并采用本次权限。使用 OS 文件锁作为初步会话写锁，PLAN 启动或切换检查目录；真实工作区副作用仍关闭。
- 验证：macOS aarch64／Rust 1.99.1 的 `./scripts/check.sh` 通过格式、Clippy、255 项测试及 release 构建。真实 CLI 子进程和本地模型替身覆盖会话持久化、再次运行、配置变更接受／拒绝及 PLAN 恢复目录检查；同进程两个 SQLite 连接覆盖锁冲突和读取。
- 未完成：恢复前只读预检遇到非空 WAL 暂时拒绝，即使 WAL 属于其他会话；跨进程锁竞争、损坏／迁移、崩溃后的悬空调用与 unknown 恢复仍需 DEV-024／DEV-025，不能据此认为 P5 完成。未验证远端 CI 或其他平台。

## 动态提示词与缓存补充

- 日期：2026-11-07。系统段按请求注入宿主时钟、时区偏移、平台、工作区和运行模式；项目指令及显式传入的偏好／记忆保留为低信任上下文。Anthropic 可选静态块缓存断点，同实例并发冷请求等待首个响应事件；OpenAI 兼容协议保持静态前缀。
- 验证：macOS aarch64／Rust 1.99.1，隔离的暂存源码执行 `./scripts/check.sh`，通过格式、Clippy、250 项测试和 release 构建。覆盖实际三协议请求结构、动态时间、显式上下文、敏感内容拒绝、静态断点及并发等待／流丢弃释放。未验证真实厂商命中率、跨进程协调或其他平台。

## DEV-022 交付记录

- 日期：2026-11-07；状态：完成（独立存储组件）。新增 [SQLite 存储](storage.md)，通过 bundled `rusqlite` 提供 WAL、FULL 同步、外键、事务和会话／运行／工具／确认／事件持久化。事件序号及状态日志按会话保持递增，不逐 token 保存文本 delta。
- 验证：macOS aarch64／Rust 1.99.1 下 `./scripts/check.sh` 通过格式、Clippy、242 项测试及 release 构建。临时数据库测试覆盖重启读回、事务回滚、事件序号、已知秘密失败关闭、文件权限与链接拒绝、损坏和更高版本拒绝；真实 core 配合临时工作区故障注入，验证意图保存失败不写文件、结果保存失败保留已观察结果并停止循环。
- 范围：已知秘密目前触发事务失败，不是完整脱敏；SQLite 尚未接入普通 CLI。跨进程锁、恢复、迁移和 PLAN 目录边界待 DEV-023 至 DEV-025；未验证远端 CI 或其他平台。

## P4 退出核对

- DEV-018 至 DEV-021 分别完成并提交；配置优先级与缺失引用、run／chat／config、三种任务输入和控制终端分别有配置测试、CLI 子进程和本地 HTTP 夹具证据。
- 伪终端验证多轮历史、profile 选择、四模式控制、权限只在空闲时修改、批准失效、`ask_user`、`//`、`/status`／`/cancel`／`/help`／`/exit`，以及活动运行 Ctrl-C 取消后回到提示符、空闲 Ctrl-C 退出；core 可控时钟验证批准和澄清等待不消耗活跃预算。
- 项目上下文覆盖用户级和工作区指令／Skill 来源、优先级、子目录作用域、哈希、刷新、权限隔离及三协议请求。`./scripts/check.sh` 通过格式、Clippy、233 项测试及 release 构建；release 二进制的版本、帮助、配置初始化／校验与空任务错误 smoke 通过。本次验证平台为 macOS aarch64／Rust 1.99.1。
- P4 按路线图使用内存会话和只读 CLI：真实工作区写入／命令、SQLite、恢复及 DEV-035 的持久化后台状态属于 P5，JSON／JSONL 与全路径脱敏属于 P6。未运行本次远端 CI、其他平台验收或 P4 完整真实模型任务；第二次 Ctrl-C 强制退出分支已实现，未在长时间清理窗口进行定时端到端验证。

## DEV-020 交付记录

- 日期：2026-11-07；状态：完成（受控验证）。终端统一分发会话输入、工具确认和澄清；支持工作模式切换、空闲权限修改、状态、取消、帮助、退出和斜杠转义。等待批准期间模式变化会使原批准失效。
- 验证：`./scripts/check.sh` 通过格式、Clippy、233 项测试及 release 构建。伪终端覆盖多轮历史、profile 选择、手动批准、问题回答、运行中权限修改拒绝、最后一个待切换模式、取消、Ctrl-C 和无终端处理；暂停时钟验证人工批准／澄清等待不消耗活跃时间；单元测试验证第一次中断取消、第二次中断选择强制退出。
- 修复：在等待批准时取消的工具闭合为 `not_executed`；已展示但未回答的澄清闭合为 `input_interrupted`；已派发且结果不明的工具仍保持 `outcome_unknown`，不自动重放。
- 未验证／后续：第二次 Ctrl-C 强制退出在清理期间的实际时序、其他平台和远端 CI 未覆盖；持久化、完整输出和 DEV-035 仍按 P5／P6 处理。

## DEV-021 交付记录

- 日期：2026-11-07；状态：完成（运行时与 CLI 受控验证）。[项目上下文说明](project-context.md)列出加载位置、优先级和边界。
- 变更：默认加载用户级和工作区约定指令、发现 Skill 元数据；工作区与同作用域 `.saya` 优先。来源采用固定目录句柄和 SHA-256；目标路径指令或来源变更时先刷新模型上下文，人工批准后及后台派发前再次检查。项目内容作为模型历史中的数据，不修改硬权限或运行配置。增加 `serde_yaml_ng` 解析 Skill 元数据；同步英文提示词与 `docs/prompt` 快照。
- 验证：macOS aarch64／Rust 1.99.1 下 `./scripts/check.sh` 通过格式、Clippy、215 项测试及 release 构建。测试覆盖 10 项来源／作用域／解析和符号链接边界、模型调用及前台／后台人工批准期间变化、三协议 CLI 请求中的数据传递及 Manual Approval 不被项目指令覆盖；提示词快照与运行时源逐字一致。
- 范围：Skill 仅发现元数据，正文读取仍遵守路径权限；来源记录目前在运行内存中，跨进程保存与恢复属 P5。未验证真实模型对项目指令的遵循程度、其他平台或远端 CI；文件变化检查并非文件系统原子快照。

## DEV-020 人工澄清阶段记录

- 日期：2026-11-07；状态：部分完成，DEV-020 保持进行中。`ask_user` 经过同一权限与一次性许可判断，Manual Approval 包括此工具；非交互返回 `needs_input` 和问题，交互通过控制终端取得回答并传回模型。等待人工回答期间暂停活跃时间预算。
- 验证：macOS aarch64／Rust 1.99.1 下 `./scripts/check.sh` 通过格式、Clippy、200 项测试及 release 构建。模型替身验证澄清、人工等待预算和 Manual Approval；本地 HTTP 与伪终端夹具验证非交互退出码、逐次批准、问题展示与回答回传。
- 范围：问题目前只在内存历史中保留，不具备进程退出后的恢复；REPL、工作模式与权限模式交互控制、信号行为及持久化仍待后续任务。未运行远端 CI 或其他平台验证。

## DEV-019 交付记录

- 日期：2026-11-07；状态：完成（只读 CLI 入口）。`saya run` 和 `saya`／`saya chat` 提供单次与连续会话，未配置默认 profile 时交互选择；`config init` 不覆盖已有文件，`config check` 校验选定配置。三种任务输入互斥并保留输入文本，精确预授权文件可载入；单次默认非交互，显式交互通过独立控制终端处理确认。
- 验证：macOS aarch64／Rust 1.99.1 下 `./scripts/check.sh` 通过格式、Clippy、226 项测试及 release 构建。CLI 子进程、本地 HTTP 与伪终端覆盖输入、三协议请求、预授权文件、控制终端与 stdin 分离、无终端回退及连续会话；模型服务均为本地夹具。
- 限制：只读运行时在 P5 持久化完成前拒绝所有写入和命令，包括预授权操作；内存会话不能跨进程恢复。JSON／JSONL 属 P6，本次未运行远端 CI 或其他平台验证。DEV-020 的控制命令与信号仍在独立核对。

## DEV-019 只读 CLI 预览记录

- 日期：2026-11-07；状态：部分完成，DEV-019 保持进行中。[预览说明](cli-preview.md)列出可用命令与限制。
- 变更：支持配置初始化／校验、三种互斥任务输入、单次模型运行、流式文本、精确预授权文件与控制终端人工确认。core 提供只读运行入口，拒绝模型伪造的前台或后台写入／命令；内存会话不具备恢复能力。新增 clap 用于 CLI 参数解析。
- 验证：macOS aarch64／Rust 1.99.1 下 `./scripts/check.sh` 通过格式、Clippy、196 项测试及 release 构建；本地 HTTP 夹具验证 CLI 的 Chat Completions 只读工具往返、流式输出和非交互确认，伪终端验证任务 stdin 与人工确认终端分离；core 夹具覆盖伪造写入、命令和后台写入。其他协议的 CLI 路径、其他平台及远端 CI 未验证。
- 剩余：交互 chat、完整人工输入／控制命令、项目指令、JSON／JSONL 与持久化；不能将只读预览当作完整 MVP。所有真实副作用仍等 P5。

## DEV-018 交付记录

- 日期：2026-11-07；状态：完成（core 配置组件与受控验证）。
- 变更：新增[配置加载组件](../crates/saya-core/src/config.rs)，解析单一 TOML 文件、profile、XDG 和参数优先级，验证凭据环境引用、端点、权限规则与预算；[配置说明](configuration.md)记录当前边界。配置错误不回显输入或环境变量值；不创建配置、调用模型或执行工具。
- 验证：macOS aarch64／Rust 1.99.1 下 `./scripts/check.sh` 通过格式、Clippy、189 项测试及 release 构建。7 项配置测试覆盖文件选择与不合并、CLI／环境／文件优先级、XDG、三协议、无默认模型、权限规则、错误脱敏及凭据引用错误。
- 未验证／未实现：本阶段只提供 core 接口，CLI、配置向模型与运行时的接线仍属 DEV-019；本次未运行其他平台或远端 CI。
- 下一任务：DEV-019。

## P3 真实服务抽样与退出核对

- 日期：2026-11-07；使用项目负责人提供的 `deepseek-flash` 测试配置，对 Chat Completions、Responses、Anthropic Messages 分别执行流式文本、无副作用的工具调用和工具结果回传；三协议均通过。Anthropic 抽样使用 `2026-10-21`；另一个已选版本仅有离线夹具证据。凭据只通过进程标准输入提供，未写入仓库或测试日志。
- Responses 首次抽样遇到未识别的 `response.reasoning_text` 事件；已按协议将其作为非展示进度处理，并增加离线回归，复测通过。此抽样仅覆盖一个服务和模型，不替代其他供应商、模型、权限场景及完整真实任务的验收。
- P3 退出条件：DEV-013 至 DEV-017 各自完成并提交，三协议均有独立的文本／工具／错误／历史往返离线夹具；DEV-034 的后台协议、预算与运行内事件快照也已验证。`./scripts/check.sh` 在本机通过格式、Clippy、182 项测试及 release 构建。
- 范围限制：本次未运行远端 CI 或其他平台；CLI、SQLite、完整 schema-v1／JSONL 事件和全路径用量／脱敏仍属 P4 至 P6，发行环境及更广泛模型联调属 P7。

## DEV-034 交付记录

- 日期：2026-11-07；状态：完成（core 与三协议受控验证）。
- 变更：增加可通过运行时句柄读取的后台接受调度、取消请求和完成事件快照；事件按锁内观察顺序编号，接受调度不等于副作用已开始。保留三协议各自的工具调用／结果配对与运行结束清理。
- 验证：macOS aarch64／Rust 1.99.1 下 `./scripts/check.sh` 通过格式、Clippy、181 项测试及 release 构建。三协议分别在真实 core 和本地 HTTP 夹具上验证后台运行时模型续跑、状态查询、运行结束清理、模型／工具上限和审核共享预算；并发任务、PLAN 取消及存储故障的事件顺序也有受控验证。
- 未验证／未实现：此事件快照仅在本次运行内存中可用，不是持久化 schema-v1／JSONL 事件流；完整输出与持久化仍属 P5／P6。尚未完成真实供应商联调及其他平台验证。
- 下一步：真实服务联调与 P3 退出条件核对。

## DEV-017 交付记录

- 日期：2026-11-07；状态：完成（profile 路由与受控验证）。
- 变更：新增固定执行 profile 的路由器，支持独立审核复用执行 profile 或选择其他已注册 profile；会话、执行 profile、模型及协议不一致时停止运行。审核禁止工具并沿用共享预算、输出限制、脱敏和失败转人工确认。
- 验证：macOS aarch64／Rust 1.99.1 下 `./scripts/check.sh` 通过格式、Clippy、178 项测试及 release 构建。4 项 profile 核查测试覆盖三协议默认／指定 profile、1024 token 上限、无工具、凭据脱敏、缺失 profile 兜底、运行时跨协议审核拒绝和配置不匹配。
- 未验证／未实现：真实供应商联调、持久化配置加载和三协议后台事件仍待后续验证。
- 下一任务：DEV-034。

## DEV-016 交付记录

- 日期：2026-11-07；状态：完成（Anthropic Messages 适配与受控验证）。
- 变更：新增两版本 Messages 的完整 JSON／SSE 文本与本地工具适配，保留签名和不透明 thinking 内容块，按顺序合并同轮工具结果；不发送已移除的采样参数。截断工具参数不会派发，并依 stop reason 区分输出／上下文限额。
- 验证：macOS aarch64／Rust 1.99.1 下 `./scripts/check.sh` 通过格式、Clippy、174 项测试及 release 构建。6 项本地夹具覆盖两个版本头、文本与 usage、签名状态、多工具往返、异常及限额，和真实 core 写文件闭环。
- 未验证／未实现：尚未使用真实供应商联调；profile 核查与三协议后台夹具仍待 P3 后续任务。
- 下一任务：DEV-017。

## DEV-015 交付记录

- 日期：2026-11-06；状态：完成（Responses 适配与受控验证）。
- 变更：新增 Responses 完整 JSON 与 typed SSE 适配；明确 `store: false`，不依赖服务端 response 链。按序保存原始消息、加密 reasoning、函数调用与对应语义项；使用 `call_id` 回传工具结果并交叉检查分片、完成项与最终结果。
- 验证：macOS aarch64／Rust 1.99.1 下 `./scripts/check.sh` 通过格式、Clippy、168 项测试及 release 构建。6 项 Responses 夹具覆盖文本、usage、原始项与不透明状态往返、完整响应、流终止、拒绝／截断／数据冲突及真实 core 写文件闭环。
- 未验证／未实现：尚未对真实供应商联调；Anthropic、profile 路由与三协议后台夹具仍待 P3 后续任务。
- 下一任务：DEV-016。

## DEV-014 交付记录

- 日期：2026-11-06；状态：完成（Chat Completions 适配与受控验证）。
- 变更：新增 Chat Completions 适配器，显式 `store: false`；支持完整 JSON 与 SSE、文本、交错工具参数、call ID／工具结果往返、usage、严格 schema 的安全校验及兼容选项。添加以 profile 指定窗口为准的上下文估算与输出预留；运行时识别非文本流事件，防止已接收响应后重试，并区分输出／上下文限额。
- 验证：macOS aarch64／Rust 1.99.1 下 `./scripts/check.sh` 通过格式、Clippy、162 项测试和 release 构建。8 项协议夹具覆盖文本／usage、多工具与历史回传、完整响应、截断／拒绝／无效参数、上下文／strict 保护、会话模型／协议一致性及驱动真实 core 写文件闭环；另验证非文本部分响应后不重试和限额状态映射。
- 未验证／未实现：尚未用真实供应商请求联调；其他协议和 profile 路由仍在 P3，CLI、持久化及最终结果用量汇总在后续阶段。
- 下一任务：DEV-015。

## D-032 Manual Approval 交付记录

- 日期：2026-11-06；状态：core 及受控验证完成，CLI 仍待 P4。
- 变更：`manual` 对每次工具调用要求人工批准，包括只读、搜索及后台启动／查询／取消；后台控制复用单次许可及模式版本检查。精确命令预授权仍仅适用于对应操作；PLAN、显式 deny 和路径边界优先。权限 prompt 与基线同步，旧的 `review` 序列化值可读取为 `manual`。
- 验证：macOS aarch64／Rust 1.99.1 下 `./scripts/check.sh` 通过格式、Clippy、152 项测试和 release 构建。测试覆盖 12 组模式矩阵、七个本地工具、后台控制逐次确认和拒绝、非交互读取／后台查询停止及旧值兼容。
- 未验证／未实现：实际 CLI 选择与交互确认仍属 P4；未运行远端 CI 或其他平台检查。

## DEV-013 交付记录

- 日期：2026-11-06；状态：完成（传输层与受控验证）。
- 变更：新增复用的异步 HTTP／SSE 传输，保留 API 根路径前缀，校验 HTTPS／回环 HTTP、鉴权和环境头；禁止自动重定向和底层重试。流解析支持跨块数据、UTF-8、多行事件与有界缓冲，状态错误不包含凭据或 provider 正文。引入 `reqwest`、`bytes`、`httpdate` 用于 HTTP、分块读取和 Retry-After 日期解析。
- 验证：macOS aarch64／Rust 1.99.1 下 `./scripts/check.sh` 通过格式、Clippy、148 项测试及 release 构建。9 项本地传输测试覆盖请求路径、两种鉴权、版本／环境头、分片流、无效输入／大小限制、HTTP 错误和 Retry-After、重定向、取消、空闲超时及隔离子进程中的系统代理。
- 未验证／未实现：真实模型、三协议映射、其他平台及远端 CI 尚未验证；完整上下文预算与全路径日志脱敏仍属后续阶段。
- 下一任务：DEV-014。

## DEV-001 交付记录

- 日期：2026-11-06；状态：完成。
- 变更：根目录 Cargo workspace、`rust-toolchain.toml`、`Cargo.lock`，以及 `crates/saya-core` 和 `crates/saya-cli`。CLI 仅输出未实现提示并返回失败，不提供产品命令。已有 MIT 许可证保留。
- 验证环境：本机 macOS aarch64，Rust 1.99.1；执行 `cargo check --workspace --locked`、`cargo build --workspace --locked`、`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo test --workspace --locked`、`cargo build --workspace --release --locked` 均通过。测试执行了 0 个用例，不代表产品行为已验证。
- 未验证：其他目标平台、CI 与产品功能；DEV-002 仍待建立测试基础和 CI。
- 下一任务：DEV-002。

## DEV-002 交付记录

- 日期：2026-11-06；状态：完成。
- 变更：`scripts/check.sh` 统一格式、Clippy、测试及 release 构建；`.github/workflows/ci.yml` 配置四个原生 runner；`crates/saya-core/tests` 提供基于 `tempfile` 的临时工作区辅助与模型、核查、协议夹具目录。夹具目录尚无实现或协议数据。
- 本地验证：macOS 27.2 aarch64、Rust 1.99.1 下运行 `./scripts/check.sh` 通过，临时工作区隔离和自动清理测试 1 项通过；`go run github.com/rhysd/actionlint/cmd/actionlint@latest .github/workflows/ci.yml`（actionlint 1.7.12）通过；其余产品行为尚无测试。
- 远端验证：[GitHub Actions 运行 42119615149](https://github.com/Ariakage/saya-harness/actions/runs/42119615149) 对提交 `db90bc4b1d7e95d063a0fc474a18c9fcbb7fc929` 手动触发；Ubuntu 22.04 x86_64／aarch64、macOS 15 aarch64／x86_64 四项均通过格式、Clippy、测试和 release 构建。
- 未验证：macOS 13 最低运行环境、发行产物及 smoke 测试仍由 DEV-031 验证；CI 通过不表示产品功能已实现或发行就绪。
- 下一任务：DEV-003。

## DEV-003 交付记录

- 日期：2026-11-06；状态：完成（核心接口与数据定义）。
- 变更：`crates/saya-core/src/contract.rs` 定义会话、运行、模式、UUIDv7 标识、协议历史与不透明项、工具状态、schema v1 事件／结果及错误；`boundary.rs` 定义模型流、工具、取消、存储及事件输出接口。新增 serde、serde_json、uuid 依赖。
- 本地验证：macOS aarch64／Rust 1.99.1 执行 `./scripts/check.sh`，格式、Clippy、测试及 release 构建通过；6 项核心契约测试和 1 项原有临时工作区测试通过。
- 未验证／未实现：当前改动未运行远端 CI；接口本身不执行事件排序、schema 版本与时间戳校验、权限、取消、持久化、重试或副作用控制。实际循环从 DEV-004 开始，协议与 SQLite 分别在后续任务实现；尚未形成稳定 Rust SDK。
- 下一任务：DEV-004。

## DEV-004 交付记录

- 日期：2026-11-06；状态：完成（受控测试入口）。
- 变更：`crates/saya-core/src/memory.rs` 提供非持久的原子内存存储；`prototype.rs` 和 `prototype/tests.rs` 提供仅测试构建可用的假模型串行循环、工具注册和替身。执行前保存意图，执行后原子保存结果与历史；保存失败停止继续调度。
- 本地验证：macOS aarch64／Rust 1.99.1 执行 `./scripts/check.sh`，格式、Clippy、测试及 release 构建通过；7 项新测试覆盖直接回答、连续工具的顺序与回传、未知工具／参数错误／执行失败的成对结果、模型错误、重复注册、注入保存失败及内存事务原子性；既有 7 项测试亦通过。
- 未验证／未实现：原型不进入非测试构建或 CLI，不执行真实工具；尚无完整的权限、取消、超时、可配置预算、错误结束结果、事件与持久化。原型的 64 模型请求／256 工具调用上限只防止测试无限循环；尚不具备 B-15 全部预算。执行后保存失败时当前原型返回存储错误，但尚未提供带可输出工具结果的最终结果，须在后续运行时与持久化任务中补齐。当前改动未运行远端 CI。
- 下一任务：DEV-005。

## DEV-005 交付记录

- 日期：2026-11-06；状态：完成（受控测试入口）。
- 变更：测试专用 `prototype/control.rs` 实现请求、工具与活跃时间预算及可暂停计时；假循环接入模型连接／流／总超时、有限重试（含抖动与 Retry-After 上限）、取消信号、工具协作清理、重复失败停止和结果状态映射。部分模型输出失败时记录 `AssistantPartial`。`BoundaryError` 增加可选 Retry-After 元数据。
- 本地验证：macOS aarch64／Rust 1.99.1 执行 `./scripts/check.sh`，格式、Clippy、测试、release 构建均通过；20 项原型测试、6 项契约测试和 1 项临时工作区测试通过。使用暂停的 Tokio 时钟覆盖预算计数／暂停、连接／空闲／总／活跃时间、重试和部分输出、工具超时及取消信号。
- 未验证／未实现：以上逻辑仅在测试构建中使用，不执行真实模型、真实工具或 CLI。人工确认／澄清暂停与实际调度尚未接入；真实进程组的强制清理由 DEV-009 实现；执行后保存失败仍缺少可输出工具结果，悬空调用闭合和持久恢复由后续存储任务处理。schema 事件时序、完整权限、工具字节上限及真实协议仍待对应任务；远端 CI 尚未验证本次改动。
- 下一任务：DEV-006。

## DEV-006 交付记录

- 日期：2026-11-06；状态：完成（受控测试入口）。
- 变更：`crates/saya-core/src/mode.rs` 提供普通构建可用的当前／待切换工作模式和确认版本状态机；测试循环在模型与工具调用边界应用用户请求、保存模式并在派发前重新判断 PLAN。四种模式提供不同的任务指令；模型的切换建议不自动改变状态。
- 本地验证：macOS aarch64／Rust 1.99.1 执行 `./scripts/check.sh`，格式、Clippy、测试和 release 构建通过；33 项原型测试、6 项契约测试和 1 项临时工作区测试通过。测试覆盖独立并发用户请求、最后请求优先、模型／工具调用期间切换、模型重试、确认版本失效、取消优先、模式保存失败、保留历史及三种权限设置下的 PLAN 只读工具重查。
- 未验证／未实现：模式控制器可在普通构建使用，但循环与模式限制只在测试构建接线，模式限制依赖测试工具分类；真实工具与权限引擎、人工确认等待、UI 控制命令、模式事件及跨进程持久恢复尚未接入。确认版本测试不代表完整授权流程已实现。执行后保存失败时的可输出工具结果和悬空调用闭合仍待后续处理；本次改动未运行远端 CI。
- 阶段余项：P1 的循环当前位于 `#[cfg(test)]`，还不能供 CLI 或未来界面复用；在具备安全受控入口并验证阶段退出条件之前，不将 P1 标为完成。

## DEV-007 交付记录

- 日期：2026-11-06；状态：完成（工具实现与本机验证）。
- 变更：`crates/saya-core/src/tools/read.rs` 提供 `read_file`、`list_dir`、`find_files`、`search_text` 及 `Tool` 适配器；使用 cap-std 目录句柄限制原生文件访问，按允许根校验路径、符号链接和搜索用 `.gitignore`，支持 bypass 的显式根豁免；提供 UTF-8、完整 SHA-256 版本、稳定排序、忽略规则、分页／截断与基线上限。Tokio 阻塞池避免同步文件遍历占用异步执行线程，工作循环检查取消。
- 本地验证：macOS aarch64／Rust 1.99.1 执行 `./scripts/check.sh`，格式、Clippy、测试及 release 构建通过；9 项只读工具测试通过，覆盖根越界与符号链接、`.gitignore` 越界与嵌套否定、UTF-8／二进制、版本、截断、忽略项、bypass 和适配器取消。现有 40 项测试亦通过。
- 未验证／未实现：四项工具尚未接入非测试 Agent 循环、真实权限引擎或 CLI；根检查不等于操作系统沙箱，进程隔离不在 MVP。取消在文件读取／遍历检查点协作处理，不保证能中断内核中阻塞的单次文件系统调用。其他目标平台与本次远端 CI 未验证。
- 下一任务：DEV-008；P1 仍需补齐非测试构建的可复用核心循环。按用户要求并行推进 P2，不将 P1 或 P2 标为完成。

## DEV-008 交付记录

- 日期：2026-11-06；状态：完成（工具实现与受控测试）。
- 变更：`crates/saya-core/src/tools/write.rs` 提供带授权钩子和意图／结果存储的 `write_file`、`apply_patch` 适配器；同目录临时文件替换、完整版本校验、权限保留、单文件统一补丁及新文件不覆盖。保存执行结果失败时，`BoundaryError` 保留可观察的工具结果。
- 本地验证：macOS aarch64／Rust 1.99.1 执行 `./scripts/check.sh`，格式、Clippy、测试和 release 构建通过；7 项写入测试覆盖新建与冲突、权限、补丁、越界／大小、PLAN 拒绝、意图失败无写入、授权失效及执行后保存失败结果保留；其余 49 项测试通过。
- 未验证／未实现：CLI 和非测试 Agent 循环尚未接入；内存存储仅用于临时测试，真实工作区副作用须待持久化与完整权限调度接入。现有文件版本校验与重命名并非针对任意外部并发修改的原子比较交换；最终运行时还须消费并对外报告保存失败时保留的结果。远端 CI 与其他目标平台本次未验证。
- 下一任务：DEV-009。

## DEV-009 交付记录

- 日期：2026-11-06；状态：完成（工具实现与受控测试）。
- 变更：`crates/saya-core/src/tools/exec.rs` 提供直接 argv 的命令适配器，包含根目录 cwd 校验、目录句柄切换、受控环境、已知凭据脱敏、超时、两路输出上限与持续排空、进程组取消／强制清理及意图／结果存储。引入 nix 与 Tokio process 能力；不隐式使用 shell 或继承交互终端。
- 本地验证：macOS aarch64／Rust 1.99.1 上 `./scripts/check.sh` 通过格式、Clippy、全部测试与 release 构建；7 项受控进程测试覆盖直接 argv、两路截断、凭据过滤、PLAN／目录／意图拒绝、取消、结果保存失败和忽略 TERM 的进程组在 5 秒后强制终止。另有 1 项脱敏单元测试及先前 56 项测试通过。最后一处可执行文件路径解析修订后，命令测试、Clippy 和 release 构建再次通过。
- 未验证／未实现：命令进程不处于操作系统沙箱，进程组不能保证约束主动脱离进程组的后代；根检查仅约束启动目录，不限制子进程文件系统或网络。实际权限规则、预授权、持久化运行时和 CLI 尚未接入；本次未运行远端 CI 或其他目标平台测试。
- 下一任务：DEV-010。

## DEV-010 交付记录

- 日期：2026-11-06；状态：完成（规则引擎与失败关闭门禁）。
- 变更：`crates/saya-core/src/permissions.rs` 解析严格 TOML 权限片段，规范化工作区、额外根和可执行文件，执行 PLAN、路径以及 deny ＞ ask ＞ allow 规则；auto 未匹配命令返回模型核查，当时的 review 写入／命令返回人工确认（此范围已由 D-032 替代），bypass 仍保留 PLAN 与显式 deny。`PolicyGate` 将非直接允许的决策作为失败关闭结果提供给写入与命令适配器。
- 本地验证：macOS aarch64／Rust 1.99.1 上 `./scripts/check.sh` 通过格式、Clippy、全部测试与 release 构建；6 项权限测试覆盖四工作模式 × 三权限模式、规则优先级、额外根／符号链接、可执行路径与 argv 前缀以及失败关闭。调整 glob 分隔符语义后，权限测试和 Clippy 再次通过；其余 64 项测试在上一次完整检查通过。
- 未验证／未实现：当前门禁对需核查或确认的操作只拒绝派发，尚未提供模型核查、交互确认、预授权或 needs_confirmation 输出；这些由 DEV-011、DEV-012 及后续 CLI 完成。生产核心循环尚未接入，路径审批与外部并发文件系统变更之间的原子绑定还需在后续派发集成中核查。未运行本次远端 CI 或其他目标平台测试。
- 下一任务：DEV-011。

## DEV-011 交付记录

- 日期：2026-11-06；状态：完成（独立核查组件与替身验证）。
- 变更：`crates/saya-core/src/review.rs` 构造无工具、无执行历史的独立核查请求；默认执行 profile、1024 输出 token，支持指定核查 profile，参数以规范化数据传入。严格解析 allow／ask／deny，错误转人工；取消和预算耗尽终止，核查及重试使用可与执行调用共享的请求／活跃时间预算。已知凭据从摘要与原因脱敏；操作参数含已知凭据时转人工而不提交不完整视图给模型。
- 本地验证：macOS aarch64／Rust 1.99.1 执行 `./scripts/check.sh`，格式、Clippy、测试和 release 构建通过；7 项新增测试覆盖隔离、profile、注入数据、严格解析／无工具、失败兜底、重试与共享预算、部分响应、取消和超时。注入数据测试补强后，核查测试及 Clippy 再次通过；其余 70 项测试在上一次完整检查通过。全部使用模型替身，无真实网络调用。
- 未验证／未实现：真实模型协议适配、将同一预算实例接入正式执行循环、用户确认和 `needs_confirmation` 输出仍待后续任务；核查结论必须在派发前再次结合当时模式、权限及操作上下文验证。本次未运行远端 CI 或其他目标平台测试。
- 下一任务：DEV-012。

## DEV-012 交付记录

- 日期：2026-11-06；状态：完成（授权组件和受控适配器验证）。
- 变更：`crates/saya-core/src/approvals.rs` 提供运行内一次性授权、严格的预授权文件、拒绝记忆、确认请求与失效检查；许可绑定运行、调用、规范化参数、工作模式与权限版本，命令还绑定 cwd 和过滤后环境摘要。写入和命令适配器在意图保存前及派发前检查授权；确认期间变更、取消和符号链接替换不会沿用许可。权限设置仅允许在绑定控制器空闲时变更，同一并发确认请求复用记录。
- 本地验证：macOS aarch64／Rust 1.99.1 执行 `./scripts/check.sh`，格式、Clippy、93 项测试及 release 构建通过；16 项授权测试包括真实临时文件和受控进程、授权单次使用、准确匹配、拒绝、模式／权限／取消失效、并发及脱敏。
- 未验证／未实现：确认请求仅通过抽象 Store 在测试内存存储验证；SQLite、终端交互与正式运行时未接入，历史授权不恢复的跨进程行为尚待持久化任务验证。工具派发入口已在受控夹具验证；正式 Agent 循环、终端与持久化尚未接入，分别由 P1、P4、P5 跟进。远端 CI 和其他目标平台本次未验证。
- 后续组合验证：`cargo test -p saya-core --test approvals mode_permission_matrix` 覆盖 12 种模式组合，受控夹具先判断只读权限后执行真实读适配器，再经实际授权组件执行临时文件写入；PLAN 拒绝写入、review 需要确认。现已通过一次性许可门禁执行只读工具；该夹具不是正式派发器，不能证明所有生产路径均已接入权限规则。
- 后续只读接线：`ReadTool::with_gate` 在读取前检查规范化目标与单次许可，支持显式 deny／ask；重复使用许可与读取前重定向符号链接均被拒绝。`./scripts/check.sh` 通过；追加的符号链接测试单独通过。低层 `ReadTool::new` 仍要求调用方自行实施权限，不代表正式运行时已接线；检查与内核打开之间的外部并发替换不构成原子安全边界。
- 后续安全修订：权限模式与版本绑定到同一个受控状态，禁止调用方仅修改运行上下文而绕过 review；只有绑定控制器空闲时才能变更。`./scripts/check.sh` 再次通过，新增伪造上下文测试及其他检查共 95 项测试通过。

## P2 派发整合记录

- 日期：2026-11-06；状态：受控测试通过，P2 阶段退出条件已验证。
- 变更：`crates/saya-core/src/dispatch.rs` 串接七项本地工具、规则／核查／确认与单次许可；读取由派发器保存意图和结果，写入与命令由适配器保存，避免重复记账。派发器只接受自身签发的许可，并共用运行取消信号与调用方提供的核查预算。
- 验证：macOS aarch64／Rust 1.99.1 上 `./scripts/check.sh` 通过格式、Clippy、100 项测试及 release 构建。受控测试对七项工具实际执行，并通过派发器覆盖 12 组模式下的读写与命令，验证确认、非交互待确认、显式拒绝与跨派发器许可拒绝；未访问真实模型或工作区。
- 未完成：派发器尚未与可复用 Agent 循环、CLI 或 SQLite 接线；低层文件适配器仍依赖调用者选择权限门禁，内存存储不能支持真实工作区恢复。P1 原型仍仅在测试构建，须完成 P1 并接线才能形成可复用 Agent 运行时；P2 的完成仅指工具与权限在受控入口的阶段验证。远端 CI 与其他平台本次未验证。

## DEV-033 交付记录

- 日期：2026-11-06；状态：完成（受控组件及假模型循环验证）。
- 变更：`crates/saya-core/src/background.rs` 提供运行内后台任务、启动／状态／取消工具、一次性权限、并发上限、后台错误停止新任务、PLAN 切换与结束清理；后台内部意图和结果不伪装为模型工具消息。模式控制与授权支持同一模式下并发；测试循环可在后台工具运行时继续下一轮，共享模型请求预算。
- 验证：macOS aarch64／Rust 1.99.1，`./scripts/check.sh` 通过格式、Clippy、测试及 release 构建；覆盖七项工具、真实受控进程并发、启动立即返回、PLAN 清理、人工确认、非交互停止、存储错误、任务限额与假模型续跑。所有文件和进程测试使用临时工作区及受控进程。
- 未完成：`prototype` 循环仍仅在测试构建；正式运行时、三协议、CLI、SQLite、恢复和完整事件／预算控制尚未接线。后台 `shutdown` 必须由运行时调用并等待；不支持跨运行持久任务，未运行真实模型、远端 CI 或其他平台验证。文件系统调用可能阻塞，进程组不约束主动脱离的后代，不构成 OS 沙箱。

## DEV-036 交付记录

- 日期：2026-11-06；状态：完成（提示词组件与受控接线）。
- 变更：`crates/saya-core/prompts/v1` 将核心、四工作模式、三权限、后台与独立核查提示词重写为英文，不固定沟通语言。`prompt.rs` 提供确定顺序装配、段名、版本、指纹和独立运行事实数据；假循环实际使用完整 prompt，核查组件使用独立审核 prompt。新增离线预览示例、30 项审核场景与来源／取舍对照，见[提示词定制](prompts.md)。
- 验证：macOS aarch64／Rust 1.99.1，`./scripts/check.sh` 通过格式、Clippy、117 项测试与 release 构建；英文装配、12 种组合、实际模式重装、审核隔离、异常注册表派发前停止和数据边界均有验证。执行／审核预览示例已实际运行；相关文档本地链接与来源清单检查通过，无网络或真实模型调用。
- 未验证／未实现：仅接入已有审核组件与测试专用 Agent 循环。正式运行时、DEV-021 项目指令来源及刷新、三协议／CLI、缓存控制与持久化元数据尚未接入；30 项场景仅含人工期望，不能证明真实模型的分类准确率或提示注入抵抗能力。本轮未推送或运行远端 CI。

## DEV-037 交付记录

- 日期：2026-11-06；状态：完成（core 运行时与受控验证）。
- 变更：`crates/saya-core/src/runtime.rs` 提供单次运行协调器及并发控制句柄，连接模型边界、英文 prompt、工具派发、权限／确认、共享预算、后台任务、会话存储及运行结果。检测会话冲突、工具调用 ID 冲突、无效模型完成；终止时闭合未派发调用，结束前等待后台清理。派发器对适配器执行前返回的参数错误补齐工具结果，避免无意图及重复记账。后台保存失败保留可观察的结果。
- 验证：macOS aarch64／Rust 1.99.1 上 `./scripts/check.sh` 通过格式、Clippy、139 项测试与 release 构建；其中 21 项运行时集成测试另连续执行 200 轮通过。受控测试包含模型／工具往返、12 组模式权限、确认／拒绝、后台续跑、模式切换、取消、预算、重试、部分流、意图／结果保存失败和会话冲突。进程清理增加组长退出后清理已关闭输出管道的后代的测试；发信号前保留组长 PID，macOS 返回 `EPERM` 时确认组已消失，否则返回错误。
- 未验证／未实现：三协议 adapter 和真实模型、SQLite 跨进程持久化／恢复、完整事件输出、CLI、项目指令装配及全路径凭据脱敏仍是 P3 至 P6 工作；`MemoryStore` 只能用于测试。没有运行远端 CI 或其他目标平台验证；尤其尚未在 Linux 上验证本次进程清理修改。底层文件并发替换和进程组清理的限制仍如 DEV-008／009 所述。

## 更新规则

- 使用待开始、进行中、受阻、待验证、完成五种任务状态；“完成”必须满足路线图的完成标准。
- 每次进入任务时记录当前任务；结束时补充文件／变更定位、检查命令与结果、报告或日志路径及未验证范围。
- 有阻碍时写明事实与影响；待验证或受阻不计为完成。下一项工作按任务依赖选择，不因时间经过自动切换。
- 阶段内所有任务及退出条件满足后，才更新阶段完成。TUI、GUI 和候选扩展不计入 MVP 任务。
- 若任务拆分，在原编号下追加子项并同步依赖；保持任务定义与此处记录一致。

## 交付记录模板

以下是待实际执行后填写的格式，没有任何预填的通过结果。

```text
任务：DEV-xxx
日期：
状态：
变更与定位：
验证命令与结果：
证据路径：
未验证范围／阻碍：
下一任务：
```


## N3 DEV-066 生命周期组件阶段

2026-11-10，完成私有内容寻址PluginStore及本地CLI管理入口，分别15项和6项回归通过。暂存源码独立副本的格式检查与全workspace全目标严格Clippy通过，日志为 `target/n3/dev066-stage-validation.log`。覆盖安装默认禁用、精确摘要启用、作用域覆盖、固定依赖、更新与既有Run租约、停用／卸载／恢复、未知清理状态及目录句柄验证。实际Skill／MCP／Hook运行装配与四平台联合验收按[N3记录](n3-validation.md)继续。


## N3 DEV-067 Hook 组件阶段

2026-11-10，完成四事件Hook执行接口与独立SQLite日志，分别12项执行及12项日志组件回归通过，暂存源码独立产物目录的格式与严格Clippy通过，日志为 `target/n3/dev067-stage-validation.log`。覆盖意图先行、当前宿主权限回调、check否决、幂等事件、预算、超时计时、真实退出恢复Unknown和清理未知。正常Run及原生执行接入按[N3记录](n3-validation.md)继续。

## N3 共享运行时集成门禁

2026-11-10，自动／手动Compact、宿主记忆、Skill、检查点、插件与Hooks已接入共享Core运行循环。托管工具沿同一模式、人工审批和持久化边界，模型、启动与Hook共用工具预算，各broker共用Run网络预算。通用文件工具及隔离worker保留宿主私有路径和inode快照，失效记忆组、旧输出引用与后台正文沿来源证明退出模型投影。

独立复核确认并修复摘要发前Memory／Skill版本竞态及初始插件准备任务被中止后的资源owner缺口。每次摘要发送前重新核对已捕获来源，初始worker由Run持有，清理先取消并join再接管，未知继续保留pin。Core正常入口覆盖18组协议／权限／PLAN记忆调用、16组读取／注入／写入设置，以及SQLite恢复、审批中遗忘与输出回取边界。

该阶段源码在本机macOS27.2 ARM通过1301项Rust测试、格式、workspace全目标严格Clippy与优化构建，日志为 `target/n3/integration-workspace-gate-3.log`。7项既有真实原生插件／Hook用例已通过；第8项初始准备abort及随后CLI／TUI四平台结果由最终固定源码 `e1da373` 收证，见[N3最终验收](n3-validation.md#四平台最终验收)。

### DEV-088 管理与交互入口

CLI已接入显式memory管理命令和独立compact命令，text chat及TUI提供 `/memory`／`/compact`。管理输入、完整差异与确认身份独立于消息草稿，未知提交保留结构化实际receipt，终止信号传给磁盘worker并等待真实完成。Memory导出复用已批准的目录句柄及原字节，长输入光标按显示单元格保持可见。

共享App、CLI及TUI阶段随本机1301项完整Rust门禁通过，包含11项App管理／父目录、27项CLI bin及61项既有正常CLI测试。首次插件启用的真实CLI诊断发现缺少Hook日志父目录，现已补安全私有创建及链接／权限回归。固定新候选的32条CLI与15条PTY最终验收继续进行，旧候选诊断分别保留为 `target/n3/cli-diagnostic-bootstrap.json` 和 `target/n3/tui-diagnostic-6.json`，它们各自的准备范围与程序身份独立记录。

### 凭据授权原参数回归修复

最终候选的历史审查回归发现，始终启用动态凭据注册表后，模型返回的工具参数在授权前被脱敏改写，独立核查会看到不完整视图。现保留原始ToolCall供精确授权和执行，另生成脱敏副本用于Session、持久化及记忆来源哈希。标识符及opaque内容继续失败关闭，歧义JSON在持久化前拒绝。

新增完成边界单元测试及真实SQLite运行回归，核对轮内新登记凭据、人工批准、原值分发、零独立审核请求，以及审批预览、数据库和后续模型请求脱敏。独立源码复核与五项历史审查诊断通过。本机 `RUST_TEST_THREADS=1 just check`通过1304项Rust测试、格式、严格Clippy与优化构建，日志为 `target/n3/integration-workspace-gate-4.log`。`just test-tooling`的75项Python回归通过，记录为 `target/n3/final-tooling-2.log`。最终固定新候选另行验收。

### D-053 模型输出目标与动态上下文余量

2026-11-10，按项目负责人新增要求，将输出目标解析为用途／请求显式覆盖、profile显式max_output_tokens、模型limit.output声明、32000回退。模型声明通过profile元数据接入。审核与摘要默认继承实际profile，已有显式值继续生效。每次请求使用完整wire计数及额外安全余量收缩实际输出值，输入基准固定为 `B=C-R-1`，80%／55%压缩水位与动态输出分别计算。

三协议采用相同计数与发送投影，覆盖Chat兼容字段、Unicode、工具schema、显式高低覆盖、小窗口、无剩余空间和极大安全余量。摘要发送实际拟合后的请求，记录其真实指纹。计数适配版本升至v2，恢复时通过身份检查选择有效检查点，并保留原始历史重建路径。

真实模型诊断定位到摘要将助手陈述升级为observed的来源分类错误，英文Compact提示词现明确角色映射及有限预算内的紧凑输出。两项回归保留真实schema的严格证据限制，文档快照同步。此前扩大提示词导致的两项固定窗口夹具失败保留于 `target/n3/integration-workspace-gate-5.log`，新预算语义及对应边界调整后完整门禁通过。

该阶段本机完整检查通过1315项Rust测试、格式、严格Clippy与优化构建，日志为 `target/n3/integration-workspace-gate-6.log`；75项Python回归见 `target/n3/final-tooling-5.log`。独立源码复核覆盖配置继承、实际wire、额外安全余量和旧检查点回退。debug诊断程序上三协议各完成实际记忆写入、Compact及恢复后的PLAN读取；Chat首轮缺少成功memory_update记录被验收判为失败，另次重试通过，原报告保留。最终固定源码的程序与四平台结果见[N3最终验收](n3-validation.md#四平台最终验收)。

### DEV-068 固定本机联合验收

固定优化程序绑定源码 `2018520`，SHA-256为 `11124d6347811eda03e27bcbe57f728e7067311c34ecc17dc8a7e56daca80b37`。三协议34条CLI、15条PTY、8项实际原生插件、19项原生隔离、完整性能v2、25项MCP／OAuth、5项历史审查及26条历史PTY均通过。CLI的105组工具意图／结果精确配对，0未完成意图；3次Manual Approval等待保留真实审批记录。

新增同Session长输出→引用检索→Compact→插件Skill续接→原引用回取联合链，通过实际wire核对用户约束、原始历史、Skill注入生命周期和租约收尾。真实deepseek-flash三协议基础冒烟6项亦通过。证据位于 `target/n3/final-local-3/`，程序与源码清单前后身份一致。当前主机为macOS27.2 ARM；固定源码 `e1da373` 随后完成四平台联合验收，见[N3最终记录](n3-validation.md#四平台最终验收)。

### DEV-089 记忆与旧检查点恢复的本机验收

同一固定优化程序在三协议可控模型中各完成9条记忆／权限CLI旅程，并完成15条真实PTY管理与Compact旅程。真实deepseek-flash三协议另各完成实际记忆写入、Compact和恢复后的PLAN读取，共9阶段，报告为 `target/n3/final-local-3/live-memory-report.json`。验收同时要求成功工具记录、实际落盘、摘要请求与原始历史保全。

真实旧优化程序生成的v1检查点通过三协议迁移到新v2程序。旧检查点由当前宿主身份检查退出请求投影，原始User约束与工具历史保留，正常新调用成功且无重放／unknown。报告位于 `target/n3/final-local-3/migration-2/report.json`，两端程序执行前后哈希一致。最初短历史夹具的拒绝及debug模型未写入案例保留原报告，最终验收各自使用独立证据。本机记忆、迁移与四平台固定候选证据共同完成DEV-089验收。

### 2026-11-10 审查整改：TUI 背压与清理证据

修复[11月10日审查](review-2026-11-10.md)的R-01：TUI从首次持续背压起最多等待2秒，到期取消所属运行；部分帧失败后的下一位图先清除不确定终端状态并从空缓冲完整重绘，用户渲染闭包仍在帧I/O计时之前执行。真实PTY覆盖部分帧旧单元格不重放、stalled panic恢复，以及完整控制环取消后台命令、退出码1、termios与FD flags恢复。

R-02的原生验收补充服务主进程、服务子进程和Hook子进程检查。Linux以`setsid`构造脱离组后代，macOS验证当前支持的同一进程组后代；两端记录存活进程的启动身份并与清理后的PID状态交叉核对，Hook子进程另以延迟survival marker的不存在证明没有遗留副作用。另补`HOME == workspace`、HOME符号链接别名、XDG外置、工作区内config的四项源码回归。

本机macOS27.2 ARM执行`RUST_TEST_THREADS=1 just check`，格式、workspace全目标严格Clippy、1320项Rust测试（另8项native ignored）与优化构建通过；`just test-tooling`的75项Python回归通过。显式debug程序通过8项真实原生插件验收；固定release程序的smoke、文本journey、5项历史审查回归及26条TUI旅程全部通过。Linux脱离组分支本轮未在Linux执行；macOS主动脱离进程组的后代仍按[原生隔离](sandbox.md)的`best_effort`边界保留，不据此宣称任意后代树已硬清理。本轮未推送，也未运行远端CI。

### 2026-11-10 第二轮复审整改

按[第二轮复审](review-2026-11-10_02.md)修复F-01至F-03。原生后代验收改为宿主侧独立采样，分别记录服务主进程、服务子进程、`run.started`Hook后代和`run.finished`Hook后代；两个Hook后代等待宿主取得PID与启动身份后退出，因此最后一次模型调用之后的事件也进入证据范围。测试守卫保留原始guest PID，在失败展开路径重新映射并对身份匹配的遗留进程执行bounded kill。

resync PTY探针的填队与resume阶段改用独立五秒期限。本机`RUST_TEST_THREADS=1 just check`通过1321项Rust测试（另8项native ignored）、格式、严格Clippy与优化构建，`just test-tooling`75项通过；真实原生插件8项、固定release的smoke、文本journey、5项历史审查回归及26项TUI旅程全部通过。本轮仍不推送、不运行远端CI，Linux脱离组分支与macOS主动脱离组限制保持原边界。

### 2026-11-11 第三轮复审与 Linux Colima 验收

按[第三轮复审](review-2026-11-10_03.md)修复F-04、F-05。失败路径守卫记录并用启动身份验证进程组leader，Drop先释放Hook、join采样线程，再执行bounded组清理并等待组`ESRCH`。原生验收为每个角色生成随机argv marker，Linux仅在`NSpid`末端等于guest PID且`/proc/<host>/cmdline`含完整角色marker时接受唯一宿主映射，不再依赖sandbox内`readlink`。

本机macOS27.2 ARM的`RUST_TEST_THREADS=1 just check`通过1321项Rust测试（另8项native ignored）、格式、严格Clippy与release构建，真实原生插件8/8通过；`just test-tooling`75项和固定release的26条TUI旅程通过。Linux ARM64使用工作区内本地Colima `review3` profile验收，VM为Ubuntu24.04／kernel6.8，容器为Ubuntu22.04，使用Rust1.99.1、Bubblewrap0.13.1和socat1.7.4.1。Colima内`scripts/check.sh`完整通过；真实原生插件8/8通过，`native_acceptance.py --skip-benchmarks`报告19项中18项通过、1项因未提供delegated cgroup记为`not_run`，证据为`target/n3/review-3-colima-native-evidence.json`（SHA-256 `2eab2e91fe48e1a3f338f78562671ba7dc690dcb94bb47403fcd27ce8f867877`）；Linux release的smoke、文本journey、5项历史审查回归及26条TUI旅程全部通过。本轮未推送、未运行远端CI。
