# 配置加载组件

配置解析已接入 CLI 与默认 TUI。`saya config init`、`saya config check` 和任务入口复用该组件。配置格式、完整示例与产品规则以 [MVP 基线 B-04、B-05 和 B-15](mvp-baseline.md) 为准。

组件只读取一份配置，依次按显式参数、相应 `SAYA_*` 环境变量、文件内容和内置默认值解析；`SAYA_CONFIG` 只选择文件，不会自动加载或合并项目配置。XDG 路径要求绝对路径，否则回退至 HOME 下的标准目录。工作区与 `permissions.additional_roots` 在加载时检查存在性，权限规则同步校验。Sandbox 路径身份和实际能力在 Run 准备阶段检查。配置加载保持只读解析。

每个 profile 显式声明协议、API 根 URL、模型、鉴权方式和上下文窗口，模型凭据和额外请求头的值只能通过环境变量名引用。默认没有 profile 或付费模型；未知字段、未定义的 profile、缺失的环境变量引用、不合法的鉴权、权限规则和预算均报错。解析错误不回显原始 TOML 或环境变量值。程序使用环境快照，可在测试中注入；CLI 将配置应用于模型、执行器和权限控制。

profile 可选 `strict_tools`、`stream`、`allow_remote_http`、`headers_env`，以及兼容网关使用的 `legacy_max_tokens`、`omit_stream_usage`、`omit_reasoning_include`；Anthropic 的 `api_version` 支持基线约定的两个版本。`strict_tools` 默认关闭：当前后台调用的开放参数对象和部分可选字段不能无损转为 OpenAI strict schema，显式开启时会在请求前报错，而不静默改变工具语义。工具参数仍在运行时校验。`commands.inherit_env` 显式列出额外继承的变量名，执行器接线时必须同时传入所有凭据和请求头引用名，避免这些敏感变量进入子进程。

`limits.write_max_bytes` 限制原生写入／补丁的输入和输出字节数，默认 1 MiB。CLI 将该值传到运行共享的调度器，前台和后台使用相同上限；零值会使配置校验失败。

## Thinking effort

当前开发线允许每个 profile 显式设置 `thinking_effort`，取值为 `low`、`medium`、`high`、`xhigh`、`max`。省略时，请求沿用服务端默认值，TUI 显示未知。可在自己的 profile 中添加一行。

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

当前 N2 开发线的完整四平台整体验收仍在进行，0.2.0 的发行范围与限制继续按其历史记录维护。[N2 验收记录](n2-validation.md)、[0.2.0 发行说明](release-notes-0.2.0.md)

省略 `sandbox` 时采用 `backend = "auto"`，在 macOS 使用系统 Seatbelt，在 Linux 使用 Bubblewrap 与 Seccomp。Linux 要求 Bubblewrap 0.13.1 或更新版本，代理网络还需要 socat，可执行文件需符合宿主可信路径和权限检查。macOS 固定使用系统 `/usr/bin/sandbox-exec`。依赖及必需能力缺失会使运行启动失败。`config check` 仅验证语法、数值和环境引用，原生能力在 Run 启动时探测。

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
| `cgroup_root` | 显式委派的 Linux cgroup v2 绝对目录 |
| `network` | 默认 `deny`，或显式 `proxy` 与完整 origin 允许列表 |

工作区默认进入原生读写视图，PLAN 与权限调度继续决定具体操作能否执行。`permissions.additional_roots` 同时加入逻辑文件权限根及原生读写根。单独添加 `sandbox.read_roots` 或 `write_roots` 只扩展 OS 视图，原生文件工具的逻辑权限仍按工作区与 `permissions.additional_roots` 判断。

解释器、包管理器和编译器位于工作区及系统运行库之外时，需要显式提供其可读目录。可将实际工具链目录加入 `sandbox.read_roots`。对应目录必须存在。此配置适用于进程读取工具链，文件工具访问该目录还需满足逻辑权限策略。原生根数量各限 128 项，根目录 `/` 无法作为原生开放根。

宿主默认保护启动时已存在的配置文件、data、state、cache 目录以及工作区 `.saya/local`。启用凭据引用而新建凭据目录后，宿主会把实际凭据目录加入保护集合。其他 `.saya` 项目指令和 Skills 仍按[项目上下文约定](project-context.md)读取。Linux 无法安全表示保护路径时返回 `unsupported_fs_policy`，具体约束见[执行契约](execution.md#n2-原生执行边界)。

显式 `backend = "host"` 选择宿主兼容执行，其文件系统与网络隔离能力报告为 `unsupported`，工作模式、权限规则与运行预算继续生效。启用了 `credential_ref` 的 HTTP MCP 服务，或 state 目录下已经存在 `credentials` 存储目录时，应用拒绝 Host 启动。

可选资源字段为 `cpu_seconds`、`file_size_bytes`、`open_files`、`total_memory_bytes`、`total_processes`，以及 `cpu_quota.quota_micros`／`period_micros`。前三项使用单进程 rlimit。后三项依赖有效的 Linux cgroup v2 委派，`cpu_quota` 表示周期 CPU 带宽。必需的资源控制不可用时准备失败，能力分别报告为 `hard`、`best_effort` 或 `unsupported`。

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
