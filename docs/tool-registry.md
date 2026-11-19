# 工具注册与可信操作描述

DEV-054为内置文件、命令、会话控制与外部适配器提供同一工具目录。运行时在请求模型之前读取目录，并按工作模式和持久化能力投影。MCP配置、连接和正常入口的接线由DEV-058／059继续完成。

## 身份与分类

`OperationDescriptor`包含宿主生成的名称、来源、副作用分类与SHA-256指纹。内置工具采用固定分类。外部工具默认副作用未知，只有显式宿主配置可以将其分类为只读，服务器描述与annotations作为来源数据保留。

外部名称采用 `mcp__SERVER__TOOL_HASH`，服务器和工具的可读部分分别最多16及18个ASCII字符，后缀为完整服务器ID与远端名称摘要的前20个十六进制字符。名称最多62字节，名称冲突明确失败。服务器ID最多32字节，远端工具名最多128字节。

指纹绑定完整来源、发现版本、分类、schema和描述。相同工具更新目录后会重新核对审批身份。目录替换先完整验证候选集合，任一失败保留旧快照。工具执行开始后持有目录租约，当前调用完成前发布操作返回 `registry_busy`。

## 权限与记录

PLAN只开放宿主认定的只读外部工具。Manual Approval逐次确认，包括外部读取。auto对未知外部副作用进入独立模型核查与人工兜底，bypass继续遵守PLAN和显式deny。外部工具通过与本地工具相同的ApprovalManager、模式租约、取消信号及Store执行。

实际派发顺序为参数校验、授权、保存ToolIntent、复查当前目录与一次性许可、调用适配器、净化并保存结果。持久化期间发生目录变化时，旧许可返回 `stale_approval` 并保存未派发结果。意图记录中的保留字段 `__saya_operation`保存宿主描述，原始模型参数和传给外部服务的参数独立处理。模型不能提供该字段。

已知凭据出现在服务器身份、发现版本、描述或schema时，注册明确失败。schema继续按原始语义验证，结果和错误共用已知凭据脱敏。适配器错误、取消与结果保存失败沿既有运行时边界处理，执行流程不自动重放。

## Schema 与限额

采用固定jsonschema 0.61.0，关闭默认远程／文件引用能力，并显式使用offline模式。正则采用线性时间引擎及编译预算。外部schema和调用参数各限64KiB，JSON结构最多4096节点、32层，目录最多256个工具。

输入schema必须声明object类型，调用参数经schema验证。局部JSON Pointer引用在4096次展开预算内解析，递归引用、资源重定向及动态引用明确拒绝，包括 `$id`、`$anchor`、`$dynamicAnchor`、`$dynamicRef`和`$recursiveRef`。不受支持的正则或schema返回 `invalid_tool_schema`。

schema编译入口供外部注册和MCP目录验证复用。MCP传输、结果类型与输出限额的实现和验收由N2后续阶段单独记录。[验证库文档](https://docs.rs/jsonschema/0.61.0/jsonschema/)

## 调用样例（D-081）

`ToolDefinition.examples`为可选的真实调用样例列表，最多5个JSON对象、合计8KiB、单个样例最多256节点与16层。样例必须是完整真实参数，禁止占位符（如`...`、`TODO`、`<path>`、`YOUR_API_KEY`等常见占位写法）、保留字段`__soyo_operation`（兼容旧拼写`__saya_operation`）与控制字符。

注册时逐条通过工具自身的离线jsonschema校验与`SecretRegistry`已知凭据检查，结构或内容不合格返回`invalid_tool_example`，命中已知凭据返回`credential_exposure`；非空样例列表加入审批指纹，空列表保持既有指纹不变。MCP服务可通过`[mcp.servers.NAME.tool_examples]`（按远端工具名索引，最多256项）注入宿主编写的样例，插件manifest的`[[mcp]]`同样支持`tool_examples`，宿主配置覆盖清单同名词条；样例不取自MCP服务器自身。

模型侧投影分两条路径：延迟加载工具经`tool_describe`在展开结果中返回样例；常驻工具将样例以规范化紧凑JSON追加到描述文本末尾，不进入input schema，因此`strict_tools`与第三方兼容端点保持合法。

## `js_repl`（F-21，D-082/D-083）

`js_repl` 是实验性、严格默认关闭的内置控制面，`enabled` 时经 `register_control_definition` 注册，副作用分类固定为 `Process`（沙箱子进程语义，不在 PLAN 只读集合内）。审批按 `Target::Metadata` 绑定完整脚本原文与 `timeout_seconds`；`[js_repl]` 限额一节见[配置](configuration.md)。

脚本发出的桥接调用不复用脚本自身的许可：每一次 `soyo.callTool` 在宿主侧重新生成合成 call（`外层:repl:N`）并完整走一遍 `check_pre_dispatch_hooks` → `dispatcher.authorize`（含人工确认）→ 一次性许可 → `execute` → `tool.finished` 与脱敏。可桥接集合只含只读工具——`FileRead`、`WebRead`、发现与输出查询类 `SessionMetadata`，以及描述符声明 `ExternalReadOnly`/`WebRead` 的外部工具；`exec`、写入、`ask_user`、会话与后台控制、`memory_*`/`skill_*`/`checkpoint_*`、浏览器工具、`todo_write` 与 `js_repl` 自身在授权前以 `permission_denied` 拒绝，未注册名称同样 fail-closed。桥接调用经 `ReplStore` 持久化 ToolIntent/ToolFinished 证据但不写入 `History`，因此合成 call ID 永远不进入模型上下文；只有脚本最终 `return` 值成为外层 `js_repl` 调用的 `ToolResult`。
