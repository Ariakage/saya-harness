# CLI MVP 完整定案方案

**版本：0.1；确认日期：2026-11-06；状态：项目负责人已整套确认。所有运行能力尚未实现。**

本文件是 CLI MVP 的已确认行为基线，对应[决策记录](decisions.md) D-027，保留并细化 D-001 至 D-026 的已确认方向。B-01 至 B-24 已由项目负责人整套采用，当前 MVP 没有尚待选择的产品阻塞项。

后续需求变更按 B 编号说明影响并记录新决策。具体函数签名、内部文件拆分和依赖补丁版本属于实现细节，在保持本基线行为的前提下由开发者处理，不再逐个收集用户偏好。

## 决定总览

| 编号 | 主题 | 已确定行为 |
| --- | --- | --- |
| B-01 | 产品边界 | 文本任务、本地工具、单 Agent；TUI／GUI 为后续阶段 |
| B-02 | 工程与分发 | saya；Rust 1.99.1／2024；core＋CLI 两个 crate；macOS／Linux；MIT |
| B-03 | CLI 与输入 | 交互、run、resume、session、config；明确 stdin 与非交互行为 |
| B-04 | 配置 | TOML；CLI ＞环境变量＞配置文件＞内置默认；凭据只引用环境变量 |
| B-05 | 模型接入 | 三种独立适配器；自定义地址；明确版本、鉴权与流式行为 |
| B-06 | 上下文 | 本地管理完整协议上下文；预算检查；MVP 不自动摘要或跨协议迁移 |
| B-07 | 工作模式交付 | 明确四种模式的输出与完成标准，支持向用户澄清 |
| B-08 | 调度与切换 | 默认串行，显式后台任务支持并发；最后一次切换请求优先；取消优先 |
| B-09 | 权限边界 | 权限可在空闲时修改；PLAN 禁用通用命令；bypass 保留硬限制 |
| B-10 | auto 与核查 | 固定规则优先级；默认复用执行模型作独立核查；不确定则询问 |
| B-11 | 人工确认与预授权 | 单操作授权；精确匹配预授权；无全局 yes 开关 |
| B-12 | 文件操作 | 根目录与路径规则、UTF-8、版本检查、单文件原子替换 |
| B-13 | 命令工具 | argv 直接启动；无隐式 shell；超时、输出上限与进程清理 |
| B-14 | 工具接口 | 固定首批工具及结构化成功／失败结果 |
| B-15 | 运行预算 | 模型／工具次数、时间、输出和读取上限均有明确默认值 |
| B-16 | 错误与重试 | 有限模型重试；工具不自动重跑；流式失败不冒充完成 |
| B-17 | 输出协议 | schema v1、固定结果字段、事件类型与退出码 |
| B-18 | 存储 | XDG 路径、SQLite WAL、本地磁盘、工作区外应用数据 |
| B-19 | 保存与恢复 | 工具执行前后持久化；恢复不重放；存储失败停止继续操作 |
| B-20 | 并发与兼容 | 会话排他写锁；版本迁移备份；配置与工作区变更显式处理 |
| B-21 | 日志与用量 | 已知凭据脱敏；无遥测；用量准确值与估算值分别标识 |
| B-22 | 项目指令 | 按 D-033 读取用户级与工作区指令并发现 Skills，记录来源，不能修改运行时权限 |
| B-23 | 验收与 CI | 三协议离线夹具、真实联调手动执行、Linux／macOS 检查 |
| B-24 | 版本与演进 | 命令与 schema 版本化；新增需求另立决策，不留下 MVP 阻塞项 |

## B-01 产品边界

- MVP 处理文本输入、代码与普通文本文件，使用本地文件和命令工具。主执行 Agent 为单个；核查模型是权限评估组件，不是第二个任务执行 Agent。
- 保留已确认的四种工作模式、三种权限模式、三类模型协议、交互与单次命令、流式和结构化输出、本地会话保存与恢复。
- 图片、音视频、网页检索、浏览器、MCP、插件安装和执行、多 Agent 协作、远程服务和长期记忆不进入 0.1。D-033 将约定目录的 Skill 发现纳入 MVP；完整插件生命周期仍属后续。通用任务首先通过本地文件与命令完成。
- TUI、GUI 仍按既定顺序后续开发；首版通过核心接口与事件复用能力，不在 MVP 选择它们的界面框架或承诺日期。这些属于明确的后续范围，不再作为 MVP 待决项。

## B-02 工程与分发

