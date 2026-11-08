# 真实模型联调记录

## DEV-030：2026-11-07

在 macOS aarch64 上以 Saya release CLI 和用户提供的测试账户，使用模型 `deepseek-flash` 验证以下接口。测试服务未提供可独立核实的模型快照版本号，不能据此推断版本或覆盖其他模型。

| 协议 | 流式直接回答 | 流式读取工具及回传 | 结果 |
| --- | --- | --- | --- |
| OpenAI Chat Completions | 通过 | 通过，2 次成功模型响应 | 通过 |
| OpenAI Responses | 通过 | 通过，3 次成功模型响应 | 通过 |
| Anthropic Messages，`2026-10-21` | 通过 | 通过，2 次成功模型响应 | 通过 |

每组使用独立 PLAN 会话，读取任务的临时工作区内含一次性随机标记。工具事件显示 `read_file` 成功，最终模型文本包含该标记，证明工具结果已到达模型；测试后工作区文件和目录结构未变化。直接回答检查收到指定标记。CLI 以 JSONL 输出；命令退出码、终结事件和模型／工具事件均被检查。

凭据从环境变量传入，仅在子进程内使用；结果只记录检查状态和计数，不记录密钥、模型回复或随机标记。该结果是单个服务、模型和时间点的小规模联调，不代表其他供应商、协议可选功能、模型对复杂任务的质量或提示注入防护。Anthropic 另一 API 版本和受控四类任务仍以离线测试为证据，未在此轮用真实模型覆盖。

操作者显式设置 `SAYA_LIVE_API_KEY`，通过 [live_protocol_smoke.py](../scripts/live_protocol_smoke.py) 的 `--binary PATH` 选择程序，按 `--help` 填入模型、三个 API 根地址和上下文窗口。Chat Completions 默认使用 `max_completion_tokens`，服务需要 `max_tokens` 时显式添加 `--legacy-max-tokens`。脚本会向选定服务发出真实请求，可能产生费用，凭据从环境变量传入。

## 四模式真实任务补充抽样：2026-11-07

同一服务和 `deepseek-flash` 模型使用 Chat Completions 跑四个临时任务；结果适用于本次模型抽样，不代表三种协议或其他模型对四类任务均已验证。每次运行限制 10 次模型请求、24 次工具调用，非交互并显式 deny `exec`；BUILD 和 CODE 的模型生成 Rust 源码由测试方在无网络、无宿主挂载、只读根文件系统且限制权限和资源的 Linux 容器中独立编译运行。容器不是对内核逃逸的绝对保证。

| 模式 | 任务与观察 | 结果 |
| --- | --- | --- |
| PLAN | 读取 Rust 示例，说明空白字符计数缺陷并列出测试计划；比较工作区文件与目录 | 通过 |
| BUILD | 在预先存在 `src/` 的临时工程中编写文本统计程序；独立编译并用含连续空格和换行的输入检查行数、词数 | 通过 |
| CODE | 读取并修复空白分隔缺陷；独立编译运行同一输入 | 通过 |
| WORK | 读取三份数据并生成含各数据与总和的报告 | 通过 |

初次实验中，PLAN 因测试配置将输出上限设为 2048 token 而返回 `output_limit`；改用默认 8192 并要求简短计划后通过。BUILD 初次遇到不存在的父目录：文件工具不会自动创建目录，测试又显式拒绝 `exec`，因此模型请求人工帮助并返回 `needs_input`；在预创建 `src/` 后通过。CODE 初次使用测试容器的不可执行临时挂载而误报验证失败，改为允许临时目录执行后独立编译运行通过；模型曾先生成不匹配路径的补丁，随后使用版本校验写入修复。WORK 初次即通过。之后四项使用修正后的条件完整重跑通过。

[四模式联调脚本](../scripts/live_modes_smoke.py)通过 `--binary PATH` 选择待测程序，省略时使用当前目录下的 `target/release/saya`。操作者显式提供凭据环境变量、可信 Chat Completions API 根地址、模型和带 Rust 编译器的 Docker 镜像。Docker 默认按所连接引擎选择平台，也可通过 `--sandbox-platform` 或 `SAYA_LIVE_SANDBOX_PLATFORM` 覆盖。Chat 参数默认使用 `max_completion_tokens`，复测上述旧参数兼容场景时添加 `--legacy-max-tokens`。脚本使用临时工作区，结束时清理，输出检查状态与计数。上述历史结果仍归属于当时记录的模型、程序和日期。

## 审查修复后复测

