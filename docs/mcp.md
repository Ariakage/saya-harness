# MCP 与凭据管理

当前 N2 开发线支持显式配置的 stdio、HTTP MCP 和宿主凭据引用，完整四平台整体验收仍在进行。0.2.0 的发行范围与限制继续按其历史记录维护。[N2 验收记录](n2-validation.md)、[0.2.0 发行说明](release-notes-0.2.0.md)

## 配置与工具发现

MCP 服务通过显式 TOML 配置选择，最多 16 个。运行创建连接并发现工具目录，结束时清理所拥有的进程和连接。恢复会话后等待新输入，下一次 Run 按本次配置重新建立服务。

服务的原始工具名、描述、输入／输出 schema 和 annotations 来自外部服务器。宿主注册表生成带 `mcp__` 前缀、服务器片段、工具片段及稳定摘要的模型工具名。发现内容经过 schema、分页、总量与目录代次检查，实际调用继续检查当前工作模式、权限、授权绑定和取消。目录变化使旧目录授权失效，模型下一次看到的是新注册表快照。

工具结果按内容类型投影并限制大小。文本与结构化结果可进入模型上下文，不受支持的内容以有界元数据表示。工具失败保留明确错误，连接中断、执行已开始但完成无法确认时按 `unknown` 处理，运行时保存记录并阻止历史重放。

## stdio 服务

```toml
[mcp.servers.local]
enabled = true
read_only_tools = ["lookup"]

[mcp.servers.local.transport]
type = "stdio"
argv = ["/absolute/path/to/mcp-server", "--stdio"]
# cwd = "/absolute/path/to/project"
# env = { SERVICE_TOKEN = "SAYA_MCP_TOKEN" }
```

启动参数采用结构化 argv。`cwd` 省略时使用工作区，`env` 的值是启动环境变量名。目标变量与环境摘要由宿主绑定，默认模型命令的敏感环境过滤仍适用。解释器或工具链位于原生视图之外时，需添加实际 `sandbox.read_roots`。[配置说明](configuration.md#n2-原生隔离配置)

stdio 启动按通用命令执行策略完成规则判断、模型核查或人工确认，执行前后持久化启动记录。Manual Approval 会确认服务启动及之后每次模型工具调用。服务器进程在与本地命令相同的 ExecutionBackend 中启动，stdout 用于有界 MCP 协议，stderr 并发排空。

PLAN 保持 stdio 服务停止。切入 PLAN 先取消并清理运行中的本地 MCP 进程，再应用工作模式。运行结束同样等待清理。握手前尚未派发进程的预算耗尽保存为已知失败，进程启动后握手中断保存为 `unknown`，随后继续完成进程收尾。

## HTTP 服务

```toml
[mcp.servers.remote]
enabled = true
read_only_tools = ["lookup"]

[mcp.servers.remote.transport]
type = "http"
endpoint = "https://mcp.example.com/mcp"
scopes = ["files:read"]
```

完整 endpoint 由宿主配置。HTTP MCP 默认使用 HTTPS，显式 `allow_insecure_http = true` 用于明文测试。该连接由宿主管理，和原生工具进程的 `sandbox.network` 代理策略分别处理。

可用 `bearer_env` 引用进程提供的 bearer token，或通过 `headers_env` 为额外请求头指定环境变量名。启用服务时校验这些引用，实际值加入共享已知凭据集合。`credential_ref` 选择宿主私有存储，与 `bearer_env` 互斥。请求目标绑定完整配置 endpoint，HTTP 传输关闭自动重定向与请求重试。

运行期仅开放宿主 `read_only_tools` 明确分类的 PLAN HTTP 工具。其他工具继续按副作用未知处理，服务器自报只读 annotations 不能授予权限。401／403 返回鉴权或权限错误，宿主等待新的显式授权流程。401 后原 MCP 工具请求保持单次发送。

## 私有存储与执行边界

当前凭据后端使用 state 目录下的 `credentials` 明文私有文件，默认位置为 `$XDG_STATE_HOME/saya/credentials` 或 `~/.local/state/saya/credentials`。目录采用 0700，记录与锁文件采用 0600，记录使用独占文件与原子替换。已有目录或文件的所有者、权限、符号链接及文件身份按私有存储约束校验。

宿主默认把启动时已存在的配置、data、state、cache 与工作区 `.saya/local` 加入原生保护集合。为活动凭据引用新建存储后，实际凭据目录也会强制加入保护集合。`host` 兼容模式在启用凭据引用或已有凭据存储目录时拒绝启动。模型使用注册后的 MCP 工具，凭据解析和最终请求注入由宿主处理。

共享凭据集合覆盖刷新前后的已知值，并用于历史保存、工具结果、事件和显示边界。启用 MCP 时，模型正文与可读 Thinking 先收集到本次请求完成，再使用最新凭据快照脱敏展示。Thinking 活跃状态仍可见。凭据集合或脱敏边界失败时相关内容会扣留或返回明确错误。

## 状态与限制

全屏 TUI 侧栏显示宿主生命周期事件中的 Configured、Connecting、Connected、Stopped、Needs auth 和 Failed，以及实际工具数。配置加载阶段显示 Configured，Connected 由连接与目录发现完成后的事件更新。列表最多接受 16 个配置服务，高屏最多显示 5 行，其余显示省略数。矮屏展示首项状态及省略数。

JSONL 运行事件使用 `mcp.status`，其服务器条目仅包含 `server`、`transport`、`state` 与 `tools`。已建立服务在清理确认后显示 Stopped，随后才产生运行终态。新 Run 先将当前配置服务初始化为 Configured，恢复会话后的连接在新输入启动。

每个服务的连接、鉴权、握手和目录发现均受到配置超时及运行共享活跃预算约束。stdio 启动同时消耗工具预算，模型核查消耗共享模型请求预算。人工确认暂停活跃计时，进程与连接清理在活跃预算结束后继续完成。

macOS 后代进程清理仍按能力等级报告为 `best_effort`。Linux 使用私有 PID namespace，系统依赖、有效委派资源控制及原生执行证据按[N2 验收记录](n2-validation.md)维护。可执行插件、Browser 和自动上下文压缩继续按[后续计划](post-mvp-plan.md)推进。
