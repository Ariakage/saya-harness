# 配置加载组件

DEV-018 提供 core 配置解析与校验组件；它尚未接入 CLI，`saya config init`、`saya config check` 和实际任务入口仍待 DEV-019。配置格式、完整示例与产品规则以 [MVP 基线 B-04、B-05 和 B-15](mvp-baseline.md) 为准。

组件只读取一份配置，依次按显式参数、相应 `SAYA_*` 环境变量、文件内容和内置默认值解析；`SAYA_CONFIG` 只选择文件，不会自动加载或合并项目配置。XDG 路径要求绝对路径，否则回退至 HOME 下的标准目录。工作区、附加根和权限规则在加载时检查；文件和目录必须已经存在。组件不会创建目录、调用模型或执行工具。

每个 profile 显式声明协议、API 根 URL、模型、鉴权方式和上下文窗口，模型凭据和额外请求头的值只能通过环境变量名引用。默认没有 profile 或付费模型；未知字段、未定义的 profile、缺失的环境变量引用、不合法的鉴权、权限规则和预算均报错。解析错误不回显原始 TOML 或环境变量值。程序使用环境快照，可在测试中注入；后续 CLI 接线仍须将配置实际应用于模型、执行器和权限控制。

profile 可选 `strict_tools`、`stream`、`allow_remote_http`、`headers_env`，以及兼容网关使用的 `legacy_max_tokens`、`omit_stream_usage`、`omit_reasoning_include`；Anthropic 的 `api_version` 支持基线约定的两个版本。`strict_tools` 默认关闭：当前后台调用的开放参数对象和部分可选字段不能无损转为 OpenAI strict schema，显式开启时会在请求前报错，而不静默改变工具语义。工具参数仍在运行时校验。`commands.inherit_env` 显式列出额外继承的变量名，执行器接线时必须同时传入所有凭据和请求头引用名，避免这些敏感变量进入子进程。
