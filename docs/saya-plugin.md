# saya-plugin v1 规范草案

**更新于2026-11-09。系统名称沿用D-031，本文细化F-03／DEV-065至DEV-067、DEV-078。manifest v1表示插件格式版本，DEV-078验证组件已完成本机验收，实际支持情况由宿主能力声明与验收结果决定。**

## 规范文件

- 包根入口为 `saya-plugin.toml`，采用 TOML。
- 结构约束见 [JSON Schema](schemas/saya-plugin-v1.schema.json)，TOML 解析成数据对象后按该 schema 校验。
- 可复制的结构示例见 [saya-plugin.example.toml](../examples/saya-plugin.example.toml)，示例中的兼容范围只演示语法。
- 路径、SemVer 范围、唯一 ID、能力组合、文件类型、来源与授权还需运行时语义验证。

## 包与扩展点

```text
plugin-root/
  saya-plugin.toml
  LICENSE
  skills/rust-review/SKILL.md
  resources/review-guide.md
  mcp/server.py
  hooks/record_summary.py
```

v1 定义声明式 Skills、资源、MCP 服务与受控 Hook 入口。执行组件经批准的解释器／运行环境启动，宿主内部扩展由稳定的工具与应用接口承接。浏览器通过宿主已启用的托管能力使用。

| 字段 | 约束 |
| --- | --- |
| `schema_version` | 整数 1，未知版本返回不兼容错误 |
| `plugin` | ID、SemVer 版本、描述、许可证和可选许可证文件／主页 |
| `compatibility` | 宿主版本范围及 `requires_features`，必须包含 `plugin-manifest-v1` |
| `capabilities` | 申请能力、网络 origins 和 credential slots，形成包的能力上界 |
| `skills` | entry ID 和包含 SKILL.md 的相对目录 |
| `resources` | entry ID、相对文件及媒体类型 |
| `mcp` | entry ID、transport、program／args 或 HTTPS endpoint、凭据槽 |
| `hooks` | entry ID、事件、种类、program／args、超时 |
| `dependencies` | 依赖包 ID 与版本范围，实际版本和摘要由 lock 固定 |

