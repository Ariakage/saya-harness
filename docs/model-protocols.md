# 模型协议接入

**状态：共享 HTTP／SSE 与 Chat Completions 适配器已通过本地夹具验证；Responses、Anthropic 与真实模型联调仍在开发。**

`crates/saya-core/src/provider/transport.rs` 为适配器提供异步 JSON 请求、完整 JSON 响应和增量 SSE 事件读取。它保留 `base_url` 的自定义路径前缀；接口路径由适配器固定提供。配置需要显式提供鉴权方式、额外请求头的环境变量引用及连接／空闲／总超时。密钥不进入错误信息。

默认验证 HTTPS 证书；HTTP 仅在本机回环地址可用，访问远端 HTTP 需要明确启用。客户端使用系统代理环境配置，不自动跟随重定向，也不自行重试；重试由运行时负责计数。额外请求头不得覆盖鉴权、版本、连接或 HTTP 消息边界字段。流式解析按行处理跨块 UTF-8、CR／LF、注释和多行 data；事件及完整 JSON 有大小限制，EOF 本身不表示模型响应成功。

`crates/saya-core/tests/transport.rs` 启动临时本地 HTTP 服务并验证路径、鉴权、版本、环境变量引用、SSE 分片、格式错误、限额、状态映射、取消、空闲超时及不跟随重定向；系统代理使用单独进程验证。未连接真实模型，未验证其他平台的代理及 TLS 行为。

## Chat Completions

`provider::chat::ChatModel` 使用明确的模型、profile、上下文窗口和输出上限。它以本地历史构造消息，显式发送 `store: false`，支持流式与完整 JSON、文本和多个工具调用，并在流完整结束后才返回完整调用；工具结果按原始 call ID 回传。usage 可用时保留 provider 数字。长度截断、拒绝、格式错误和缺少终止标志不会作为成功返回。收到非文本流事件后也不自动重试。

profile 明确选择 strict 时，无法在不改变工具语义的情况下转换的开放对象或可选字段会报配置错误；兼容网关可显式关闭 strict。兼容选项可改用 `max_tokens` 或省略流式 usage 请求。上下文保护使用本地估算加输出预留，不等同于供应商实际 token 计数，不自动删除历史。

Responses、Anthropic、profile 核查和三协议后台工具映射分别由 DEV-015 至 DEV-017、DEV-034 完成；真实服务联调不能由本地夹具代替。
