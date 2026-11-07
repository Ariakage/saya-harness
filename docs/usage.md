# Saya CLI 使用指南

Saya 当前为 CLI 开发预览。本指南中的运行示例需要可访问且兼容所选协议的模型服务；仓库不提供模型账号、凭据或模型服务。[验收索引](acceptance.md)区分本机替身测试与真实服务验证，发行平台和最低环境验收仍属 P7。

## 构建与配置

在仓库根目录构建；项目固定 Rust 1.99.1，`rustup` 可按仓库的工具链文件安装所需工具链：

```sh
cargo build --workspace --release --locked
./target/release/saya --help
```

复制[示例配置](../examples/config.example.toml)后修改模型、API 根地址与实际上下文窗口。示例指向本机占位服务，不会启动服务：

```sh
config_path="$(mktemp -d)/saya.toml"
cp examples/config.example.toml "$config_path"
./target/release/saya --config "$config_path" config check
```

`config check` 检查配置及环境引用，**不会连接模型服务**。也可用 `saya --config PATH config init` 新建空模板，再自行填写 profile；该命令拒绝覆盖已有文件。不指定 `--config` 时使用 `$XDG_CONFIG_HOME/saya/config.toml`，或 `~/.config/saya/config.toml`；`SAYA_CONFIG` 也可选择配置文件。优先级为 CLI 参数、相应环境变量、配置文件、默认值。

支持 `openai-chat-completions`、`openai-responses`、`anthropic-messages`。`base_url` 是 API 根地址，程序分别追加 `/chat/completions`、`/responses`、`/messages`；应核对服务商实际接口，兼容协议不代表支持全部参数。远程明文 HTTP 默认禁用。本机样例无需鉴权；远程服务可参考样例中注释的 profile，凭据只通过 `api_key_env` 指定**环境变量名**，值由使用者在启动前安全提供，不写入配置、提交或日志。配置校验会检查所有 profile 引用的环境变量，即使该 profile 未选中。具体鉴权和可选参数见[配置](configuration.md)。

## 单次任务与会话

以下示例假定在仓库根目录执行，`config_path` 指向已修改且可连接的配置；如要使用其他工作区，在子命令前加全局参数 `--cwd PATH`。模型可能根据任务请求工具，执行前按当前权限策略判断：

```sh
./target/release/saya --config "$config_path" --mode plan run 'Inspect this workspace and propose a plan'
./target/release/saya --config "$config_path" --output json run 'Summarize this workspace'
./target/release/saya --config "$config_path" --output jsonl run 'Summarize this workspace'
./target/release/saya --config "$config_path" chat
./target/release/saya --config "$config_path" session list --page 1
./target/release/saya --config "$config_path" session show SESSION_ID
./target/release/saya --config "$config_path" resume SESSION_ID
./target/release/saya --config "$config_path" run --session SESSION_ID 'Continue the task'
```

将 `SESSION_ID` 替换为列表中的实际 ID。也可用 `run --prompt-file PATH` 或 `run --stdin` 提供任务，三种输入方式互斥；`run --interactive` 使用控制终端确认和澄清，即使任务从 stdin 读取。`--output json` 输出最终结果，`jsonl` 输出有序运行事件；会话查询支持文本和 JSON。控制终端提示不会混入机器输出。配置已知凭据时，文本输出可能等待整条模型消息以完成脱敏。

`chat` 中使用 `/mode` 切换 PLAN、BUILD、CODE、WORK；模型只能建议切换。工作模式在当前调用完成后、下一次调用前生效；进入 PLAN 前先清理后台写入和命令。`/permissions` 在空闲时修改权限；`/status`、`/help` 和 `/exit` 分别查看状态、帮助和退出。完整交互命令与 Ctrl-C 行为见 [CLI 开发预览](cli-preview.md)。

## 权限与后台任务

默认工作模式为 WORK，默认权限为 `auto`。PLAN 保持只读，禁止文件写入和通用命令。`auto` 先执行规则判断，必要时进行独立模型审核，不能安全判断时请求人工确认；`manual` 对每次工具调用要求人工批准，包括读取、搜索和后台状态查询。`bypass` 跳过人工确认及路径限制，但 PLAN 和显式 deny 仍然生效。**当前没有操作系统沙箱**；命令启动路径的检查不等于对子进程文件系统或网络的隔离。

非交互任务无法取得必要确认时返回 `needs_confirmation`（退出码 3）；需要用户澄清时返回 `needs_input`（退出码 4）。对确切命令可使用一次运行内的[预授权示例](../examples/approval.example.json)：