包至少包含一个扩展入口。同类 entry ID 和 dependency ID 唯一，顶层及对象中的未知字段拒绝。包版本使用 SemVer 2.0.0，范围按实现时固定的 Rust semver 语法验证。[SemVer](https://semver.org/)、[Cargo 版本范围](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html#version-requirement-syntax)

## 命名与兼容

包 ID 使用小写命名空间，示例为 `io.saya.examples.rust-workflow`。entry ID 使用小写字母、数字与连字符。内部身份采用 `plugin_id/entry_id`，模型侧名称由 Tool Registry 稳定映射并校验协议长度／字符限制。

宿主版本匹配与 feature 集匹配都通过后才能启用。Skill、stdio MCP、HTTP MCP、Hooks 和托管浏览器分别声明对应 feature。未满足条件时显示缺失项。模型工具别名绑定完整 ID、包摘要和工具版本，碰撞作为注册错误处理。

| 入口 | 必需feature | 对应能力检查 |
| --- | --- | --- |
| 任意包／独立资源 | `plugin-manifest-v1` | 结构、内容身份与宿主版本 |
| Skill | `skill-directory-v1` | Skill路径、来源及按需加载 |
| stdio MCP | `mcp-stdio-v1` | `process.exec`与`mcp.stdio` |
| HTTPS MCP | `mcp-http-v1` | `network.outbound`、`mcp.http`、已声明的精确origin及凭据槽 |
| Hook | `hook-events-v1` | `process.exec`及`hook.observe`／`hook.check` |
| 托管Browser | `browser-managed-v1` | 对应Browser能力，宿主实现后才声明支持 |

同 ID／版本的不同摘要记录为来源冲突，启用时明确选择内容身份。包 ID 与发布者文本为声明信息，信任决定依据实际来源、摘要和用户选择。

现有用户级／工作区 Skill 同名优先级按 D-033 处理。插件入口默认使用限定名，用户显式定义的别名记录来源及覆盖范围。

## 权限与配置

有效权限由包申请、显式启用范围、当前用户策略、工作模式和后端能力的交集确定，显式 deny 优先。

| 能力类别 | 示例申请 | 生效检查 |
| --- | --- | --- |
| 工作区 | `workspace.read`、`workspace.write` | 受控视图、路径与写回规则 |
| 执行 | `process.exec`、`mcp.stdio` | 已批准运行环境、argv、AST 风险和隔离 |
| 网络／远程 MCP | `network.outbound`、`mcp.http` | 精确 origin、实际出口和连接授权 |
| 凭据 | `credential.use` 与命名槽 | 宿主配置的用途绑定和撤销状态 |
| Hooks | `hook.observe`、`hook.check` | 事件、操作许可、超时及执行预算 |
| 浏览器／产物 | `browser.observe`、`browser.interact`、`artifact.read`、`artifact.write` | 已启用宿主能力、当前资源 ACL 与操作策略 |

网络 origin 采用规范化 HTTPS origin，endpoint 的路径与凭据用途另行检查。凭据槽为符号名称，实际 secret 引用由用户在选定配置中绑定。literal secret、任意宿主环境继承和插件指定的宿主绝对挂载分别由语义验证及执行策略限制。

项目 `.saya` 可承载待审阅声明，运行配置仍遵循 B-04 的显式选择。安装与启用通过用户操作记录，工作区文件变化产生可审阅差异。启用记录绑定包摘要和作用域，当前每次工具操作继续按本次权限判断。

Skill 文本、资源和服务输出以低信任来源数据传递，执行组件的能力交集在后端落实。模型工具调用继续由宿主派发，并通过来源注入夹具评估行为。

## 路径、存储与摘要

路径使用规范化 UTF-8 相对路径，组件采用 `/` 分隔。拒绝绝对路径、盘符、反斜杠、点路径组件、NUL／控制字符、链接、设备及特殊文件。相同规范化或大小写折叠路径冲突时拒绝安装。

碰撞键按Unicode NFC、完整大小写折叠、再次NFC的顺序生成。声明引用与封存文件名按实际UTF-8字节精确匹配，树摘要使用这些实际路径字节。验证器以目录FD逐组件打开，拒绝链接、特殊文件和硬链接，文件读取前后核对身份及内容长度。返回值持有实际读取的封存字节，安装器消费该字节快照。

包内容安装到XDG数据目录的私有内容寻址存储，按digest固定，按只读策略提供给原生worker。验证器上限为4,096个普通文件、8,192个总路径项（包含隐式目录）、展开总量64MiB、单文件16MiB、目录深度16、UTF-8相对路径1,024字节及manifest 256KiB。归档提取器在创建条目与展开内容前执行同一增量FileBudget，归档格式与安装接入由DEV-066交付。

树摘要算法为 `saya-tree-sha256-v1`。

1. 收集包括 manifest 的全部普通文件，使用规范化相对路径，按 UTF-8 字节排序。
2. 以 ASCII `saya-plugin-tree-v1` 及零字节作为摘要域。
3. 每项加入路径长度的 u32 big-endian、路径字节、文件长度的 u64 big-endian 和文件内容 SHA-256 的 32 个原始字节。
4. 得到整个树的 SHA-256，文件时间及宿主权限元数据分别由安装策略处理。

lock 记录包 ID、版本、来源、安全的获取引用、树摘要、依赖摘要和所需能力指纹。lock 位于包内容树外，源码身份和启用记录同时绑定摘要。启用与实际加载前验证内容身份，数据损坏进入失败状态。

lock采用TOML v1，顶层字段为 `schema_version`、`digest_algorithm`、`packages`、`aliases`。每个包记录ID、版本、来源、digest、能力指纹及精确依赖版本／digest。来源kind为 `local-directory`、`archive` 或 `https`，每个ID选择一个版本。缺失依赖、范围不匹配、依赖环、同ID／版本的内容冲突和别名碰撞均返回错误。别名以字母开头，最多64个ASCII字母、数字、下划线或连字符，并绑定完整内容身份。

## 启动与生命周期

```text
validate -> install -> inspect -> enable
  -> 注册元数据与能力 -> 按需启动 -> 运行租约
  -> disable -> 取消与清理 -> 释放租约 -> uninstall / GC
```

安装先完成解析、结构／语义校验、文件清单与摘要。依赖从已显式取得的包中解析，缺失或循环依赖返回错误。安装阶段只处理包内容与登记。

Skills 按激活请求加载正文。MCP 服务在需要时按 package／entry／Run 身份启动，调用与结果进入统一记录。更新先保存新版本，运行中的实例持有原版本租约，后续 Run 使用选定的新版本。停用立即关闭新调度，并取消、清理已拥有的执行资源。

全局安装状态、作用域启用状态和运行实例健康状态分别保存。cleanup pending／unknown 有独立展示，卸载在租约及资源处理后回收对象。工作区产物通过自身生命周期管理。

## 可执行入口和 Hooks

`program` 为宿主批准的运行环境名称。`args` 为直接 argv，`@plugin/` 路径前缀解析到当前已验证包的只读访问位置。替换使用路径值，解释器和 shell 入口经过命令分析与当前策略。

MCP transport 首版覆盖 stdio，远程 HTTPS 需通过 `mcp.http`、网络及相应 feature 检查。服务器扩展请求按协商能力、请求预算和宿主策略处理。

Hook 事件草案为 `run.started`、`run.finished`、`tool.before_dispatch`、`tool.finished`。observer 返回受限 annotations，check 只在 `tool.before_dispatch` 使用，返回 `pass`、`deny` 或 `abstain`。检查结果与其他权限条件共同决定派发。

Hook 进程作为独立操作保存意图、结果与 parent call，使用当前模式／权限。Manual Approval 产生相应操作确认。人工等待按运行预算规则计时，实际执行使用独立超时。拒绝、格式错误或 check 超时阻止原操作，observer 失败记录诊断。递归 Hook 触发由宿主深度与事件身份约束。

## 本地管理命令

```sh
saya plugin validate ./plugin-root
saya plugin install --path ./plugin-root
saya plugin list
saya plugin inspect io.saya.examples.rust-workflow
saya plugin enable io.saya.examples.rust-workflow --digest <tree-sha256> --scope workspace
saya plugin disable io.saya.examples.rust-workflow
saya plugin update io.saya.examples.rust-workflow --path ./updated-plugin --expected-digest <tree-sha256>
saya plugin uninstall io.saya.examples.rust-workflow --digest <tree-sha256>
saya plugin recover
```

CLI／TUI 展示版本、来源、树摘要、依赖、申请能力、有效范围和实例状态。升级展示内容与能力差异，新的摘要及能力范围重新记录启用决定。

## 验收与实现状态

DEV-078 实现结构及语义验证、内容摘要、lock 和命名规则，DEV-065／066／067 接入激活、生命周期和执行入口。测试覆盖未知字段、版本、缺失 feature、恶意路径、归档膨胀、摘要损坏、依赖环、并发更新／停用、过期批准、Hook 递归和清理失败。

DEV-078已交付独立Rust验证组件 `plugin_manifest`，包含manifest／lock解析、schema与语义、宿主兼容、密封文件内容、树摘要、依赖和工具别名检查。13项定向回归覆盖Unicode规范等价路径、实际请求装配器的别名契约及文件边界。新增unicode-normalization用于路径碰撞键的NFC处理，semver与unicase复用已有锁定版本。

本机执行 `RUST_TEST_THREADS=1 just check`，格式、全workspace全目标严格Clippy、942项Rust测试及优化构建通过。独立源码复核发现的Unicode碰撞与别名首字符问题已修复并复核，日志见 `target/n3/dev078-workspace.log`。安装器、运行时加载、权限接入和命令由DEV-065／066／067继续实现。

2026-11-10，新增已打开目录句柄的检查入口，私有安装树复验沿同一cap-std能力读取。路径被替换的回归验证原目录内容与替代目录分别保持独立身份。该增量随本机完整门禁986项Rust测试通过，记录见 `target/n3/dev076-062-workspace.log`。


## DEV-066 本地生命周期组件

`plugin_store` 已实现内容寻址只读对象、严格索引及lock、工作区／用户启用覆盖、固定依赖闭包、明确摘要授权、更新、停用、卸载和恢复。安装后保持未启用，新的内容摘要需要明确启用。工作区停用覆盖用户启用，依赖显式停用撤销相应闭包，更新保留既有Run持有的版本。

租约持久化并用文件锁标记所有权。完成资源清理后才能释放租约，进程退出或清理未知时保留unknown及对象pin。对象／索引文件经目录句柄校验，单次Store上限为128对象、512MiB与1024租约。同步文件操作由入口放入阻塞工作线程。

CLI提供上述本地管理命令，列表每页最多20条，使用显式配置的data目录下plugins目录。启用要求完整树摘要与作用域，结果区分启用状态、版本和清理状态。PLAN拒绝管理写操作。本阶段以暂存源码的独立副本执行格式检查、全workspace全目标严格Clippy、组件15项及CLI 6项回归，全部通过，日志为 `target/n3/dev066-stage-validation.log`，前序定向日志为 `target/n3/plugin-store-component.log` 和 `target/n3/plugin-cli-tests.log`。正常Run中的Skill／MCP服务组装及最终平台证据继续在[N3验收](n3-validation.md)维护。


## DEV-067 Hook 执行与日志组件

`hooks` 已实现四个事件的独立操作接口、当前宿主执行回调、observer／check返回契约、调用预算、递归约束、超时与取消清理。check只在工具派发前使用，pass／abstain继续核对原操作权限，deny、格式错误与超时阻止原派发。执行计时从宿主确认Started开始，人工等待由共享预算暂停／恢复处理。

`hook_store` 使用独立私有SQLite和生命周期所有者文件锁，原子认领事件与每Run调用限额，重复事件复用同一结果。重启把未完成Running转Unknown，保留原意图并等待明确处理。执行结果先完成资源清理再保存，结果保存失败仍通过BoundaryError保留脱敏后的可观察记录。

本阶段暂存源码使用独立产物目录执行格式与全workspace全目标严格Clippy，12项Hook执行及12项SQLite日志测试均通过，日志为 `target/n3/dev067-stage-validation.log`。覆盖真实子进程在意图提交后退出、跨重启预算、并发认领、重复结果、凭据变化、超时和清理未知。真实Run事件、插件原生执行器与四平台验收按[N3记录](n3-validation.md)继续集成。

## DEV-065 Skill激活组件

2026-11-10，SkillManager复用项目发现的同名覆盖与根身份，提供有界分页目录、显式正文激活／停用及UTF-8资源分页。请求绑定catalog来源摘要与实际文件版本，变化的正文退出活动投影，新来源需要宿主刷新后再次选择。插件Skill使用限定名、已密封包内容与当前Run租约，正文和资源不授予执行权限。13项组件回归及全workspace全目标严格Clippy通过，覆盖替换、链接、秘密轮换、游标和租约撤回。正常工具调用与最终平台证据继续按[N3验收](n3-validation.md)维护。

## DEV-067 批次边界修复

独立复核后，每个Hook条目在开始前重新取得当前凭据集合并重建脱敏payload及其绑定摘要。前一条Hook引入的新凭据不会沿批次快照泄漏到后续条目。遇到既有Running／Unknown认领时立即停止后续条目，保留cleanup_pending与原操作身份。14项执行回归及全workspace全目标严格Clippy通过，真实原生Run结果继续按N3记录维护。
