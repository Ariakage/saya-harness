# 配置加载组件

配置解析已接入 CLI 与默认 TUI。`saya config init`、`saya config check` 和任务入口复用该组件。配置格式、完整示例与产品规则以 [MVP 基线 B-04、B-05 和 B-15](mvp-baseline.md) 为准。

组件只读取一份配置，依次按显式参数、相应 `SAYA_*` 环境变量、文件内容和内置默认值解析；`SAYA_CONFIG` 只选择文件，不会自动加载或合并项目配置。XDG 路径要求绝对路径，否则回退至 HOME 下的标准目录。工作区与 `permissions.additional_roots` 在加载时检查存在性，权限规则同步校验。Sandbox 路径身份和实际能力在 Run 准备阶段检查。配置加载保持只读解析。

每个 profile 显式声明协议、API 根 URL、模型、鉴权方式和上下文窗口，模型凭据和额外请求头的值只能通过环境变量名引用。默认没有 profile 或付费模型；未知字段、未定义的 profile、缺失的环境变量引用、不合法的鉴权、权限规则和预算均报错。解析错误不回显原始 TOML 或环境变量值。程序使用环境快照，可在测试中注入；CLI 将配置应用于模型、执行器和权限控制。

profile 可选 `strict_tools`、`stream`、`allow_remote_http`、`headers_env`，以及兼容网关使用的 `legacy_max_tokens`、`omit_stream_usage`、`omit_reasoning_include`；Anthropic 的 `api_version` 支持基线约定的两个版本。`strict_tools` 默认关闭：当前后台调用的开放参数对象和部分可选字段不能无损转为 OpenAI strict schema，显式开启时会在请求前报错，而不静默改变工具语义。工具参数仍在运行时校验。`commands.inherit_env` 显式列出额外继承的变量名，执行器接线时必须同时传入所有凭据和请求头引用名，避免这些敏感变量进入子进程。

`limits.write_max_bytes` 限制原生写入／补丁的输入和输出字节数，默认 1 MiB。CLI 将该值传到运行共享的调度器，前台和后台使用相同上限；零值会使配置校验失败。

## 模型输出 token 目标

N3及当前开发线按D-053并由D-054更新默认，为Execution、Review和Compaction统一选择输出目标desired，优先级如下。

1. 本次请求或用途的显式max_output_tokens。独立核查使用 `review.max_output_tokens`，摘要使用 `context.summary_max_output_tokens`。
2. 实际所选profile的显式 `max_output_tokens`。
3. 该profile的 `limit.output`声明值。
4. 适配器在响应中返回的可识别output limit。
5. 未声明时采用65536 tokens，且任何目标不得超过1000000。

模型声明值当前由用户在显式配置中提供，可使用 `[profiles.NAME.limit]`下的output，或在已有profile内加入以下可选字段。数值由所用模型及服务能力确定。

```toml
limit = { output = 64000 }
# max_output_tokens = 48000
```

省略profile的max_output_tokens时，上例目标为64000。启用注释中的覆盖后，目标为48000。声明值承担默认选择，显式覆盖也可高于声明值，服务端对实际模型参数的校验继续生效。当前配置加载使用这些显式元数据，自动模型目录发现作为后续接入。

核查与摘要的用途覆盖默认均省略，分别继承实际所选核查profile和执行profile。已显式配置的1024／4096等值继续生效。覆盖字段及limit.output要求1至1000000，省略与显式值分别处理。新增声明值进入profile身份指纹。未配置max_output_tokens且无limit.output的旧配置现在采用65536；已保存会话的指纹发生变化时，使用 `--accept-config-change`或TUI的 `/resume-accept`显式接受后恢复。

每次派发以完整wire输入计数 `I_wire`、模型窗口 `C`及安全余量 `R`拟合实际输出上限，满足 `1 <= O <= min(desired, C - R - I_wire)`。最终请求体按实际O重新计数和验证，输入内容、协议封装及输出字段均进入校验。序列化数字宽度变化时可保留少量额外保守余量。输出值映射为Chat的max_completion_tokens或兼容max_tokens、Responses的max_output_tokens、Messages的max_tokens。计数保持estimated标记，服务端实际token与reasoning开销以响应为准。

