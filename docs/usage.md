# Saya 使用指南

0.2.0已发布默认TUI、增强日志和会话导出，当前N2开发线增加原生隔离、MCP和凭据引用，并已完成四平台整体验收。本指南说明当前开发线行为，版本与实际验收见[开发进度](progress.md)和[N2验收记录](n2-validation.md)。运行模型任务需要兼容所选协议的模型服务和用户配置的凭据。[验收索引](acceptance.md)区分受控替身与真实服务验证。

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

输出token目标省略时继承profile.limit.output声明值，再采用适配器返回的可识别上游output limit，最后回退65536；单个目标上限为1000000。可用profile.max_output_tokens显式覆盖，审核／摘要还可设置用途覆盖。实际请求按剩余上下文动态收缩，既有显式1024／4096等配置继续生效。已保存会话的profile指纹变化时，使用 `--accept-config-change`或TUI的 `/resume-accept`显式接受后恢复。声明元数据及完整优先级见[输出目标配置](configuration.md#模型输出-token-目标)。

## 默认 TUI 与兼容入口

当前开发线中，stdin 和 stdout 均连接终端时，直接运行 `saya` 进入 TUI。全局配置、工作区、profile、工作模式和权限参数继续适用。

```sh
./target/release/saya --config "$config_path"
./target/release/saya --config "$config_path" --cwd /path/to/project --mode code
```

Enter 发送消息，Alt+Enter 或 Ctrl+J 换行，Ctrl+P 打开命令面板。Ctrl+C 取消运行或界面操作，空闲时清空草稿。输入为空时 Ctrl+D 退出，活跃操作先取消并等待收尾。PgUp／PgDn 滚动对话或详情，End 跟随最新对话，Esc 关闭详情。待批准时，y 批准，n 或 Esc 拒绝。需要澄清时在输入框回复。

在单行输入的开头键入 `/`，光标位于末尾时显示命令建议与灰色补全后缀。↑／↓选择，Tab或→填入草稿，Enter执行。Esc先隐藏建议并保留草稿。支持工作模式、权限和导出格式等固定参数，profile名、会话／任务ID与目标路径由用户补齐。`//`可输入字面量斜杠，多行文本继续使用常规编辑。

补全候选显示时，Enter直接执行当前无参数命令或完整的 `/sandbox on|off` 等命令；以空格结尾、仍需用户填写参数的候选只填入编辑器，不会立即执行。

输入框为空时，Tab按WORK→CODE→PLAN→BUILD→WORK循环模式。运行中会显示待生效选择，并在core调用边界应用。已有草稿时继续用于编辑或补全，Ctrl+P命令面板打开时按面板交互处理。

`/new` 新建会话，`/sessions [PAGE]` 浏览，`/resume ID` 恢复后等待输入。`/profiles` 与 `/profile NAME` 选择已配置模型。`/tools`、`/jobs` 查看一致性快照，`/ps` 查看当前Run已接收的后台任务，`/cancel-job ID` 取消当前运行的后台任务。`/export json|markdown PATH` 导出对话，`/export-full json|markdown PATH` 显式包含工具和完整记录。详情、布局和显示上限见[TUI 设计](tui.md)。

运行设置可在空闲界面调整：`/cd PATH` 将当前会话迁移到现有目录并持久保存工作区；相对路径按当前工作区解析。`/model PROFILE` 选择模型配置并建立新的空会话；`/effort low|medium|high|xhigh|max|default` 与 `/thinking` 等价，修改当前进程的思考强度。`/autocompact 950k|1m|TOKENS|auto|off` 设置精确输入触发水位或恢复百分比／关闭自动压缩。`/sandbox on|off|status` 中`status`可随时只读查看，`on/off`仅空闲可切换；这些变更只影响后续运行，权限规则、显式deny和PLAN只读边界继续生效。

`/sessions`打开已保存会话弹窗，展示会话标题、ID、模式、profile和最后状态，Enter恢复选中会话。`/skills`打开用户级与工作区发现的Skill选择器，`/mcps`打开MCP服务选择器，`/plugins`打开已安装插件选择器。选择器支持↑↓、PgUp／PgDn和Enter，Esc返回保留的草稿。MCP服务的Enter操作切换后续运行的启用状态；Skill与Plugin条目Enter打开只读详情。Skill正文激活仍经过运行时Skill工具与权限，Plugin启用／停用仍通过显式 `saya plugin enable|disable` 能力授权。

Agent运行中继续输入普通消息会把内容作为引导补充，在下一个模型边界保存并加入上下文；当前流式响应和已展开工具调用不会被替换。引导队列上限为16条、累计256KiB。`/tools`按工具调用卡片渲染名称、状态、run／call关联、参数和结构化输出，不直接展示整段JSON。`exec`卡片显示实际命令、工作目录、超时、观测耗时与退出码；审批与澄清使用居中弹窗，不占用普通详情侧栏。

从终端粘贴或拖入现有本地图片、音频、视频或其他文件时，输入区分别显示高亮的 `[Image N]`、`[Audio N]`、`[Video N]` 或 `[File N]`，不会暴露绝对路径。图片／音频／视频仅在其profile显式开启对应 `multimodal` 能力时作为原生内容发送；普通File仍携带真实路径。`file://`地址、带空格路径和常见转义形式可识别。附件token作为整体移动和删除。`Ctrl+V`可直接读取系统剪贴板图片，保存到`~/.saya/paste_image/`后插入Image token；没有图片时回退到文本粘贴。

`/markdown source|render`切换Assistant正文的源码／排版视图，`/footnote NUMBER`跳到最近消息的对应脚注，`/footnote back`返回上一引用。debug构建的 `saya tui-preview [SCENE] [--source]`可打开独立的交互式预览，具体语法、公式／图表回退和场景见[Markdown说明](markdown-rendering.md)。

窗口标题为 `Saya | 首条任务的简短标题`。所有尺寸采用黑底外框与灰色蓝条输入区，Ready行右侧显示权限，底栏显示模式、模型、可选thinking effort和输入占比。达到120列×24行时显示会话、上下文分项估算和扩展状态侧栏。环形指示依据最近一次执行请求的输入用量，缺失显示 `—`，分项估算明确标记为Estimated。可在profile中设置 `thinking_effort = "max"` 等级别，三协议均按相应字段发送，支持范围由所选模型决定。[Thinking effort配置](configuration.md#thinking-effort)

`saya chat` 保留文本交互，`run`、`resume`、`session`、`config` 和 `logs` 继续通过显式子命令使用。无子命令的管道／非终端调用、JSON／JSONL 输出冲突返回用法错误2，stdout为空，并在打开配置、会话或终端之前提示选择具体子命令。脚本输入使用 `saya run --stdin`。

Thinking与长工具卡片支持点击标题或Ctrl+O展开，鼠标滚轮控制指针所在的对话／详情区域。全屏Tasks展示模型通过 `todo_write` 维护的会话进度，恢复后沿用最近成功列表。可读推理的服务范围与显示限制见[TUI说明](tui.md#thinking任务清单与折叠)。

## 单次任务与文本会话

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

将 `SESSION_ID` 替换为列表中的实际 ID。也可用 `run --prompt-file PATH` 或 `run --stdin` 提供任务，三种输入方式互斥；`run --interactive` 使用控制终端确认和澄清，即使任务从 stdin 读取。`--output json` 输出最终结果，`jsonl` 输出有序运行事件；会话查询支持文本和 JSON。控制终端提示不会混入机器输出。配置已知凭据时，文本输出可能等待整条模型消息以完成脱敏。启用 MCP 后使用动态凭据集合，模型正文与可读 Thinking 在本次请求完成后按最新凭据快照脱敏展示。

`chat` 中使用 `/mode` 切换 PLAN、BUILD、CODE、WORK；模型只能建议切换。工作模式在当前调用完成后、下一次调用前生效；进入 PLAN 前先清理后台写入和命令。`/permissions` 在空闲时修改权限；`/status`、`/help` 和 `/exit` 分别查看状态、帮助和退出。完整交互命令与 Ctrl-C 行为见 [CLI 开发预览](cli-preview.md)。

## 权限与后台任务

文本 CLI 的流式与完整回答统一替换终端控制字符和双向文本控制符，保留换行、Tab 和正常 Unicode。JSON／JSONL 与已保存历史保留原始文本，由 JSON 编码转义控制字符。

默认工作模式为 WORK，默认权限为 `auto`。PLAN 保持只读，禁止文件写入和通用命令。`auto` 先执行规则判断，必要时进行独立模型审核，不能安全判断时请求人工确认；`manual` 对每次工具调用要求人工批准，包括读取、搜索和后台状态查询。`bypass` 跳过人工确认及路径限制，但 PLAN 和显式 deny 仍然生效。当前 N2 开发线默认使用原生 `sandbox.backend = "auto"`，权限决定许可，执行环境落实独立的文件／网络约束。`bypass` 继续受实际原生执行边界约束。0.2.0 的无 OS 沙箱限制保留在[历史发行说明](release-notes-0.2.0.md)。

非交互任务无法取得必要确认时返回 `needs_confirmation`（退出码 3）；需要用户澄清时返回 `needs_input`（退出码 4）。对确切命令可使用一次运行内的[预授权示例](../examples/approval.example.json)：

```sh
./target/release/saya --config "$config_path" --permissions manual run --approval-file examples/approval.example.json 'Run /bin/echo with the argument hello'
```

该文件仅匹配工作区内默认 cwd 的 `[/bin/echo, hello]`，不会自行运行命令；只有模型提出匹配的调用才会使用，且不覆盖 PLAN、deny 或路径规则。预授权不会在恢复会话后继承。使用者应在授权前检查完整 argv 和 cwd；详细规则见[执行与恢复契约](execution.md)。文件写入要求父目录已存在；需要创建目录时须通过适用权限允许的命令完成。

模型可显式启动运行内后台工具；交互期间 `/jobs`、`/job ID`、`/cancel-job ID` 可查询和取消。运行结束会清理未完成任务。Saya 被强制终止后，失去管理的进程可能仍在运行；恢复时不重放工具，结果未知不等于已停止。

## 原生隔离与 MCP

默认 Auto 在 macOS 选择 Seatbelt，在 Linux 选择 Bubblewrap 与 Seccomp。Linux 代理网络使用 socat 与宿主 NetworkBroker。缺少依赖或必需能力时，Run 返回明确启动错误。`config check` 验证配置，实际隔离能力在 Run 开始时探测。

工作区默认进入原生视图。额外工具链目录通过 `sandbox.read_roots` 显式提供，原生文件工具的逻辑权限仍由工作区与 `permissions.additional_roots` 控制。默认网络为 `deny`，代理 origin、资源限额和显式 Host 兼容模式见[配置](configuration.md#n2-原生隔离配置)。已有凭据存储或活动 `credential_ref` 会使 Host 启动失败。

MCP 的 stdio 启动、HTTP 服务、宿主只读分类和凭据管理见[MCP 使用说明](mcp.md)。配置文件中的服务默认仅是待连接项，运行实际连接并完成目录发现后，TUI 侧栏才显示 Connected 和真实工具数。结束时清理服务，JSONL 可看到 `mcp.status` 生命周期事件。

```sh
saya --config /path/to/config.toml mcp credentials list
saya --config /path/to/config.toml mcp login remote --scope files:read --open-browser
saya --config /path/to/config.toml mcp credentials revoke RETURNED_REFERENCE
```

登录在宿主交互终端中完成，运行期缺失授权时返回 `needs_auth`。凭据引用绑定服务器、精确 endpoint、批准 scope 以及 OAuth 的 issuer／canonical resource，令牌刷新与撤销由宿主处理。工具已执行但无法确认完成时保留 `unknown`，恢复与重连继续遵守执行记录，原始工具调用保持单次派发。

## 项目上下文、持久化与用量

Saya 发现用户级及工作区的约定项目指令与 Skill 元数据，工作区同名项优先；加载范围及信任边界见[项目上下文](project-context.md)。`--preferences-file PATH`、`--memory-file PATH` 可显式提供本次调用的 UTF-8 偏好和记忆文本，各限 64 KiB；内容会发送给模型，不会自动发现或作为持久记忆维护。文件含当前配置的已知凭据时，在模型请求前拒绝，检测包括完整 JSON、Markdown 围栏和普通正文中的嵌套 JSON 转义与 Unicode 转义。检测视图最多解码256层，累计扫描上限64 MiB，超限沿用失败关闭边界。通过检测的个人文本按原文发送给模型。

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

原生隔离与 MCP 已完成 N2 固定源码验收；可执行插件、自动／手动上下文压缩、检查点和全局记忆已接入共享入口并完成 N3 固定源码验收。Goal、Browser 和 GUI 继续按[能力路线图](capability-roadmap.md)推进；后续增量的验证范围以[开发进度](progress.md)为准。

## 会话导出

当前开发线可离线导出已保存会话，父目录须已存在，目标使用独占创建和0600权限。默认 Markdown、transcript 和省略工具正文，可显式选择完整范围与工具正文。

```sh
saya session export SESSION_ID --file ./session.md
saya --output json session export SESSION_ID --format json --scope full --include-tools --file ./session.json
```

相对导出路径以本次配置的工作区cwd为基准。当前调用显式使用PLAN时，目标须同时位于本次工作区和被导出会话保存的工作区外。JSON／JSONL 的确认包含文件路径、字节数、SHA256及快照水位。取消退出130，其他导出错误退出2，错误码及文件发布保证见[导出说明](session-export.md)。

## 本地日志查询

当前开发线支持 trace、debug、info、warn、error 五级诊断，默认阈值为warn。优先级为命令行、SAYA_LOG_LEVEL、默认值。日志与运行JSON／JSONL的stdout分别处理。

```sh
saya --log-level debug run "Inspect this project"
saya logs show --session SESSION_ID --level info --limit 50
saya --output jsonl logs tail --session SESSION_ID
```

show支持text／json／jsonl，tail支持text／jsonl并以Ctrl-C退出。查询支持run、时间范围、offset和limit，具体字段与轮转边界见[日志说明](observability.md)。

### 凭据检测边界

已知凭据按原文及逐字符JSON／Unicode混合转义检测。无法安全清除的残留正文整体扣留。重复键或超范围数字构成的无凭据JSON文本保持原文，含凭据且无法无损处理时失败关闭。无反斜杠纯文本不占用转义解码预算。

### 流式正文完成

文本模式按每次模型请求跟踪已显示的正文前缀。完成事件独立提供回答或补充后缀时，终端显示缺失内容。少数合法交错输出使最终正文顺序与已显示增量不一致时，会换行展示完整已校验回答，可能再次显示部分内容。协议原生thinking及opaque字段不进入该正文输出。