- 命令名 `saya`，首个版本 `0.1.0`，Cargo workspace 包含 `saya-core` 与 `saya-cli`；CLI 产生 `saya` 二进制。核心包含运行时、模型适配、工具、权限和存储，不依赖终端渲染。
- 固定 Rust `1.99.1`、edition `2024`，最低支持版本同为 `1.99.1`；提交 `Cargo.lock`。workspace 通过 `rust-toolchain.toml` 固定该版本。版本依据见 [Rust 官方发布](https://blog.rust-lang.org/2026/10/15/Rust-1.99.1/)。
- 首批支持 macOS 13+、Linux glibc 2.35+，均覆盖 x86_64 和 aarch64；Windows、musl、移动端与浏览器构建不进入 0.1。
- 依赖职责确定为 Tokio（异步）、clap（CLI）、reqwest＋rustls（HTTP／TLS）、serde／serde_json／toml（序列化）、thiserror（核心错误）、tracing（日志）、rusqlite bundled（SQLite）。搜索、哈希和标识等辅助依赖按实际需要加入；初始化时选择与工具链兼容的稳定版本并锁定，不为补丁版本另开需求讨论。
- 许可证为 [MIT](../LICENSE)，已写入仓库根目录，版权行为 `Copyright (c) 2026 Saya Harness contributors`；标准文本参见 [SPDX](https://spdx.org/licenses/MIT.html)。
- 首版从源码构建并提供手动触发的 release 压缩包流程，不自动上传、发布、签名或安装。自动更新不进入 0.1。

## B-03 CLI 与输入

下表是已确认的命令契约，实际实现范围见[CLI 开发预览](cli-preview.md)和[开发进度](progress.md)。

| 命令 | 行为 |
| --- | --- |
| `saya`／`saya chat` | 新建交互会话，默认 WORK＋auto；没有终端时报输入错误 |
| `saya run [PROMPT]` | 单次任务，默认不交互；使用 `--interactive` 才允许向终端询问 |
| `saya resume SESSION_ID` | 交互恢复，等待新输入；不自动运行旧任务 |
| `saya run --session SESSION_ID [PROMPT]` | 向已保存会话显式提交新输入，运行一次 |
| `saya session list`／`show SESSION_ID` | 列出会话或查看记录，支持 text／json 输出 |
| `saya config init`／`check` | 显式建立无密钥配置模板，或验证配置；已有配置不覆盖 |

- 运行入口参数为 `--config`、`--profile`、`--mode plan|build|code|work`、`--permissions auto|bypass|manual`、`--cwd`、`--data-dir`、`--output text|json|jsonl`。`--approval-file` 仅用于 run，`--accept-config-change` 仅用于恢复既有会话。`bypass` 和 `manual` 分别是已确认权限名称的 CLI 写法；历史序列化值 `review` 可读取为 `manual`。
- D-034 增加本次调用显式个人上下文文件 `--preferences-file`、`--memory-file`，以及兼容端点的可选 `--anthropic-prompt-cache`；实际行为、数据边界与缓存限制见[CLI 预览](cli-preview.md)和[提示词文档](prompts.md)。
- `run` 可用位置参数、`--prompt-file PATH` 或 `--stdin` 提供任务，三者互斥。没有输入或输入为空时报错；不暗中读取管道输入。
- 默认 text 输出；交互文本实时流式展示。JSON 与 JSONL 用于单次运行和会话查询，不把 REPL 的连续输入混入一个 JSON 结果。
- REPL 控制命令为 `/mode`、`/permissions`、`/status`、`/cancel`、`/help`、`/exit`；以 `//` 开头表示普通文本中的 `/`。工作模式可在运行中请求切换，权限模式只在运行空闲时修改。
- `run --interactive` 的确认与澄清通过控制终端进行，不占用任务的标准输入；无法打开控制终端时按非交互处理。

## B-04 配置

- 配置格式为 TOML，`schema_version = 1`。默认文件是 `$XDG_CONFIG_HOME/saya/config.toml`，未设置时使用 `~/.config/saya/config.toml`；`--config` 指定文件替代默认文件，不将两份文件隐式合并。
- 设置优先级：CLI 参数 ＞受支持的 `SAYA_*` 环境变量＞选定配置文件＞内置默认值。支持的环境设置为 `SAYA_CONFIG`、`SAYA_PROFILE`、`SAYA_MODE`、`SAYA_PERMISSIONS`、`SAYA_CWD`、`SAYA_DATA_DIR`；`SAYA_CONFIG` 仅用于选文件。
- MVP 不自动加载工作区内的配置文件。项目指令文件按 B-22 处理，不能据其内容提升权限或更改模型服务。
- 模型 API 凭据仅通过配置中的环境变量名称引用；不支持把密钥值写进配置或 CLI 参数。允许显式配置无鉴权的本地兼容接口。
- 未配置默认 profile 时，交互入口让用户选择已配置项；单次命令报配置错误。没有内置付费模型、账号或自动服务回退。
- 未知配置键、无效枚举、缺失引用与不支持的 schema 在执行前报错。

以下是无真实凭据的格式示例；`YOUR_MODEL` 与上下文窗口值需要用户按所用模型填写。

```toml
schema_version = 1

[defaults]
profile = "openai-responses"
mode = "work"
permissions = "auto"
output = "text"

[profiles.openai-responses]
protocol = "openai-responses"
base_url = "https://api.openai.com/v1"
model = "YOUR_MODEL"
auth = "bearer"
api_key_env = "OPENAI_API_KEY"
context_window_tokens = 128000
max_output_tokens = 8192

[profiles.openai-chat]
protocol = "openai-chat-completions"
base_url = "https://api.openai.com/v1"
model = "YOUR_MODEL"
auth = "bearer"
api_key_env = "OPENAI_API_KEY"
context_window_tokens = 128000
max_output_tokens = 8192

[profiles.anthropic]
protocol = "anthropic-messages"
base_url = "https://api.anthropic.com/v1"
model = "YOUR_MODEL"
auth = "bearer"
api_key_env = "ANTHROPIC_API_KEY"
api_version = "2026-10-21"
context_window_tokens = 128000
max_output_tokens = 8192

[review]
# 未设置 profile 时，复用本次执行 profile。
max_output_tokens = 1024

[permissions]
additional_roots = []

[[permissions.rules]]
effect = "ask"
tool = "exec"
executable = "/usr/bin/git"
args_prefix = ["push"]

[limits]
max_model_requests = 64
max_tool_calls = 256
active_run_timeout_seconds = 1800
command_timeout_seconds = 120
model_timeout_seconds = 180
```

## B-05 模型接入

- `base_url` 是 API 根路径，适配器保留自定义前缀并分别追加 `chat/completions`、`responses`、`messages`。兼容网关可自定义地址，不自动猜测或切换协议。
- 三种适配器都完成文本流、完整响应、工具调用及错误映射。使用直接 HTTP，不依赖某家 Agent SDK 驱动循环。reqwest 的异步客户端依赖 Tokio，适合这里的统一 HTTP 层；见 [reqwest 文档](https://docs.rs/reqwest/latest/reqwest/)。
- OpenAI 两种协议显式发送 `store: false`；Responses 由本地回传所需 output items，不依赖 `previous_response_id` 才能恢复。函数调用结果与原始调用标识关联，协议类型与必要的不透明状态保存在适配层。依据 [OpenAI 官方迁移文档](https://developers.openai.com/api/docs/guides/migrate-to-responses)。
- Anthropic 默认 `anthropic-version: 2026-10-21`，另支持显式选 `2023-06-01` 的文本／本地工具子集。默认不发送 temperature、top_p、top_k；新版本已移除这些参数。鉴权支持 `bearer`、`x-api-key` 和显式 `none`。依据 [Anthropic 版本说明](https://platform.claude.com/docs/en/api/versioning)与[迁移说明](https://platform.claude.com/docs/en/api/migration-2026-10-21)。
- 默认使用 HTTPS 并验证证书；本机回环地址允许 HTTP，远程 HTTP 需要配置显式启用。无自动跨站重定向；沿用系统代理环境设置。
- 支持 profile 的 `headers_env` 映射，以请求头名对应环境变量名，不能覆盖专用鉴权与版本字段；不开放覆盖协议保留字段的任意请求体透传。默认不启用 beta、服务端工具、provider 侧后台任务和多候选回答；Saya 本地运行内后台工具由 B-08 管理。
- 工具 schema 使用明确类型与运行时校验；支持 strict 的协议显式设置，兼容配置可关闭。流结束、参数 JSON 完整且响应可用于工具调度后才派发，不消费半段参数。依据 [OpenAI 函数调用](https://developers.openai.com/api/docs/guides/function-calling)与 [Anthropic 工具流](https://platform.claude.com/docs/en/agents-and-tools/tool-use/fine-grained-tool-streaming)。

## B-06 上下文

- 本地保存消息、工具调用／结果、关联标识、适用的项目指令来源与协议状态；不能仅保存最终可见文字。
- 同一会话内不切换协议或模型。MVP 更换协议／模型时新建会话，旧会话仍可查看；核查 profile 不受此限制。
- Responses 必要的加密 reasoning items、Anthropic 必要的签名／不透明状态按原顺序保存与回传，不解码、不作为普通思考文本展示。历史工具消息映射按各自协议实现，尤其保持 Anthropic 的结果紧邻调用要求；见 [Anthropic 工具结果契约](https://platform.claude.com/docs/en/agents-and-tools/tool-use/handle-tool-calls)。
- 每个 profile 必填模型名与 `context_window_tokens`，不猜测任意兼容模型的窗口；最大输出默认 8192，允许 profile 降低或覆盖。
- 输入与输出预留合计达到配置窗口的 90% 时，停止新模型请求并以 `limit_reached/context_budget` 结束；本地估算与 provider 返回的确切 usage 区分。估算不能代替 provider 的最终上下文限制，超限响应明确记录。
- 0.1 不自动摘要、不静默裁掉历史，不启用 provider 专属自动 compaction。用户可新建会话；跨会话长期记忆不进入 MVP。

## B-07 工作模式交付

| 模式 | 完成标准 |
| --- | --- |
| PLAN | 只读调查后给出目标理解、必要澄清、执行步骤与验收方法；不落地修改 |
| BUILD | 围绕目标完成必要实现、适用验证、产物与交付说明；卡住时说明未完成项 |
| CODE | 聚焦代码编写／修改／调试，并做与改动相称的验证；可以测试和审查相关改动 |
| WORK | 完成本地通用任务，交付所需文件或结果，并说明未完成项与依据 |

- 所有模式均可直接完成、调用允许的工具或向用户澄清；不强制每次先进入 PLAN，也不自动切换工作模式。
- 人工澄清通过 `ask_user` 控制工具发起。交互时等待回答并继续原运行；非交互时返回 `needs_input`，保存问题后结束，不编造回答。
- BUILD 的交付不自动包含提交、推送或发布。用户明确要求这些动作后，仍沿对应工具与权限规则执行。

## B-08 调度与切换

- 每个会话同时只有一次运行。工具默认按返回顺序串行派发；七项本地工具可显式请求当前运行内后台执行，获得任务标识后模型可继续工作并查询状态／结果。每次派发仍检查状态与权限。后台任务有并发上限，启动、查询和取消受运行预算约束；结果不得伪装成已经完成。
- 后台任务不跨运行、不随恢复重放；运行正常结束或取消时，停止新任务并取消、等待清理未完成任务。切换到 PLAN 时，先取消并清理后台写入与命令，然后才应用 PLAN；已发生的副作用据实记录。
- 后台调用以启动结果闭合原工具调用，后续通过查询工具返回任务结果；每次模型协议的调用／结果配对仍按协议要求保持完整。
- 连续工作模式切换请求采用最后一次有效请求；已在执行的调用完成后、下一次调用前应用，当前与待切换模式分别展示。
- 等待确认时没有工具正在执行，可立即应用工作模式切换；原确认失效，对待执行操作按新模式重新判断。
- 取消优先于切换。取消时停止新调度、清理当前调用；保留用户最后请求的工作模式供下一次输入使用，但不因此发起新运行。
- 当前调用失败或超时也视为已经结束，应用待切换模式后按错误策略结束或继续，不将失败当成切换成功的任务结果。

## B-09 权限边界

- `auto`、`bypass`、`manual` 分别对应已确认的三种权限模式；默认 `auto`。Manual Approval 的每次工具调用均须人工批准，包括读取、搜索和后台启动／查询／取消；模型请求不单独确认。
- 权限模式允许在会话空闲时由用户修改，运行中直接拒绝修改并提示取消或等待。修改清空本运行外可重用的临时许可；0.1 不自动持久化授权。
- PLAN 仅启用只读文件／搜索工具及人工交互，禁用写工具和通用 `exec`。这条限制在三种权限模式与显式预授权下均不能绕过。
- auto／manual 的文件工具默认只访问规范化工作区及用户明确配置的 additional roots；范围外直接拒绝，用户可显式扩充根。bypass 跳过这些路径范围检查。
- 用户明确配置的 deny 规则、工作模式限制、参数校验、取消和运行预算仍然生效；bypass 跳过人工确认、路径规则和 auto 模型核查，不生成虚假的核查记录。
- 路径约束落实在文件工具及命令启动目录校验。通用命令是本机进程，MVP 不提供操作系统沙箱，不把 cwd 或参数审批宣称为子进程文件系统隔离。真正的沙箱明确列入后续能力。

## B-10 auto 与核查

- 规则可产生 `allow`、`ask`、`deny`，通过工具名、规范化路径 glob、解析后的可执行文件绝对路径和 argv 前缀匹配。优先级为 deny ＞ask ＞allow；匹配多个冲突规则时按此顺序合并，不依赖文件排序。
- 规则键固定为 `effect`、`tool`、`path_glob`、`executable`、`args_prefix`；effect 必填，其余省略表示不限制该维度，已填写条件必须全部匹配。path_glob 匹配规范化绝对路径；后两项只适用于 exec；路径根检查先于普通规则，不被 allow 覆盖。
- 工作区内原生读写／搜索工具默认 allow；无命中规则的 exec 进入模型核查，明确 ask 直接询问，deny 直接拒绝。manual 下 allow 不免除任何工具调用的人工确认，适用的精确命令预授权可满足对应确认。
- 核查默认复用执行 profile，但使用独立、无历史任务执行上下文、无工具的请求；可用 `review.profile` 指定其他已配置 profile。输出上限 1024 tokens，不向核查模型提供密钥。
- 输入为操作目标、工具名、完整规范化参数、工作／权限模式和必要用户目标摘要；工具参数始终作为待核查数据，不作为核查系统指令。
- 输出固定为 `{ "decision": "allow|ask|deny", "reason": "简短原因" }`。解析失败、服务失败、不支持所需核查或无法判定一律转人工确认；不能自动扩大权限。
- 核查不递归调用核查器；其请求和重试都计入运行的模型请求预算。不确定但无法询问时返回 `needs_confirmation`。

## B-11 人工确认与预授权

- 人工确认只对一次具体操作有效，展示工具、完整命令或目标路径、cwd、工作模式与变更摘要。拒绝返回工具拒绝结果，不自动换一个等价命令绕过拒绝。
- 许可绑定规范化参数、执行目录、工作模式和权限设置版本；任一改变就重新判断。0.1 没有“记住全部授权”或全局 `--yes`。
- 非交互预授权使用 `--approval-file PATH`，文件 JSON 的 `schema_version = 1`，包含 `commands` 数组，每项有完整 `argv` 与 `cwd`；仅匹配本次运行，规范化后精确匹配，不接受通配符或仅程序名前缀。载入时绑定本次运行初始工作模式与权限设置版本，切换后失效。
- 显式预授权可以满足命令确认，但不能覆盖 PLAN、显式 deny 或 auto／manual 的路径边界。文件写入的批量预授权不进入 0.1，可选择 auto 或交互确认。
- 所有模式下需要人工确认但无法交互且没有适用授权时，保存确认请求，返回 `needs_confirmation` 并退出；待确认操作不执行。

## B-12 文件操作

- 工作区默认启动 cwd，`--cwd` 可覆盖；路径按最终实际目标校验，处理 `..` 和符号链接。新文件校验已存在父目录的真实路径，禁止通过链接逃逸允许根。
- 默认处理普通 UTF-8 文本。读取返回内容、截断信息和完整内容版本哈希；二进制与特殊设备文件不作为文本读取。
- 既有文件修改必须提交读取时得到的 `expected_version`，写前不匹配返回 conflict；新文件以“必须不存在”为前提创建，禁止意外覆盖。
- 写入使用同目录临时文件与替换，保留既有权限；不提供跨文件事务或自动回滚。冲突后交给 Agent 重读，不静默强制覆盖。
- 搜索默认遵守 `.gitignore`，跳过 `.git`、`target`、`node_modules` 和隐藏文件；调用者可显式要求包含被忽略／隐藏的文件，仍受路径权限约束。
- 0.1 原生工具不提供目录树删除、重命名或任意文件编码转换；有必要时由经过许可的命令执行。

## B-13 命令工具

- `exec` 接收 argv 数组、cwd、超时和显式环境变量引用，直接启动程序，不隐式拼接 shell 字符串。需要 shell 时，模型必须显式请求对应程序及 `-c` 参数，按完整操作核查。
- 记录退出码、stdout、stderr、截断和超时／取消标记。工作目录默认工作区，默认继承 PATH、HOME、TMPDIR、LANG 与 LC_* 等基础环境；不继承模型配置引用的凭据变量。其他继承变量通过用户配置 `commands.inherit_env` 明确允许，工具可设置非敏感字面环境值或引用已允许的变量，不接收任意宿主凭据。
- 默认 120 秒，配置的最大值初始为 600 秒；默认值与最大值均可由用户配置调整，模型单次请求不能超过已配置最大值。输出达到保留上限后继续排空管道，避免卡住，但不继续保留无限数据。
- 取消／超时先终止进程组，等待 5 秒后强制清理；显式后台命令仅存活于当前运行，无跨运行常驻进程、交互式 PTY 或终端程序托管。需要输入的命令不继承用户终端，避免永久等待。
- 原生命令的副作用按真实执行结果记录，进程失败不代表此前文件修改已撤销。

## B-14 首批工具接口

| 工具 | 核心输入 | 输出与约束 |
| --- | --- | --- |
| `read_file` | path、起始行、行数 | 文本、行范围、版本、truncated |
| `list_dir` | path | 直接子项，按名称稳定排序，不递归 |
| `find_files` | path、glob、包含忽略项开关 | 匹配路径列表及 truncated |
| `search_text` | path、pattern、regex 开关、包含忽略项开关 | 路径、行号、匹配文本及 truncated |
| `write_file` | path、content、expected_version | 创建／更新结果、新版本；null 版本表示要求目标不存在 |
| `apply_patch` | path、patch、expected_version | 单文件上下文补丁结果、新版本；不隐式删除文件 |
| `exec` | argv、cwd、timeout、环境引用 | 退出码、两路输出、截断／超时／取消信息 |
| `ask_user` | question、可选选项 | 用户回答，或非交互 needs_input |
| `background_start` | tool、arguments | 获批后立即返回 `started` 与运行内 job_id；需确认时返回 `needs_confirmation`，不执行操作 |
| `background_status` | job_id | 返回运行中、完成结果、失败或不存在；不等待任务完成 |
| `background_cancel` | job_id | 请求取消当前运行的任务，调用方仍可查询最终状态 |

工具请求使用 JSON Schema，均有调用 ID。统一结果含 `ok`、`data`、`error`、`truncated`；错误含稳定 code 与可读 message，协议适配器再映射到各家工具消息。未启用或未知工具返回错误而不执行。

## B-15 默认运行预算

| 项目 | limits 配置键 | 默认值／规则 |
| --- | --- | --- |
| 模型请求 | max_model_requests | 每次运行 64 次，含执行、核查和重试 |
| 工具调用 | max_tool_calls | 每次运行 256 次，后台启动、查询与取消以及拒绝与无效请求均计数 |
| 后台任务 | max_background_running／max_background_jobs | 默认最多同时运行 4 个，保留本运行最多 256 个任务结果；由运行时配置，均不能绕过整体预算 |
| 活跃运行时间 | active_run_timeout_seconds | 1800 秒；等待人工确认／澄清时暂停计时 |
| 模型连接超时 | model_connect_timeout_seconds | 10 秒 |
| 模型流空闲超时 | model_idle_timeout_seconds | 60 秒 |
| 单次模型总超时 | model_timeout_seconds | 180 秒 |
| 命令默认／最大超时 | command_timeout_seconds／max_command_timeout_seconds | 120／600 秒 |
| 读文件返回／处理上限 | read_return_max_bytes／read_file_max_bytes | 256 KiB／16 MiB |
| 单次原生写入／补丁 | write_max_bytes | 1 MiB |
| 命令两路输出 | command_stdout_max_bytes／command_stderr_max_bytes | 各 64 KiB |
| 文本搜索／文件查找 | search_max_results／search_return_max_bytes | 200 项或 128 KiB，先到者为准 |
| 目录列出 | list_max_entries／list_return_max_bytes | 1000 项或 128 KiB，先到者为准 |
| 模型输出 | profiles.*.max_output_tokens／review.max_output_tokens，不在 limits 内 | 8192／1024 tokens |

上限均通过用户配置调整，不能由模型工具参数自行取消；bypass 不关闭运行预算。达到限制后保留已有结果，以 `limit_reached` 结束，不自动启动新运行续费执行。

## B-16 错误、取消与重试

- 模型暂时性连接失败、429 和可重试 5xx，在没有接收任何成功响应的正文／流事件前最多重试 2 次，使用 1 秒、2 秒退避和抖动；Retry-After 最多等待 30 秒，仍计入预算。鉴权、配置、参数和普通 4xx 不自动重试。
- 已接收部分模型响应后失败，不重试拼接新响应；保存 partial 标记并以失败结束，不执行不完整工具请求。
- 工具不由框架自动重跑。执行错误、拒绝、未知工具或参数错误以结构化结果返回，模型可在剩余预算内产生新的调用；同名工具同参数连续失败 3 次停止运行。
- 用户拒绝人工确认后，同一规范化操作在本次运行内不会再次弹窗；返回拒绝，Agent 可以采用不同方案或说明阻碍。
- 第一次 Ctrl-C 取消当前运行并清理，交互会话回到输入状态；空闲时 Ctrl-C 退出。第二次 Ctrl-C 强制结束进程，残留运行记录在恢复时标为 interrupted。
- provider 拒绝、无效协议结果或未知终止原因不包装为任务成功；输出限额与上下文预算映射为 limit_reached，其他明确错误映射 failed。

## B-17 输出协议与退出码

- 所有机器输出为 UTF-8，`schema_version = 1`；stdout 不混入日志、ANSI 控制码、进度提示或人工确认文字。诊断写 stderr，终端确认使用控制终端。
- 运行的 JSON 输出恰好一个最终对象；JSONL 每行一个事件，并且只有一个 `run.finished` 终结事件，其 data 包含同样的结果对象。状态保存必须先于对应持久业务事件的对外确认。session 查询的 JSON 使用 `{ "schema_version": 1, "data": ... }`，不伪造运行或 run.finished 事件。
- 结果固定字段：schema_version、session_id、run_id、status、stop_reason、text、error、usage、artifacts、persistence。不存在的值为 null 或空数组，不临时省略字段。
- 事件信封固定字段：schema_version、session_id、run_id、seq、timestamp、type、data；seq 在会话内单调递增，时间为 UTC RFC 3339。业务事件有序交付，文本 delta 不保证每个字符都单独持久化，完整消息结束时保存。
- 事件类型为 session.created、session.resumed、run.started、mode.change_requested、mode.changed、model.started、model.text_delta、model.finished、tool.requested、permission.reviewed、permission.requested、permission.resolved、tool.started、tool.finished、input.requested、input.received、run.finished。

事件 data 的必需信息按下表固定；新增可选信息不改变这些含义。

| 事件 | data 的必需信息 |
| --- | --- |
| session.created／session.resumed | work_mode、permissions、workspace、profile |
| run.started | work_mode、permissions、profile |
| mode.change_requested／mode.changed | from、to |
| model.started | request_id、purpose（execution／review）、profile |
| model.text_delta | request_id、text，仅输出任务执行的可见文字 |
| model.finished | request_id、purpose、ok、usage、error |
| tool.requested | call_id、name、args，按凭据规则脱敏 |
| permission.reviewed | call_id、decision、source（rule／model）、reason，仅在发生对应判断时产生 |
| permission.requested | approval_id、call_id、operation_summary、work_mode |
| permission.resolved | approval_id、call_id、decision、source（user／preauthorization） |
| tool.started／tool.finished | call_id、name；finished 另含 result |
| input.requested／input.received | input_id；requested 含 question、options，received 含 answer |
| run.finished | 最终结果对象 |

| status | 退出码 | 含义 |
| --- | --- | --- |
| completed | 0 | 本次运行正常完成 |
| failed | 1 | 模型、工具、协议、存储等运行失败 |
| failed | 2 | CLI 输入／配置无效；还未创建运行时，ID 为 null |
| needs_confirmation | 3 | 无法交互且缺少所需许可 |
| needs_input | 4 | 非交互环境需要澄清 |
| limit_reached | 5 | 次数、时间、token 或上下文预算到达上限 |
| cancelled | 130 | 用户取消；交互界面回到提示符而不是立刻退出整个会话 |

完整命令行尚未解析成功的语法错误只写 stderr 并退出 2；已识别输出模式后的配置和运行错误遵守所选输出格式。help／version 是普通 CLI 输出，不包装成运行结果。

```json
{"schema_version":1,"session_id":"SESSION_ID","run_id":"RUN_ID","status":"needs_confirmation","stop_reason":"approval_required","text":"需要确认命令执行","error":null,"usage":{"input_tokens":null,"output_tokens":null,"estimated":true},"artifacts":[],"persistence":{"saved":true,"error":null}}
```

error 为 null 或 `{ "code": "稳定标识", "message": "可读说明" }`；artifacts 仅列实际创建／修改且已知的目标路径与操作，命令产生但未检测到的文件不编造。新增可选事件数据保持兼容；改变语义或必需字段升级 schema。

## B-18 存储位置与格式

- 数据根为 `$XDG_DATA_HOME/saya`，默认 `~/.local/share/saya`，可用 `--data-dir` 覆盖；数据库为 `sessions.sqlite3`。日志根为 `$XDG_STATE_HOME/saya`，默认 `~/.local/state/saya`。
- 采用 SQLite WAL、foreign_keys 开启、synchronous FULL，schema 初版为 1。rusqlite 选择 bundled，避免依赖系统 SQLite；参见 [rusqlite bundled 说明](https://github.com/rusqlite/rusqlite)与 [SQLite WAL](https://sqlite.org/wal.html)。会话库仅支持本地磁盘，不支持网络共享文件系统。
- 核心实体为 sessions、runs、messages／provider_items、tool_calls、events；会话和运行 ID 使用 UUIDv7。精确表结构作为内部实现随迁移版本管理，不鼓励外部直接依赖数据库列。
- 会话保留直到用户管理数据目录，不自动过期或上传。0.1 提供 list／show／resume；删除、导出和全文检索命令不进入首版。
- PLAN 的数据目录、日志与缓存必须在目标工作区外。若所选目录重叠，启动检查失败，要求换数据目录／工作区；不为保存会话而默默在 PLAN 工作区创建应用文件。

## B-19 保存时机与恢复

- 用户输入、模式生效、完整模型消息、工具执行意图／结果、运行结束均事务保存。工具执行意图必须在副作用前提交；提交失败则不执行工具。
- 文本 delta 只用于流式展示，消息完成或中断时合并保存；不将一次 token 输出变为一次数据库事务。
- 工具有 requested、waiting_confirmation、running、succeeded、failed、denied、unknown 等记录状态；崩溃后没有完成结果的 running 变为 unknown，不声称副作用没有发生。
- 工具已经执行而保存结果失败时停止后续调度，返回 failed／storage_error，并保留可输出的结果与 persistence 错误；不自动回滚或重放。
- 恢复原历史、工作模式和模型配置引用，等待新输入；权限取本次启动配置，历史许可不复活。待确认、未执行和结果不明的工具都不会自动执行。
- 为保持协议上下文完整，终止运行时为悬空工具调用补上对应的结构化终止结果：未执行、需要输入／确认、取消或执行结果不明必须区分。恢复仍不重新执行操作；将已有工具消息正常回传给模型不属于重执行工具。
- 会话列表默认按更新时间倒序，每页 20 条，显示 ID、从首个用户输入截取的标题、工作区、工作模式、profile、更新时间与最近运行状态；不额外调用模型生成标题。

## B-20 并发、配置变更与兼容

- 每个会话获得跨进程排他写锁后才能运行；第二个写入者立即返回 session_busy，查询仍可读。锁由进程生命周期释放，不能仅靠过期时间猜测是否正在执行。
- 迁移前建立一致的 SQLite 备份，再事务迁移；迁移失败不覆盖原库。遇到更高 schema 版本拒绝写入，损坏数据库报 storage_corrupt，不静默重建丢掉历史。
- 恢复时比较不含密钥的 profile 指纹：协议、模型、base URL 与生成设置。凭据轮换不算模型变更；缺失凭据或 profile 时先补配置，不回退其他服务。
- 协议／模型变化需要新会话。相同协议和模型的地址或生成设置变化，由用户通过 `--accept-config-change` 显式接受；记录新指纹后继续，默认不自动接受。
- 工作区不存在或路径变化时，要求用 `--cwd` 显式重新关联；不从标题或相邻目录猜测目标。恢复权限仍采用本次启动设置。

## B-21 日志、凭据与用量

- 默认结构化日志级别 warn；进度由 UI 事件展示。日志按 10 MiB 轮转，保留 5 份，已知 API 凭据及鉴权头在日志、错误、会话记录中脱敏。
- 不采集遥测、用户标识或自动上传会话；不记录原始 HTTP 鉴权数据。工具输出中已知凭据值同样遮蔽，不承诺识别任意未知业务秘密。
- 记录模型／核查请求次数、工具次数、持续时间与 provider 返回的 token 用量；没有确切 usage 时使用 null 并标注估算，不编造准确账单。
- 金额预算仅在用户配置输入／输出价格时计算，未配置时不显示猜测金额；默认靠次数、时间与 token 上限控制运行。

## B-22 项目指令与提示词

- D-030 定制提示词正文使用英文，不固定用户沟通语言；遵循用户明确语言偏好，否则跟随当前请求。提示词版本、装配和来源取舍见[提示词定制](prompts.md)。

- 运行时提示词由核心约束、当前工作模式、用户任务和适用项目说明组成；工具结果单独作为工具数据，不拼进高优先级指令。
- 按 D-034，每次模型请求向静态系统前缀后追加当前时钟、宿主和工作区等描述性事实；用户显式提供的偏好和记忆与项目文件一样作为有来源的低信任上下文，不构成权限。静态前缀与动态后缀分离，并按协议提供缓存优化。
- 按 D-033，默认读取用户主目录 `~/.agents/AGENTS.md`、`~/.saya/AGENTS.md` 以及工作区根 `AGENTS.md`、`.agents/AGENTS.md`、`.saya/AGENTS.md` 中存在的约定指令；操作子路径时继续适用从工作区根至该路径的 `AGENTS.md`，按由外到内提供上下文。不扫描工作区外的任意父目录或其他项目。
- 发现用户主目录和工作区中 `.agents/skills/*/SKILL.md`、`.saya/skills/*/SKILL.md` 的 Skill 元数据；同名时工作区优先于用户级，同一作用域 `.saya` 优先于 `.agents`。发现只提供能力线索，不自动执行或安装 Skill、脚本或插件。Skill 正文按需处理，具体实现与权限接线见 DEV-021；完整安装与插件生命周期仍属 F-03。
- 工作区指令在冲突时优先于用户级指令；它们均是项目上下文，不能覆盖用户明确要求或运行时硬限制，也不能切换工作模式、权限模式、服务地址或自动授予许可。记录指令来源路径与哈希；文件变更在下一次调用前重新读取。
- 运行配置仍按 B-04 选择单一配置文件，不因发现 `.agents`、`.saya` 而自动读取或合并工作区运行配置；不自动扫描插件、外部账号或其他项目。

## B-23 验收、CI 与实现顺序

- 默认测试使用临时工作区、模型／核查替身和本地 HTTP 夹具，不依赖真实账号、外部网络或付费调用。
- 三类协议分别覆盖直接回答、单／多工具、流式分片、无效参数、拒绝／截断、网络错误和恢复所需状态；不能只用一个协议证明全部接入完成。
- 权限矩阵覆盖四种工作模式×三种权限模式，尤其验证 PLAN 禁用写入／命令、bypass 的路径豁免、auto 的核查与兜底、单次命令预授权和历史授权不恢复。
- 会话验证执行前后保存、崩溃 unknown、不自动重放、配置／工作区变更、写锁、迁移和存储失败。JSON／JSONL 检查 schema、顺序、无日志混入与退出码。
- 验收夹具确定为：PLAN 阅读示例 Rust 工程产出方案且文件不变；BUILD 从需求完成一个小型文本统计程序；CODE 修复该程序的可复现缺陷；WORK 将三份文本汇总为报告。模型能力的真实表现由手动联调补充，替身只验证运行时契约。
- Linux／macOS CI 执行 fmt、Clippy `-D warnings`、workspace tests 和 release build；Linux 发行在 Ubuntu 22.04 基线环境构建以匹配 glibc 2.35，macOS deployment target 为 13。首版发行覆盖两类系统的 x86_64／aarch64，非主机架构至少做构建与手动 smoke 验证，未验证的产物不标记已支持。
- 实现顺序：核心类型与假模型闭环 → 文件与命令／权限 → 三协议 → CLI → SQLite 恢复 → 结构化输出与全流程验收。阶段可以并行组织，但不以占位实现宣称需求完成。

## B-24 版本与后续变更

- 本基线决定产品行为、用户接口与验收范围；核心 Rust API 在 0.1 开发期按实现调整，不承诺独立 SDK 的稳定性。
- 配置、结果／事件和数据库分别有版本，不能靠包版本猜测数据结构；不兼容变更记录迁移或明确拒绝。
- 本包已登记为 D-027 并同步原文档；新增范围、改变默认值或改变公开契约再记录新决策。普通内部代码组织与依赖补丁升级不重新向用户逐项询问。
- 原执行与验收草案已按本基线收敛，但“已确定”与“已实现／验证”始终分开。写完文档不等于完成 Rust 构建、协议联调或平台验收。
- Git 提交继续使用 Conventional Commits；提交、推送、发布仍须用户明确要求。一次性确定需求不自动触发上述操作。

## 已收敛主题覆盖

| 原开放问题来源 | 本方案对应决定 |
| --- | --- |
| CLI 名称、语法、输入、默认与输出 | B-02、B-03、B-17 |
| 配置格式、优先级、凭据与默认模型 | B-04、B-05 |
| 核查触发、模型、规则、路径与授权 | B-09 至 B-11 |
| 工作模式职责、切换、权限模式切换 | B-07 至 B-09 |
| 工具名称、文件修改、命令与 PLAN 调查 | B-12 至 B-14 |
| 多工具、错误、超时、取消、预算与重试 | B-08、B-15、B-16 |
| 三协议映射、流式、历史与压缩 | B-05、B-06 |
| 保存位置、格式、时机、保留与恢复 | B-18 至 B-20 |
| 数据损坏、并发、版本与配置变更 | B-19、B-20、B-24 |
| 用量、隐私、日志与外部依赖 | B-05、B-21、B-22 |
| 平台、工具链、依赖、许可证与分发 | B-02、B-23 |
| 具体任务夹具、验收、CI 与未来路线 | B-01、B-23、B-24 |

本文件覆盖当前 MVP 文档中的全部开放主题。外部服务是否实际可用、特定模型兼容性、性能数值和平台验证是实施时需要取得的证据，不伪装为已经验证，也不作为继续收集产品偏好的理由。