JSON正文、协议消息、工具结果与Run请求／时间的原有限额继续执行。Compact的64KiB结构化正文上限，以及独立核查64KiB正文与512字符原因上限分别约束实际内容。输出token目标变化保留这些边界及来源验证。[上下文预算与水位](context-management.md#完整请求预算)、[D-054](decisions.md)

## 多模态输入

profile 默认不接受原生媒体输入。只有用户确认所选模型与服务支持某类模态时，才在 profile 中显式开启；关闭的模态不会静默降级为文本。

```toml
[profiles.example.multimodal]
image = true
audio = true
video = false
```

TUI 将拖入或粘贴的现有本地文件显示为蓝色 `[Image N]`、暖色 `[Audio N]`、标题色 `[Video N]`，其他文件仍为绿色 `File: [File N]`。发送时前三类 token 先展开为带路径的媒体引用，在输入落库前完成一次有界读取并保存带 SHA-256 的 OutputStore 快照；持久历史随后只携带快照引用，协议适配器从快照生成内容，普通 File 仍作为路径文本交给模型和工具。当前图片支持 PNG、JPEG、GIF、WebP，单个上限8MiB；音频支持 WAV、MP3，单个上限32MiB；视频支持 MP4、M4V、MOV、WebM、MKV、AVI，单个上限32MiB。单条用户消息累计媒体上限64MiB。

媒体文件不存在、超过上限、扩展名不受支持或 profile 未开启对应模态时，请求在本地返回明确错误，不发送部分媒体。媒体内容按数据量进入上下文估算，视频没有统一token计数，估算不等同供应商计费或视觉采样结果。能力开关进入profile身份指纹，修改后恢复既有会话需显式接受配置变化。[模型协议](model-protocols.md#多模态内容)、[D-060](decisions.md)

## Thinking effort

当前开发线允许每个 profile 显式设置 `thinking_effort`，取值为 `low`、`medium`、`high`、`xhigh`、`max`。省略时，请求沿用服务端默认值，TUI 显示未知。可在自己的 profile 中添加一行。

TUI空闲时可用 `/effort LEVEL` 或等价的 `/thinking LEVEL` 修改当前进程的选择，`default`恢复为省略状态。修改不写回配置文件；已打开会话的配置指纹会随显式选择更新，之后使用原配置文件恢复时若指纹不同，仍需显式接受配置变化。`/model PROFILE` 只选择已有profile并建立新会话，不会修改profile的模型字段。

```toml
thinking_effort = "max"
```

三种协议各自发送下列字段，级别按配置原值传递。

| 协议 | 请求字段 |
| --- | --- |
| OpenAI Chat Completions | `reasoning_effort` |
| OpenAI Responses | `reasoning.effort` |
| Anthropic Messages | `output_config.effort` |

底栏中的橙色级别对应当前执行 profile 实际发送的参数。服务端接受哪些级别及其内部映射取决于模型，未支持的请求沿现有协议错误路径返回。独立审核使用审核 profile 的设置。更改级别会改变配置指纹，恢复已有会话需显式接受配置变化。原来省略此字段的 profile 保留既有指纹。

OpenAI 的 Chat 与 Responses 使用各自的 reasoning 字段，支持范围和默认值依模型确定。[OpenAI Chat API](https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create)、[OpenAI reasoning guide](https://developers.openai.com/api/docs/guides/reasoning)

Anthropic 的 effort 独立控制输出投入，thinking 启用方式另由模型与服务决定。当前适配仅发送 `output_config.effort`。DeepSeek 的兼容协议同样支持上表字段，文档中的原生级别为 `low`、`high`、`max`，其中 `medium` 和 `xhigh` 会映射到 `high`。界面展示请求值，响应未提供级别确认时无法证明服务端内部采用的级别。[Anthropic effort](https://platform.claude.com/docs/en/build-with-claude/effort)、[DeepSeek thinking mode](https://api-docs.deepseek.com/guides/thinking_mode/)

此项通过显式配置文件选择，可由 CLI、文本会话和 TUI 共用。更换 profile 采用目标 profile 的设置。

## 成本估算

可为 profile 显式配置成本估算费率；下例中的金额仅是格式示例，不代表任何厂商价格：

```toml
[profiles.example.pricing]
currency = "USD"
input_microunits_per_million = 1000000
output_microunits_per_million = 2000000
```

`currency` 是用户指定的三位大写币种代码；两个整数分别表示每百万输入／输出 token 的价格，单位为币种的百万分之一。例如 1000000 微单位等于该币种 1 单位。只有费率和对应请求的完整 token 用量均已知时才计算估算；结果向上取整到微单位并标记为估算，执行与审核 profile 分开记录，不推断缓存价格、账单或汇率。价格变更不改变模型身份指纹；目前没有金额上限配置，运行限制仍使用已确认的次数、时间与 token 规则。

## N2 原生隔离配置

当前 N2 开发线已通过完整四平台整体验收，0.2.0 的发行范围与限制继续按其历史记录维护。[N2 验收记录](n2-validation.md)、[0.2.0 发行说明](release-notes-0.2.0.md)

省略 `sandbox` 时采用 `backend = "auto"`，在 macOS 使用系统 Seatbelt，在 Linux 使用 Bubblewrap 与 Seccomp。Linux 要求 Bubblewrap 0.13.1 或更新版本，代理网络还需要 socat，可执行文件需符合宿主可信路径和权限检查。macOS 固定使用系统 `/usr/bin/sandbox-exec`。依赖及必需能力缺失会使运行启动失败。`config check` 仅验证语法、数值和环境引用，原生能力在 Run 启动时探测。

Linux 匿名 Unix 通道的连接状态影响网络边界。当前原生后端要求可确认的正式内核范围，5.15分支至少5.15.86、6.1分支至少6.1.2，或正式6.2及以后版本，并执行关闭／重连接行为探测。RC、无法解析或无法确认回补身份的版本返回 `sandbox_capability_unavailable`。5.4／5.10本轮未纳入确认范围。厂商的 `5.15.0-xxx` 版本串无法直接等同于上游5.15.86。[原生边界](sandbox.md)、[上游修复](https://github.com/torvalds/linux/commit/3ff8bff704f4de125dca2262e5b5b963a3da1d87)

```toml
[sandbox]
backend = "auto"
read_roots = []
write_roots = []
protected_paths = []

[sandbox.network]
mode = "deny"
```

| 字段 | 当前含义 |
| --- | --- |
| `backend` | `auto`、`seatbelt`、`bubblewrap` 或显式 `host` |
| `read_roots` | 额外允许执行环境读取的目录，相对路径以工作区为基准 |
| `write_roots` | 额外允许执行环境写入的目录，相对路径以工作区为基准 |
| `protected_paths` | 优先于可读／可写根的宿主保护路径 |
| `resources` | 可选的单进程与已委派 cgroup 资源限制 |
| `cgroup_root` | 原生 Linux 后端使用的显式委派 cgroup v2 绝对目录，Host 指定此项会在准备时失败 |
| `network` | 默认 `deny`，或显式 `proxy` 与完整 origin 允许列表 |

Linux默认只读系统视图在目录存在时包含 `/usr`、`/bin`、`/sbin`、`/lib`、`/lib64` 和 `/etc/alternatives`，使系统工具的包管理器别名可沿既有可见目标解析。目录内容按实际只读映射，保护路径在所有根挂载后应用，并覆盖系统目录的全部规范路径／guest别名。[执行边界](sandbox.md)

工作区默认进入原生读写视图，PLAN 与权限调度继续决定具体操作能否执行。`permissions.additional_roots` 同时加入逻辑文件权限根及原生读写根。单独添加 `sandbox.read_roots` 或 `write_roots` 只扩展 OS 视图，原生文件工具的逻辑权限仍按工作区与 `permissions.additional_roots` 判断。

解释器、包管理器和编译器位于工作区及系统运行库之外时，需要显式提供其可读目录。可将实际工具链目录加入 `sandbox.read_roots`。对应目录必须存在。此配置适用于进程读取工具链，文件工具访问该目录还需满足逻辑权限策略。原生根数量各限 128 项，根目录 `/` 无法作为原生开放根。

Linux原生视图省略 `/proc`。依赖 `/proc/self/exe` 定位工具链的程序可能需要显式路径，Rust 编译可同时指定 `--sysroot`，并在隔离内的编译命令中设置 `LD_LIBRARY_PATH` 指向该工具链的 `lib`。早期验收按 glibc 通过 `/proc/self/exe` 计算 `$ORIGIN` 的机制推断缺库原因。显式工具链路径配置已通过 Linux x86_64／aarch64 的实际编译验收。[N2验收](n2-validation.md)、[glibc实现记录](https://sourceware.org/pipermail/glibc-cvs/2022q4/081599.html)

x86_64 GNU Linux的Rust默认LLD wrapper还会通过 `/proc/self/exe` 查找同目录链接器。当前编译夹具显式加入 `-C linker-features=-lld`，使用宿主已配置的系统链接器，保留无proc视图。该选项按目标限定，ARM和macOS夹具保持各自已有命令。实际Linux结果见[N2记录](n2-validation.md)。[Rust链接器选项](https://doc.rust-lang.org/rustc/codegen-options/index.html#linker-features)

宿主默认保护启动时已存在的配置文件、data、state、cache 目录以及工作区 `.saya/local`。启用凭据引用而新建凭据目录后，宿主会把实际凭据目录加入保护集合。其他 `.saya` 项目指令和 Skills 仍按[项目上下文约定](project-context.md)读取。Linux 无法安全表示保护路径时返回 `unsupported_fs_policy`，具体约束见[执行契约](execution.md#n2-原生执行边界)。

显式 `backend = "host"` 选择宿主兼容执行，其文件系统、网络及聚合资源隔离能力报告为 `unsupported`，工作模式、权限规则与运行预算继续生效。Host 支持现有单进程 rlimit。指定 `cgroup_root` 或请求 `total_memory_bytes`、`total_processes`、`cpu_quota` 中任一项时，Run 准备在模型派发前返回 `sandbox_capability_unavailable`。启用了 `credential_ref` 的 HTTP MCP 服务，或 state 目录下已经存在 `credentials` 存储目录时，应用继续拒绝 Host 启动。

可选资源字段为 `cpu_seconds`、`file_size_bytes`、`open_files`、`total_memory_bytes`、`total_processes`，以及 `cpu_quota.quota_micros`／`period_micros`。前三项使用单进程 rlimit。后三项依赖 Bubblewrap 原生文件系统边界与有效的 Linux cgroup v2 委派，`cpu_quota` 表示周期 CPU 带宽。必需的资源控制不可用时准备失败，能力分别报告为 `hard`、`best_effort` 或 `unsupported`。

Linux 原生后端拒绝把任何 `cgroup`／`cgroup2` 挂载或其祖先、后代及 bind 别名纳入可写视图，返回 `unsupported_fs_policy`。此限制覆盖默认工作区、`sandbox.write_roots` 与 `permissions.additional_roots`，包含与委派根互为兄弟的其他 cgroup。`sandbox.read_roots` 可按只读方式开放 cgroup 目录，其他策略检查继续生效。

本轮资源配置边界已通过四平台验收，Linux x86_64／aarch64 的委派正负对照通过。委派用例检查内核限制、活动成员和清理，资源耗尽压力仍未验证。[资源与清理边界](sandbox.md#资源与清理能力矩阵)、[N2 验收](n2-validation.md)

网络代理采用 HTTP 代理请求与 HTTPS CONNECT，按完整 origin 授权。示例允许一个外部 HTTPS origin。

```toml
[sandbox.network]
mode = "proxy"
allowed_origins = ["https://example.com"]
```

本地开发目标还需设置相同的 `localhost_development_origin`，并把该 origin 放入 `allowed_origins`。代理检查 DNS 结果并把连接绑定到已核查 IP。CONNECT 授权粒度为目标主机与端口，隧道内的加密路径和方法保持不透明。此网络配置约束原生执行环境。模型请求与宿主管理的 HTTP MCP 连接分别使用显式 profile／服务器配置。

## MCP 配置

`mcp.servers` 最多配置 16 个服务，每个服务显式选择 `stdio` 或 `http`，默认启用。分发到模型的工具由宿主注册表命名，参数、schema、权限、目录版本和取消仍经过统一调度。服务器描述及 annotations 作为外部数据处理。

```toml
[mcp.servers.local]
enabled = false
read_only_tools = ["lookup"]

[mcp.servers.local.transport]
type = "stdio"
argv = ["/absolute/path/to/server", "--stdio"]
# cwd = "/absolute/path/to/workspace"
# env = { SERVICE_TOKEN = "SAYA_MCP_TOKEN" }
```

`env` 左侧是子进程目标变量名，右侧是启动环境中的引用名。只有启用服务的环境引用需要存在。该映射由宿主显式选择，MCP 启动继续经过命令权限与实际环境摘要绑定。`read_only_tools` 是宿主对原始远端工具名的分类，PLAN 的 HTTP MCP 工具仅开放其中明确列出的项，stdio 服务在 PLAN 中保持停止。

HTTP 配置使用 `[mcp.servers.NAME.transport]`，设置 `type = "http"` 与完整 `endpoint`。默认要求 HTTPS。明确设置 `allow_insecure_http = true` 可选择明文测试端点。凭据通过 `bearer_env`、`headers_env` 或私有存储的 `credential_ref` 选择，`bearer_env` 与 `credential_ref` 互斥。`scopes` 绑定凭据授权范围。登录、撤销、作用域及完整示例见[MCP 使用说明](mcp.md)。

每个服务可设置 `[mcp.servers.NAME.limits]`。默认连接超时 20 秒，请求／目录发现超时 120 秒，单个 wire 消息和完整工具目录各 1 MiB，最多 32 页、256 个工具，结果投影 256 KiB、128 个内容块。运行时同时执行共享活跃时间与工具预算，人工确认期间暂停活跃计时，收尾独立完成。

## 输出捕获与上下文预算

N3开发线默认开启私有OutputStore，位置为所选 `data_dir` 下的 `outputs/`。新增配置独立控制捕获存储，原有 `limits.command_stdout_max_bytes`／`command_stderr_max_bytes`继续限制各流的有效捕获量，默认各64KiB。显式扩大这些值时仍受OutputStore对象及Run剩余额度约束。用户媒体快照与工具捕获共用该私有根及总库／对象数配额；`output_capture.enabled = false`只关闭工具输出捕获，不禁用Session媒体快照。

```toml
[output_capture]
enabled = true

[output_capture.limits]
max_object_bytes = 16777216
max_run_bytes = 134217728
max_store_bytes = 1073741824
max_objects = 4096
tail_bytes = 4096
preview_bytes = 4096
max_page_bytes = 16384
max_page_lines = 200
max_search_matches = 200
```

捕获器提供首尾、保留范围、缺口和完整性，超额后继续排空。无法安全解码或脱敏的窗口保留withheld原因。MCP保存投影前的文本／structuredContent为NDJSON，非文本媒体保留省略描述。模型侧output_read默认8KiB／100行，最多8KiB／200行；output_search最多50项、4行上下文、8KiB。小于这些值的宿主配置优先。查询游标绑定脱敏视图摘要，凭据集合变化后旧游标返回cursor_invalid。

宿主引用在工具结果事务内登记，然后发布对象。恢复核对已封存对象及宿主登记。查询按当前权限和Session归属执行。Manual Approval覆盖读取和搜索，PLAN允许只读查询，显式deny优先。Run发布的对象持有租约，历史引用作为可回收记录保留，对象被回收后返回output_unavailable。读取宿主引用登记设4096条记录的有界保护，超过时返回明确错误。

共享运行入口以所选profile构建ContextBudget。窗口为 `C`，安全余量为 `R = ceil(C / 10) + 1 + extra_safety_tokens`，稳定输入基准为 `B = C - R - 1`，为每次请求保留至少1 token输出空间。80%／55%水位均按B计算，实际输出再按完整输入的剩余空间收缩。所有缓存前缀、工具schema、动态数据、消息封装和opaque状态都参与计数，最终保护保留严格90%余量。必留内容超限时返回 `context_limit`，原始Session历史保持可恢复。

```toml
[context]
auto_compact = true
trigger_percent = 80
# 可选精确输入token水位；存在时优先于 trigger_percent。
# trigger_tokens = 950000
target_percent = 55
extra_safety_tokens = 0
# 可选用途覆盖，省略时继承执行profile的输出目标。
# summary_max_output_tokens = 65536
max_compaction_calls = 32
compaction_timeout_seconds = 600
```

默认在下一次Execution模型请求前达到80%稳定输入基准时触发Compact；显式 `trigger_tokens` 使用精确输入token水位并受稳定输入容量上限约束。TUI `/autocompact TOKENS|auto|off` 可切换当前进程的后续请求，`950k`、`1m`等写法会被解析，`auto`恢复profile声明的百分比。成功投影必须降至55%以内。target要求大于0且小于trigger，trigger最多100%。摘要输出继承所选执行profile目标，显式summary_max_output_tokens优先，再依据每次摘要／合并请求的剩余空间收缩。单次压缩最多32次实际模型调用，可配置1至64。操作超时默认600秒，可配置1至3600秒，同时受当前Run剩余活跃时间约束。

摘要使用执行profile和空工具集合。实际摘要请求、传输重试与运行活跃时间共用Run预算，默认最多16个完整组分块、4轮合并和每次请求2次允许的传输重试。失败不会切换为无限摘要循环。`auto_compact = false`保留手动入口及完整请求预算检查。

`saya compact SESSION_ID`在独占已保存Session的写锁下执行独立摘要。文本会话和TUI的 `/compact` 在空闲时压缩当前Session，运行时排队至下一模型边界；末轮完成仍有请求时再执行独立Compact。它们保留原始UserText及工具历史，成功检查点通过来源水位CAS提交。[流程与恢复](context-management.md)

本地计数直接使用三协议实际请求序列化器。当前输入token采用标记为estimated的字符启发式，ASCII每4字符、其他Unicode标量每1字符，证据绑定完整请求指纹、协议与适配器版本。计数在本地执行，远端tokenizer／count服务属于后续可验证接缝。供应商最终窗口保护继续执行，估算方法与版本进入记录。[逐项证据](n3-validation.md)

## 全局记忆

```toml
[memory]
read = true
inject = true
write = true
# root = "/absolute/private/saya-memory-root"
```

默认根为启动环境 `HOME` 下的 `.saya`，索引为 `MEMORY.md`，主题目录为 `memory/`。显式root要求绝对路径。运行配置仍从本次显式选择的配置文件加载，指令／Skill发现保持独立规则。

三个开关默认启用。read关闭模型查询与自动事实注入，inject单独关闭自动注入，write关闭更新／遗忘／回滚／备份恢复。内部仍需读取版本和遗忘状态以验证旧上下文兼容性。缺失根按空记忆处理，读和投影保持零创建。`--memory-file`和`--preferences-file`沿显式文件参数提供本次个人上下文。

write与内容读取独立，read关闭时显式CLI仍可按给定版本执行持久变更，版本和receipt只返回元数据。TUI完整review包含旧正文，需先启用read，关闭read会使待确认review失效。

PLAN只读，Manual Approval对每次模型记忆工具调用确认，auto变更采用规则与独立模型核查。显式deny和宿主受保护路径继续生效。`saya memory`提供离线管理，TUI `/memory`提供有界浏览、差异、确认、导出、恢复和空闲时的进程级开关。[实际工具、默认限额与保留范围](memory.md)

### 插件宿主绑定

`[plugins] enabled = true`允许本次Run选择已由用户安装并启用的包，未启用的包保持未加载。包不能从工作区指令修改运行配置。宿主可在 `plugins.bindings.runtimes` 指定当前实际解释器，例如下面的绝对路径。绑定的解释器与只读运行库由宿主核对后进入私有entry视图。

```toml
[plugins.bindings.runtimes.shell]
program = "/bin/sh"
```

当前插件HTTP凭据支持明确的环境变量、header及bearer绑定。插件的存储式 `credential_ref` 尚缺少限定origin的适配器，使用时返回 `plugin_credential_scope_unsupported`。普通MCP既有凭据流程继续沿其自身契约执行。配置Run聚合memory／process／CPU约束并启用进程型插件时，当前返回 `unsupported_resource_policy`，避免将同一Run额度重复分配给各entry。原生后端自身无法满足约束时可先返回 `sandbox_capability_unavailable`。
