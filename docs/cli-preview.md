# CLI 开发预览

当前 CLI 是 P5 的开发预览。`saya run` 与 `saya chat` 接入三种协议，并可在真实工作区运行只读文件／搜索工具。工作区副作用必须等待 P5 恢复边界完成：无论工作模式、权限模式、预授权或前台／后台调用如何设置，本入口都不派发写文件、补丁或命令。本地会话已接入 SQLite；故障恢复、迁移与并发验收仍在开发，这不是完整 MVP。

配置格式见[配置组件](configuration.md)和[基线 B-04](mvp-baseline.md)。例如，先用 `saya config init` 创建空模板，填入明确的模型 profile 和凭据环境变量名，再用 `saya config check` 校验；此命令不会覆盖已有文件。默认配置文件位于 `$XDG_CONFIG_HOME/saya/config.toml` 或 `~/.config/saya/config.toml`，也可用 `--config PATH` 选择文件。模板不包含模型或真实凭据。运行示例（假定已经配置默认 profile）：

```sh
saya run 'Inspect this workspace'
saya run --prompt-file task.txt
printf 'Inspect this workspace\n' | saya run --stdin
saya --permissions manual run --interactive --stdin < task.txt
saya chat
saya session list
saya session show SESSION_ID
saya resume SESSION_ID
saya run --session SESSION_ID 'Continue the task'
```

系统提示词在每次模型请求时注入系统时钟采样（UTC、本地时间及当时的 UTC 偏移）、宿主操作系统和架构、工作区及当前工作／权限模式。时钟可能随请求耗时变旧；不会推断用户地理位置或提供独立 `date` 工具。项目指令和 Skill 仍以带来源的数据提供。

可通过 `--preferences-file PATH` 和 `--memory-file PATH` 显式向本次调用提供用户偏好及长期记忆文本，例如 `saya --preferences-file prefs.md --memory-file memory.md run 'Continue'`。各文件必须为 UTF-8、最多 64 KiB；内容会发送给所选模型，包含已配置凭据时拒绝发送。Saya 不自动发现、维护或保存这些文件，内容属于低信任上下文，不能授予权限。

对明确支持 Anthropic Messages `cache_control` 的服务，可用 `--anthropic-prompt-cache` 在静态系统提示词末尾设置 5 分钟 ephemeral 缓存断点；兼容服务默认不启用此标记。OpenAI 格式和 DeepSeek 使用服务端自动前缀缓存，不发送 Anthropic 专用字段。缓存命中取决于厂商、模型、最小 token 阈值、请求顺序和缓存可用性，不能保证命中或节省费用。动态时间之后的内容不能复用为跨请求静态前缀。

三种单次输入方式互斥，没有隐式读取 stdin。`--interactive` 通过控制终端逐次确认工具调用，并回答 `ask_user` 澄清；无法打开控制终端时按非交互处理。非交互场景需要确认时返回退出码 3，需要澄清时显示问题并返回退出码 4。也可传入 `--approval-file PATH`；命令预授权在持久化前不会启用命令执行。

`saya` 和 `saya chat` 启动连续对话；没有默认 profile 时可在终端选择已配置的 profile。交互命令为 `/mode`、`/permissions`、`/status`、`/cancel`、`/help`、`/exit`；使用 `//` 输入以 `/` 开头的普通文本。工作模式可在运行中请求切换，权限模式仅能在空闲时修改。第一次 Ctrl-C 取消当前运行并返回提示符；再次按 Ctrl-C 可强制退出，空闲时 Ctrl-C 退出。

会话列表默认显示 20 条，`--page N` 翻页，查询可使用 `--output json`；运行的 JSON 和 JSONL 仍未完成。恢复沿用会话工作模式和 profile，但采用本次启动的权限，不重放工具或恢复授权。模型／协议变化要求新会话；相同模型的其他配置变更需 `--accept-config-change`。工作区移动后通过 `--cwd` 显式重新关联。

为防止恢复过程在 PLAN 工作区产生数据库文件，恢复前使用会话安全检查点或只读预检；旧会话遗留 WAL 可通过校验后的内存快照预检。恢复会将中断的运行记为失败，区分未执行、等待确认、输入中断和结果不明，补齐工具调用结果并展示恢复提示，不自动重放。若执行结果未能保存，CLI 显示已观察结果并停止当前运行；交互会话也会退出，避免继续调度。
