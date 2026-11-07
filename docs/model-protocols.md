# 模型协议接入

**状态：共享 HTTP／SSE、三种协议适配器及 profile 核查接线已通过本地夹具验证；指定模型的三协议小规模联调通过。**

`crates/saya-core/src/provider/transport.rs` 为适配器提供异步 JSON 请求、完整 JSON 响应和增量 SSE 事件读取。它保留 `base_url` 的自定义路径前缀；接口路径由适配器固定提供。配置需要显式提供鉴权方式、额外请求头的环境变量引用及连接／空闲／总超时。密钥不进入错误信息。

默认验证 HTTPS 证书；HTTP 仅在本机回环地址可用，访问远端 HTTP 需要明确启用。客户端使用系统代理环境配置，不自动跟随重定向，也不自行重试；重试由运行时负责计数。额外请求头不得覆盖鉴权、版本、连接或 HTTP 消息边界字段。流式解析按行处理跨块 UTF-8、CR／LF、注释和多行 data；事件及完整 JSON 有大小限制，EOF 本身不表示模型响应成功。

`crates/saya-core/tests/transport.rs` 启动临时本地 HTTP 服务并验证路径、鉴权、版本、环境变量引用、SSE 分片、格式错误、限额、状态映射、取消、空闲／总超时及不跟随重定向；系统代理使用单独进程验证。该自动测试不连接真实模型；其他平台的代理及 TLS 行为尚未验证。

## Chat Completions

`provider::chat::ChatModel` 使用明确的模型、profile、上下文窗口和输出上限。它以本地历史构造消息，显式发送 `store: false`，支持流式与完整 JSON、文本和多个工具调用，并在流完整结束后才返回完整调用；工具结果按原始 call ID 回传。usage 可用时保留 provider 数字。长度截断、拒绝、格式错误和缺少终止标志不会作为成功返回。收到非文本流事件后也不自动重试。

profile 明确选择 strict 时，无法在不改变工具语义的情况下转换的开放对象或可选字段会报配置错误；兼容网关可显式关闭 strict。兼容选项可改用 `max_tokens` 或省略流式 usage 请求。上下文保护使用本地估算加输出预留，不等同于供应商实际 token 计数，不自动删除历史。

当前开发线还支持兼容服务返回的 `reasoning_content`。完整 JSON 和 SSE 中的内容作为该轮 assistant 的协议项保存，后续请求将其回传到对应的 assistant 消息。流式片段按顺序组合，与文本和工具参数共用16MiB单轮聚合限额。历史投影仅接受 Chat 协议、单一字符串字段及紧邻的完整 assistant 语义项，孤立、畸形或跨协议项返回错误。已保存的协议项可在恢复后继续使用，用户可见文本、增量和会话导出使用原有语义投影。该字段的回传满足 DeepSeek 思考模式携带工具时的连续调用要求。[DeepSeek thinking mode](https://api-docs.deepseek.com/guides/thinking_mode/)

## Responses

`provider::responses::ResponsesModel` 显式发送 `store: false`，按本地历史回传原始 output items；不使用 `previous_response_id`。加密 reasoning、消息和函数调用按顺序保存，文本与工具调用作为独立运行时语义项关联到对应原始项；回传前会校验二者一致，避免重复或伪造调用。函数结果通过 `call_id` 与调用关联。

适配器支持完整 JSON 与 typed SSE，交叉检查分片参数、完成项和最终 output；响应完成且流结束后才返回工具调用。拒绝、截断、未完成流、冲突项及未支持的服务端工具不会被当作成功。可显式关闭 `reasoning.encrypted_content` 的 include 字段，以兼容不接受它的网关。

## Anthropic Messages

`provider::anthropic::AnthropicModel` 支持已选 `2026-10-21` 与 `2023-06-01` 两个版本的文本／本地工具子集，发送明确的版本头，不发送 `temperature`、`top_p` 或 `top_k`。流式与完整响应均保留签名 thinking、redacted thinking 和原始内容块；thinking 不作为普通回答显示。

工具输入分片在内容块和消息完成后才形成调用。多个工具结果合并到紧随调用的 user 消息中，并分别关联 `tool_use_id`；截断、拒绝、签名缺失、未完成流与不支持的内容不会被视为成功。

## Profile 与独立核查

`provider::router::ProfileRouter` 将执行请求固定到会话选定的 profile；独立审核默认复用该 profile，也可通过 `review.profile` 指定另一个已注册的 profile，甚至使用不同协议。审核不携带可执行工具或完整任务历史，默认输出上限为 1024 tokens，和执行请求共享模型请求／活跃时间预算。审核服务不可用、输出无效或指定 profile 不可用时退回人工确认；取消与预算耗尽仍终止运行。

每个 profile 可独立设置 `thinking_effort`。Chat、Responses、Messages 分别使用 `reasoning_effort`、`reasoning.effort` 和 `output_config.effort`，未配置时省略字段，独立审核采用其目标 profile 的值。五档取值、模型兼容和指纹恢复规则见[配置说明](configuration.md#thinking-effort)。

## 后台工具映射

DEV-034 使用三种协议各自的本地 HTTP 夹具验证后台启动回执、查询与调用 ID 配对：受控命令仍在运行时，模型继续发出下一次请求；运行结束或触及预算时清理任务。模型、审核和后台控制共享运行预算。内存中的后台生命周期快照与完整对外事件输出的边界见[运行时接口](runtime.md)。真实服务联调不能由本地夹具代替。

## 真实服务抽样

2026-11-07，使用项目负责人提供的 `deepseek-flash` 测试配置，对其 Chat Completions、Responses 和 Anthropic Messages 接口分别完成流式文本、单次工具调用及工具结果回传。Anthropic 测试使用默认 `2026-10-21` 版本；工具为无本地副作用的测试替身。这是单一模型和服务的抽样，不证明其他模型、供应商或全部功能均已联调。Responses 在联调中发现并补充支持了 [reasoning 文本流事件](https://api-docs.deepseek.com/guides/responses_api/)；内容不作为普通答案展示。

可选探测程序为 `cargo run -p saya-core --example protocol_probe -- 协议 API根路径 模型`；协议为 `chat`、`responses` 或 `anthropic`，凭据从标准输入读取，不写入项目配置。终端直接输入可能被回显，应通过关闭回显的安全输入方式提供。程序只输出检查阶段及状态，不执行本地工具或打印模型正文。不要将凭据写进命令行参数、示例或 Git。
