# CLI 开发预览

当前 CLI 是 P4 的开发预览。`saya run` 接入三种协议，并可在真实工作区运行只读文件／搜索工具；CLI 子进程目前只用本地 Chat Completions 夹具验证，三协议适配器另有 core 级测试。工作区副作用必须等待 P5 持久化完成：无论工作模式、权限模式、预授权或前台／后台调用如何设置，本入口都不派发写文件、补丁或命令。内存会话随进程退出丢失，不能恢复；这不是完整 MVP。

配置格式见[配置组件](configuration.md)和[基线 B-04](mvp-baseline.md)。例如，先用 `saya config init` 创建空模板，填入明确的模型 profile 和凭据环境变量名，再用 `saya config check` 校验；此命令不会覆盖已有文件。默认配置文件位于 `$XDG_CONFIG_HOME/saya/config.toml` 或 `~/.config/saya/config.toml`，也可用 `--config PATH` 选择文件。模板不包含模型或真实凭据。运行示例（假定已经配置默认 profile）：

```sh
saya run 'Inspect this workspace'
saya run --prompt-file task.txt
printf 'Inspect this workspace\n' | saya run --stdin
saya --permissions manual run --interactive --stdin < task.txt
```

三种输入方式互斥，没有隐式读取 stdin。`--interactive` 通过控制终端逐次确认工具调用，并回答 `ask_user` 澄清；无法打开控制终端时按非交互处理。非交互场景需要确认时返回退出码 3，需要澄清时显示问题并返回退出码 4。也可传入 `--approval-file PATH`；命令预授权在持久化前不会启用命令执行。默认 text 输出；JSON、JSONL、持久化、恢复和 `chat` REPL 仍未完成。