```sh
./target/release/saya --config "$config_path" --permissions manual run --approval-file examples/approval.example.json 'Run /bin/echo with the argument hello'
```

该文件仅匹配工作区内默认 cwd 的 `[/bin/echo, hello]`，不会自行运行命令；只有模型提出匹配的调用才会使用，且不覆盖 PLAN、deny 或路径规则。预授权不会在恢复会话后继承。使用者应在授权前检查完整 argv 和 cwd；详细规则见[执行与恢复契约](execution.md)。文件写入要求父目录已存在；需要创建目录时须通过适用权限允许的命令完成。

模型可显式启动运行内后台工具；交互期间 `/jobs`、`/job ID`、`/cancel-job ID` 可查询和取消。运行结束会清理未完成任务。Saya 被强制终止后，失去管理的进程可能仍在运行；恢复时不重放工具，结果未知不等于已停止。

## 项目上下文、持久化与用量

Saya 发现用户级及工作区的约定项目指令与 Skill 元数据，工作区同名项优先；加载范围及信任边界见[项目上下文](project-context.md)。`--preferences-file PATH`、`--memory-file PATH` 可显式提供本次调用的 UTF-8 偏好和记忆文本，各限 64 KiB；内容会发送给模型，不会自动发现或作为持久记忆维护。

会话数据库默认在 `$XDG_DATA_HOME/saya/sessions.sqlite3`，未设置时在 `~/.local/share/saya/sessions.sqlite3`；`--data-dir` 可选择数据目录。警告和错误日志默认在 `$XDG_STATE_HOME/saya/saya.log` 或 `~/.local/state/saya/saya.log`，按 10 MiB 轮转并保留 5 个备份。PLAN 要求数据、状态及缓存目录不落在工作区内；相关 XDG 变量须为绝对路径。[存储说明](storage.md)介绍锁、WAL、迁移备份与故障恢复。

恢复会话沿用历史和工作模式，`run --session` 和 `resume` 均可通过显式 `--mode` 覆盖并保存工作模式；切入 PLAN 前先检查存储目录边界。恢复重新读取项目上下文，采用本次启动的权限；不恢复历史授权或自动重放。模型／协议改变需要新会话，其他配置变化需显式 `--accept-config-change`；工作区迁移可通过 `--cwd` 指定。运行详情中的 `run_metrics` 区分执行及审核用量，服务未报告时为 null。只有显式配置费率且用量完整才估算成本，结果不是账单；`artifacts` 仅列出已确认成功的 Saya 文件写入。

已知配置凭据会在相关输出、记录和日志中遮蔽；不能安全修改的不透明签名内容会使保存失败。未知秘密不会被保证识别，使用者仍应避免把敏感信息放入任务和项目文件。

## 故障处理

- **配置校验失败**：检查默认 profile、协议、API 根地址、模型、上下文窗口和全部引用的环境变量；不要在求助信息中附上密钥。`config check` 成功不证明网络或鉴权成功。
- **连接或协议失败**：检查服务是否运行、根地址及接口是否支持所选协议；查看脱敏后的 CLI 错误和本地日志。示例服务地址本身只是占位。
- **需要确认或澄清**：交互环境可使用 `run --interactive`；脚本可按需提供精确预授权文件，或处理退出码 3／4。预授权不放宽规则或 PLAN 限制。
- **PLAN 存储路径冲突**：将数据、状态和缓存目录放在工作区之外后重试，包括通过 XDG 变量选择的目录。
- **会话正在使用、配置改变或恢复失败**：先确认是否有另一 Saya 进程持有会话锁；核对 profile 和工作区。不要删除数据库或 WAL 来强行解锁；涉及损坏时先保留原文件及备份，再调查错误。
- **日志不可写**：检查状态目录权限与剩余空间；日志路径不应指向不安全的符号链接。

当前不提供 OS 沙箱、MCP、插件系统、自动上下文压缩、TUI 或 GUI；后续规划见[能力路线图](capability-roadmap.md)。

## 会话导出

当前开发线可离线导出已保存会话，父目录须已存在，目标使用独占创建和0600权限。默认 Markdown、transcript 和省略工具正文，可显式选择完整范围与工具正文。

```sh
saya session export SESSION_ID --file ./session.md
saya --output json session export SESSION_ID --format json --scope full --include-tools --file ./session.json
```

当前调用显式使用 PLAN 时，目标须位于工作区外。JSON／JSONL 的确认包含文件路径、字节数、SHA256及快照水位。取消退出130，其他导出错误退出2，错误码及文件发布保证见[导出说明](session-export.md)。
