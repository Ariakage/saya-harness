# 协议夹具

Chat Completions、Responses 和 Anthropic Messages 的适配器与受控 HTTP／SSE 验证已实现。协议载荷在 [chat.rs](../../chat.rs)、[responses.rs](../../responses.rs) 和 [anthropic.rs](../../anthropic.rs) 中构造，共用 [HTTP 夹具](../http_support/mod.rs)。

本目录保留为独立协议数据文件的入口，当前载荷由上述 Rust 测试维护。