2026-11-07 从 `854be1b` 的已校验 macOS ARM 候选包解出程序，使用本地私有凭据配置进行三协议真实模型 smoke。Chat Completions、Responses 和 Anthropic Messages 各自完成直接回答，以及读取随机标记文件并回传结果，两类检查共六项全部通过。读取任务各有一个成功文件工具结果与两次成功模型响应，测试工作区保持不变。

本轮使用同一 `deepseek-flash` 模型和此前的服务根地址，Anthropic 版本为 `2026-10-21`。后续发行构建新增打包和验证工具，运行时代码、提示词与 Cargo 依赖相对该修复版本保持相同内容。检查脚本仅保存状态及计数，凭据继续保存在 Git 本地排除的私有文件中。

## 0.2.0-rc.2 三协议与四模式抽样：2026-11-09

本次使用源码 `daee043821529e270b876a77d5c6e8f47bed1c33` 对应的最终 macOS aarch64 候选程序，来自候选工作流 `42518738474`。程序与归档的摘要已与本轮真实模型记录核对一致。测试时间为2026-11-08 20:18:01至20:18:55 UTC，对应本地2026-11-09。[候选工作流](https://github.com/Ariakage/saya-harness/actions/runs/42518738474)、[RC2 发行证据](https://github.com/Ariakage/saya-harness/releases/download/v0.2.0-rc.2/release-evidence.json)

| 项目 | SHA-256 |
| --- | --- |
| macOS aarch64 程序 | `cc7174a02970267933ca302c624ae3a659e85c9341a765c930ad8b9d12153c7a` |
| macOS aarch64 归档 | `e8e8544419153aea06a89d64b9c96fbff31a3197b807b9a6f02940dda0db048d` |

使用同一 DeepSeek 服务与 `deepseek-flash` 模型，三种协议分别执行 PLAN、BUILD、CODE、WORK 四种任务，共12项全部通过。合计42次模型请求及42次成功响应，观察到的服务失败为0。工具请求36次，成功35次，失败1次。

| 协议 | 模式 | 模型请求／成功响应 | 工具请求／成功结果 | 任务结果 |
| --- | --- | --- | --- | --- |
| OpenAI Chat Completions | PLAN | 2／2 | 1／1 | passed |
| OpenAI Chat Completions | BUILD | 3／3 | 2／2 | passed |
| OpenAI Chat Completions | CODE | 5／5 | 4／3 | passed |
| OpenAI Chat Completions | WORK | 4／4 | 5／5 | passed |
| OpenAI Responses | PLAN | 2／2 | 1／1 | passed |
| OpenAI Responses | BUILD | 4／4 | 3／3 | passed |
| OpenAI Responses | CODE | 4／4 | 3／3 | passed |
| OpenAI Responses | WORK | 4／4 | 5／5 | passed |
| Anthropic Messages | PLAN | 2／2 | 1／1 | passed |
| Anthropic Messages | BUILD | 4／4 | 3／3 | passed |
| Anthropic Messages | CODE | 4／4 | 3／3 | passed |
| Anthropic Messages | WORK | 4／4 | 5／5 | passed |

Chat Completions 的 CODE 运行包含两次 `apply_patch` 请求，一次失败、一次成功，最终运行状态为 `completed`，产物与预期静态内容精确匹配。保留记录没有单次失败的错误码、原因或结果顺序，本文仅记录计数与最终状态。

三个 PLAN 工作区均保持不变，整轮 `exec` 请求和成功执行数均为0。BUILD／CODE 生成的 Rust 源码仅做精确静态文件比较，未编译或执行。每项任务预算为6次模型请求、12次工具调用和180秒活跃时间。[工作模式与权限](execution.md)、[模型协议](model-protocols.md)

原配置中的 `stream = true` 和 `thinking_effort = "max"` 全程保留，本轮没有调整兼容参数。Chat Completions 沿用原私有配置的 `legacy_max_tokens = true`，Anthropic Messages 沿用 `api_version = "2026-10-21"`。含已知凭据时，CLI 对文本事件作安全合并，HTTP wire 分块未独立观察。服务也未独立报告内部实际采用的 effort，配置请求值与服务内部行为分别记录。[Thinking effort 配置](configuration.md#thinking-effort)

证据只保留状态、计数与摘要，原任务提示词、模型回复、生成文件和临时会话数据库已在验证后删除，凭据与完整请求未进入保留证据。本结果限于本次单个服务、模型和时间点的抽样，其他模型兼容性、任务质量与生成程序的运行正确性需另行验证。[RC2 版本范围](release-notes-0.2.0-rc.2.md)
