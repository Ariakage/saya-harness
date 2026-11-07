# 配置加载组件

配置解析已接入 CLI；`saya config init`、`saya config check` 和实际任务入口可使用该组件。配置格式、完整示例与产品规则以 [MVP 基线 B-04、B-05 和 B-15](mvp-baseline.md) 为准。

组件只读取一份配置，依次按显式参数、相应 `SAYA_*` 环境变量、文件内容和内置默认值解析；`SAYA_CONFIG` 只选择文件，不会自动加载或合并项目配置。XDG 路径要求绝对路径，否则回退至 HOME 下的标准目录。工作区、附加根和权限规则在加载时检查；文件和目录必须已经存在。组件不会创建目录、调用模型或执行工具。

每个 profile 显式声明协议、API 根 URL、模型、鉴权方式和上下文窗口，模型凭据和额外请求头的值只能通过环境变量名引用。默认没有 profile 或付费模型；未知字段、未定义的 profile、缺失的环境变量引用、不合法的鉴权、权限规则和预算均报错。解析错误不回显原始 TOML 或环境变量值。程序使用环境快照，可在测试中注入；CLI 将配置应用于模型、执行器和权限控制。

profile 可选 `strict_tools`、`stream`、`allow_remote_http`、`headers_env`，以及兼容网关使用的 `legacy_max_tokens`、`omit_stream_usage`、`omit_reasoning_include`；Anthropic 的 `api_version` 支持基线约定的两个版本。`strict_tools` 默认关闭：当前后台调用的开放参数对象和部分可选字段不能无损转为 OpenAI strict schema，显式开启时会在请求前报错，而不静默改变工具语义。工具参数仍在运行时校验。`commands.inherit_env` 显式列出额外继承的变量名，执行器接线时必须同时传入所有凭据和请求头引用名，避免这些敏感变量进入子进程。

`limits.write_max_bytes` 限制原生写入／补丁的输入和输出字节数，默认 1 MiB。CLI 将该值传到运行共享的调度器，前台和后台使用相同上限；零值会使配置校验失败。

可为 profile 显式配置成本估算费率；下例中的金额仅是格式示例，不代表任何厂商价格：

```toml
[profiles.example.pricing]
currency = "USD"
input_microunits_per_million = 1000000
output_microunits_per_million = 2000000
```

`currency` 是用户指定的三位大写币种代码；两个整数分别表示每百万输入／输出 token 的价格，单位为币种的百万分之一。例如 1000000 微单位等于该币种 1 单位。只有费率和对应请求的完整 token 用量均已知时才计算估算；结果向上取整到微单位并标记为估算，执行与审核 profile 分开记录，不推断缓存价格、账单或汇率。价格变更不改变模型身份指纹；目前没有金额上限配置，运行限制仍使用已确认的次数、时间与 token 规则。
