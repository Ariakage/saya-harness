# 全局记忆与模型编辑

**更新于2026-11-10。N3开发线已包含全局存储、运行时投影、专用模型工具及CLI／TUI管理入口，并由固定源码 `e1da373` 完成四平台集成与联合验收。实际结果由[开发进度](progress.md)和[N3验收](n3-validation.md#四平台最终验收)维护。**

默认记忆根为 `~/.saya`，索引为 `~/.saya/MEMORY.md`，主题资料位于 `~/.saya/memory/`。宿主可以在显式运行配置中选择其他绝对根路径。读取缺失根或空索引保持只读，首次目录及索引发布沿已授权写入流程进行。

参考材料中的分层读取、来源版本、增量整理、更正与遗忘已按Saya的权限和存储契约定制。原始材料的来源身份与取舍见[记忆提示词来源](prompt/drafts/n3/memory-sources.md)。运行时英文规则与后续独立整理任务草案分别维护。

## 配置与显式文件

读取、自动注入和写入分别由 `[memory]` 的 `read`、`inject`、`write` 控制，默认均为 `true`。`read = false` 关闭模型读取／检索与自动事实注入；`inject = false` 关闭自动注入，保留获准的主动查询；`write = false` 关闭更新、遗忘、回滚和备份恢复。PLAN始终只读。完整字段见[配置](configuration.md#全局记忆)。

`--memory-file` 和 `--preferences-file` 继续显式读取UTF-8文本，作为本次运行的个人上下文来源。它们与全局MemoryStore的根、版本、专用工具及三个开关分别处理。调用者应按当前任务选择这些显式输入。[使用指南](usage.md)

自动注入是宿主上下文装配的一部分。宿主提供有界路由和相关事实，显式deny／ask规则可以抑制相应注入。模型发起的补充读取、检索和证据查询均进入普通工具授权流程。Manual Approval对这些调用逐次确认。

## 文件、来源与作用域

索引采用版本1的可读Markdown格式，包含 `saya-memory`、`saya-entry`、`saya-fact` JSON元数据注释及结束标记。entry、fact和source使用稳定ID。事实携带kind、来源版本、alias和derived_from，分别标记明确偏好、用户更正、观察事实、任务决定和推测。

global条目与当前workspace条目组成可见视图。WorkspaceIdentity由真实工作区路径及设备／inode派生，同名仓库和其他checkout具有各自身份。检索和备份按这一可见范围过滤。来源reference为不透明的证据身份，实际源内容的读取仍经过对应接口与权限。

主题路径限于 `memory/` 内的规范相对Markdown路径，经目录句柄与NOFOLLOW读取。默认索引512KiB，单主题64KiB，主题合计1MiB，最多256条entry、1024条fact／source、64个主题，单事实16KiB。历史默认保留32个索引版本。外部修改参与当前revision，解析或版本失败返回明确错误并保留原文件。

宿主保护 `MEMORY.md` 和 `memory/`，通用文件工具及原生文件worker执行相同保护。其他 `.saya` 指令与Skills仍遵循各自发现路径。已知凭据在索引、主题、提案、审阅、结果及导出边界校验，动态凭据变化会重新验证待执行内容。

## 模型工具

当前请求中的工具manifest与schema决定实际可调用集合。

| 工具 | 行为与边界 |
| --- | --- |
| `memory_read` | 按entry／fact ID分页读取当前事实，或读取当前索引声明的topic_path。主题读取要求当前expected_revision，结果标记片段范围与完整性 |
| `memory_search` | 对当前作用域内的正文、标题、alias及来源做有界文本检索，游标绑定查询与快照版本 |
| `memory_evidence` | 发现当前Session中实际存在的来源selector、hash与观察状态，默认最新32项，使用next_cursor读取较早来源 |
| `memory_update` | 提供operation_id、expected_revision、scope及每项事实的证据selector。已有事实使用稳定ID更正，未提交的已有事实继续保留 |
| `memory_forget` | 提供operation_id、expected_revision、scope及明确fact_ids，结果说明活动视图清理和仍保留范围 |

证据目录只返回宿主历史元数据，read或write启用时提供，PLAN可用。`memory_evidence` 的 `user_message.ordinal` 是原始持久历史中的一基UserText序号，Compact后保持同一来源身份。`tool_result.call_id` 绑定真实且闭合的调用／结果对。证据目录提供身份和状态，具体事实仍需具备相应的可见原文依据。既有记忆工具的输出从独立事实来源中排除，避免将旧记忆循环转述成新证据。

global事实需要用户消息支持，工具观察限于对应workspace。用户输入、成功、失败及unknown状态分别表达证据等级，结果或excerpt截断继续保留缺口。模型正文和压缩摘要不能自行制造用户授权、已执行结果或来源身份。

默认查询页为16KiB、16项，可请求最多64项，查询词最多1024字节。自动投影最多16KiB，其中路由预算4KiB、选中正文预算8KiB，路由和选中事实各最多16项。工具参数最多64KiB，每次更新最多32项事实，每项最多8个来源selector，总选择最多32项。独立核查默认最多32KiB，excerpt默认最多2048字节，省略状态进入核查数据。

## 授权、审阅与一致性

auto变更先经过当前规则，再进行独立模型核查，必要时由人工确认。Manual Approval覆盖每次模型工具调用，包括读取和证据目录。PLAN开放只读能力，更新与遗忘硬拒绝。bypass仍服从PLAN、显式deny及宿主受保护资源边界。

宿主在 `__saya_managed.review` 中提供当前用户来源、选中证据、scope、前后差异、版本与截断信息。审核器按证据等级判断语义和授权，`goal_summary`保留调用方任务摘要的身份。更新绑定完整目标集合、当前模式／策略、Session／Run／call、来源hash及预期revision，在实际存取前再次核对。

`GlobalMemoryStore::propose`为纯变更计算。`commit_cancellable`持排他锁，核对索引、凭据、根身份与预期版本后发布。host_request_digest进入receipt与WAL，旧请求在后续更正／遗忘后重试时返回原操作记录及当前revision。执行前后Session记录和MemoryStore发布各自保留实际状态，unknown结果交由宿主恢复，调度器停止受影响的后续执行。

WAL意图、私有stage、原子索引替换与独立ledger共同维持提交状态。锁等待上限200ms，取消前置检查保持零创建。会话恢复使用本次配置、权限和当前记忆版本。

## 更正、遗忘与实际保留范围

明确遗忘对目标、alias、词法重复及派生事实记录墓碑。来源撤回按支持关系移除受影响内容，保留仍有独立支持的事实。活动投影抑制受影响的旧entry标题、updated_at、主题链接以及source的description、reference和observed_at，来源元数据使用withheld标记，稳定身份与版本继续用于审计。

当前ledger约束旧索引回放、历史rollback和备份restore。被遗忘内容不能借这些入口重新进入活动视图。删除主题链接后，主题原件沿其拥有者的生命周期保留。历史索引版本、原始Session证据及既有导出也分别保留，遗忘结果和界面展示这一范围。

当前记忆／遗忘版本参与ContextView和摘要兼容性。宿主记录请求实际使用的Memory来源与返回历史hash，对失效的记忆调用组及受旧记忆影响的模型输出实施完整协议组替换。下一次请求和Compact输入使用经宿主验证的当前投影，原始历史继续用于持久审计与用户导出。[上下文设计](context-management.md)

## CLI管理

`saya memory`提供settings、list、version、show、search、diff、review、apply、forget、history、rollback、export、restore、operation和recover。使用 `saya memory --help` 与具体子命令帮助查看完整参数。

显式CLI持久变更由write独立控制，read关闭时仍可应用已提供版本的apply、forget、rollback、restore或recover。settings、version、operation与history仅返回配置、版本或操作元数据；内容列表、查询、完整差异和导出需要read启用。变更使用完整expected_revision和稳定operation_id。`diff`／`review`读取显式MemoryChange JSON，展示有界分页差异，`apply`应用对应的明确用户操作。forget按声明scope和fact ID处理。rollback选择历史索引版本，restore校验活动视图备份、来源、遗忘约束和当前主题引用。PLAN拒绝持久管理写入，`write = false`拒绝记忆变更。

备份覆盖当前global及workspace活动视图、来源元数据、当前遗忘约束和主题引用。主题正文、历史版本和原始Session证据沿各自存储保留。导出采用私有权限、完整字节校验和排他创建。未知发布可使用显式 `saya memory recover` 收敛已登记WAL，输入操作保持原身份。信号取消置入同一worker标志，宿主等待实际磁盘操作完成并报告已观察结果，unknown保留结构化receipt。

## TUI管理

`/memory`打开独立弹窗，保留当前消息草稿。弹窗使用自己的命令输入，PgUp／PgDn或滚轮只滚动弹窗内容，Esc关闭。

- `list [PAGE]`、`search QUERY`、`show ENTRY_ID|FACT_ID`、`sources ID`和`next`展示当前条目、来源、scope及版本。列表每页12项，读取／检索沿原生cursor继续。
- `correct FACT_ID TEXT`、`forget FACT_ID`、`diff MEMORY_CHANGE_JSON`、`rollback INDEX_VERSION`和`restore PATH`先准备只读提案，展示完整变更事实、entry／source元数据及资源链接差异。单次完整review上限64KiB，超限需要缩小操作或使用CLI分页审阅。
- `review`重新显示当前提案。`confirm OP_ID FULL_REVISION`明确批准当前不可替换的提案，`reject`丢弃提案。提交前重新核对本次配置、scope、版本、PLAN及write开关。
- `export PATH`先捕获备份字节、真实父目录身份与目录句柄，确认后在同一目录句柄内原子发布。审阅期间的路径换向、目录替换及新凭据会使操作失败。
- `history [PAGE]`、`operation OP_ID`查看保留版本和操作记录。unknown结果保留receipt与最后确认的operation ID，并提示显式CLI恢复。
- `set read|inject|write on|off`在空闲时调整当前进程设置。`help`展示命令，`close`返回会话。

运行中允许管理查询，变更准备、发布和开关修改要求空闲。TUI完整差异包含before正文，因此准备与确认要求read启用，关闭read会丢弃已有提案并隐藏其正文。管理属于用户明确发起的界面操作，模型工具沿运行时的权限与人工审批契约执行。取消后等待实际磁盘worker结束，已经发布的结果继续展示。

## 实施与验收

DEV-084至DEV-089覆盖存储、读取投影、模型写入、版本与遗忘、管理界面及跨会话联合矩阵。组件阶段已记录33项存储回归与严格Clippy证据，见 `target/n3/memory-component.log` 和 `target/n3/n3-components-clippy.log`；运行入口、证据目录、Compact、UI和四平台结果见[N3最终验收](n3-validation.md#四平台最终验收)。

验收需要覆盖外部编辑、并发冲突、重试幂等、WAL故障、符号链接、凭据轮换、来源伪造、跨workspace、旧历史／摘要重注入及三协议正常入口。源码集成结果与原生四平台证据分别记录。[任务分解](post-mvp-plan.md)

## DEV-085 有界查询组件

2026-11-10，查询与投影组件提供UTF-8原文分页、revision／filter绑定游标、当前workspace过滤、来源与alias检索、有限路由和事实预算。缺失根保持零创建，外部编辑、更正及遗忘立即改变后续投影身份。13项新增查询／投影回归与33项既有存储回归通过，全workspace全目标严格Clippy通过，日志分别为 `target/n3/integration-core-tests-3.log` 与 `target/n3/integration-clippy-3.log`。运行入口与最终平台矩阵见[N3最终验收](n3-validation.md#四平台最终验收)。

## DEV-087 活动视图备份与恢复

2026-11-10，新增带workspace、内容校验和、当前遗忘约束及主题引用的备份格式，恢复沿既有提案、锁、WAL、幂等receipt与原子发布链路执行。导入保留其他workspace内容和当前墓碑，旧备份无法恢复已遗忘事实，缺失或变化的主题拒绝发布。3项新增回归与既有查询／存储共49项通过，全workspace全目标严格Clippy通过。CLI／TUI管理、正常入口及四平台证据见[N3最终验收](n3-validation.md#四平台最终验收)。

## DEV-086 受控记忆工具组件

专用memory_read／search／evidence／update／forget使用当前宿主根、workspace、Session原始来源与稳定版本。准备阶段生成绑定完整历史、证据和差异的不可复制操作，执行沿MemoryGate两次授权并核对实际Rule／Model Review／User来源。证据目录默认最新32项，append-only历史游标保持原始ordinal，已有记忆工具输出不能充当独立事实来源。20项组件回归及严格全目标Clippy通过，正常运行权限与遗忘联合证据继续按N3记录维护。
