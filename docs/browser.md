# Browser 能力设计

**更新于 2026-11-14。浏览器能力归 F-10，DEV-079 至 DEV-082 负责接口、worker、策略与界面集成，DEV-083 负责联合验收。DEV-079 已实现宿主接口、受控网页读取与离线测试。DEV-080 的组件已完成 Playwright MCP 启动、身份校验、固定工具映射与生命周期，并按 D-066 增加显式专用 VM 后端；该后端的真实入口已在专用 Colima VM 上实际执行并通过（renderer 的 NoNewPrivs／Seccomp 与合法 sandbox argv、远端 Run 根和进程清理）。native worker 的组件测试通过，但 macOS Seatbelt 与 Linux Bubblewrap 当前都不能完成 Chromium 内层 sandbox 初始化，因此 native 浏览器未取得真实平台验收，也不得视为可用。DEV-081A 的 Browser 网络阶段已完成并取得专用 VM 真实网络验收；DEV-081 的登录凭据槽、文件 broker、后台取消与 unknown 提交阶段也已完成并取得专用 VM 真实验收（表单凭据填充、实际下载／上传、错误 origin、后台取消、Run 清理与引用恢复）。DEV-082 已实现截图宿主／产物／模型图片注入与 CLI／TUI 展示，并在本机离线与 PTY 验证；pinned `@playwright/mcp 0.0.85` 的 `browser_take_screenshot` schema 没有 `mask` 参数，无法在截图生成前执行真实遮蔽，因此 worker／VM 的 `screenshots` 与 `screenshot_masking` fact 均为 `false`，生产截图返回 `browser_capability_disabled`，不返回未遮蔽图片；只有具备遮蔽能力的 provider 才注册 `browser_screenshot`。DEV-083 的本机／专用 VM 联合阶段已完成 21 passed、1 disabled，三协议 CLI／TUI 回归通过；生产截图、native Browser 与新增四平台固定源码证据仍未满足，N5 全范围尚未完成。**

## DEV-079 实现状态

DEV-079 只交付宿主侧组件，不包含真实 Chromium 或 Playwright worker，也不声称浏览器会话已经可用。

- `saya-core::browser` 提供 `BrowserProvider` 与 `BrowserState`。每个 Run 拥有私有会话；同一页面动作串行；页面、文档和元素引用使用随机不透明 token，宿主保留元素映射。`snapshot` 创建新文档并使旧引用失效；`navigate` 是建立新文档的动作，只返回 `page`／`url`（旧 document 引用因此失效，不要求 document token）；`click`、`fill`、`press`、`upload` 要求显式当前 `document` token 并核验，`wait` 后保守作废文档；这些动作结果返回 `document_revision`。未知结果绑定到发生动作的具体页面：只有该页面自身的新 snapshot 或完整 close 才能恢复；其他页面 snapshot 或新 open 不会清除它。`open` 自身 unknown 使用独立全局保守状态阻断所有旧页面，只有完整 close／shutdown 可清。`open`／`close`／`navigate`／`click`／`fill`／`press`／`upload`／`download` 的已派发取消按 `outcome_unknown` 处理并持久化为 unknown；无副作用的 `wait` 取消为 `cancelled`。入口即已取消则返回 `cancelled`。`browser_close` 在 provider 确认前不丢弃 session／pages，已知失败可安全重试；shutdown 幂等但失败可重试，且完整 shutdown 后的 open 失败或取消不会复用旧的干净状态。`FixtureProvider` 提供无网络的可控状态、并发、取消、unknown、close 失败、provider 截断与慢 shutdown 测试。
- `saya-core::web` 提供受控 `fetch_url` 与仅在显式 endpoint 配置时注册的 `web_search`。仅允许 HTTP／HTTPS GET／HEAD，拒绝 userinfo、fragment、特殊 scheme 和系统 proxy；每次重定向重新执行 origin、公开地址与完整 DNS 答案检查，并把连接固定到已核查 IP。DNS 解析与响应体读取同时受 timeout 与取消约束；响应大小、内容类型、UTF-8、超时和取消均有边界。字节截断落在 UTF-8 多字节中间时保留有效前缀并标记 truncated，真正损坏的输入仍返回 `invalid_utf8`。HTML 正文通过已锁定的 `lol_html 3.0.3` 做有界流式提取，除 `RawText`／`ScriptData` 外还以 element handler 移除任意命名空间的 script／style／noscript／template（覆盖 SVG／MathML），并在相邻文本块之间保留空白分隔。所有网页内容标记为 `untrusted_web`。
- 模型工具面固定为 `fetch_url`、`web_search` 及 `browser_open`／`close`／`navigate`／`snapshot`／`click`／`fill`／`press`／`wait`／`screenshot`／`download`／`upload`。不注册 selector、任意脚本、evaluate、install、devtools 或原始 MCP 工具。截图、下载和上传仅在 provider facts 声明支持时注册；DEV-081 的下载／上传保存有界 `File` artifact 并返回引用；DEV-082 后 worker／VM 的 `screenshots` 与 `screenshot_masking` 仍为 `false`，因为 pinned `browser_take_screenshot` 没有 `mask` 参数；只有同时声明两项能力且完成真实遮蔽的 provider 才注册 `browser_screenshot`。`browser_file_upload` 只由宿主文件 broker 在 Run 私有 staging 路径上调用，不注册为模型工具。provider 返回的 URL／title／detail／filename／mime 等来源字段有明确长度与控制字符上限，非法值返回 `invalid_provider_result`，不截断后冒充最终来源。download／upload 结果同样带 `source = untrusted_browser`。
- registry 新增 `WebRead`、`BrowserRead`、`BrowserWrite`。`browser_open`／`close`／`navigate` 为 `BrowserWrite`；`browser_snapshot`／`wait`／`screenshot` 为 `BrowserRead`；`click`／`fill`／`press`／`download`／`upload` 为 `BrowserWrite`。PLAN 只开放 `WebRead`，两类 Browser 效果均拒绝；已保存观察继续通过 `output_read`／`output_search` 检索。`auto` 允许 `WebRead`／`BrowserRead`，`BrowserWrite` 走现有独立模型核查；Manual Approval 逐次确认；bypass 仍受 PLAN 与显式 deny 限制。只读运行时既不注册也不执行 Browser 工具。宿主 built-in adapter 通过有界 schema 校验进入既有注册、审批指纹、捕获与持久化路径。
- OutputStore 新增 `Web`／`Browser` 流。长正文和页面快照经 `capture → save_result_with_outputs → publish` 保存，保留 `output_ref`、coverage 与 truncated 信息。快照捕获包含宿主投影后的 text／elements／document／source 等字段，捕获上限使用 `BrowserSettings.max_snapshot_bytes`；provider、宿主提取、inline 或捕获任一不完整均以 `CaptureCompletion::LimitReached` 记录，`truncated` 一致反映到 `data`、`ToolResult` 与 output metadata。检索到对象但 `retained_bytes = 0`、withheld 或 capture disabled 时，`fetch_url` 与 `browser_snapshot` 回退有界 inline 内容并标记 truncated，不返回空成功。`web_search` 的长结果投影只保留 `result_count` 等元数据，正文经 `output_read`／`output_search` 回取。
- `AgentRuntime::with_web`／`with_browser` 注册入口已存在。`RuntimeServices` 持有浏览器状态，并在既有 PLAN 清理与 Run shutdown 中先于 environment 与 broker 清理以有界超时调用 provider shutdown；browser 清理失败不会跳过 MCP、插件、environment、broker 与目录清理，首个错误在末尾汇总。清理先触发生命周期取消以唤醒在途 `wait`／navigate，再获取页面锁并关闭 provider；清理保留可重启状态，返回 WORK 后显式 `browser_open` 会创建全新 provider 生命周期并重新绑定同一 Run 的存活后台 supervisor。`WebSettings`／`BrowserSettings` 默认关闭，配置加载复用 broker policy 执行严格校验；搜索 endpoint 必须严格规范化 HTTP／HTTPS，并拒绝字面私网／回环地址（显式 localhost 开发 origin 除外）；开发 origin 必须是回环地址，回环 allowlist 条目必须同时指定为开发 origin。
- 生产装配：`saya-app` 接入显式启用的 web；`browser.enabled` 在 `browser.worker.enabled` 时装配 DEV-080 的 native provider，在 `browser.vm.enabled` 时装配显式专用 VM provider（DEV-080 扩展与 DEV-081A 网络）。未启用 worker、Host backend、macOS Seatbelt 或缺少 VM 网络策略均在 worker／Chromium 启动前明确失败。真实 native Linux Chromium sandbox 证据仍待取得；登录、文件 broker 与后台生命周期已由 DEV-081 完成并取得专用 VM 真实验收，模型图片与跨平台验收仍属后续任务。

## DEV-080 worker 与生产装配实现状态

本阶段交付 `saya-core::browser::worker` 的 Playwright MCP provider、原生浏览器环境组合与 `saya-app` 生产装配；通用 Linux／Seatbelt profile 保持不变，macOS 明确不提供浏览器 sandbox。

- 固定 `@playwright/mcp 0.0.85`、`playwright`／`playwright-core 1.66.0-alpha-2026-11-09`、Chromium `158.0.8127.18`（revision 1254）。宿主显式提供绝对 Node、worker root、Chromium executable、身份 manifest 与 manifest SHA-256；不调用 `npx`／`npm`，不自动安装或下载。
- 启动前以 `O_NOFOLLOW` 和锁定目录句柄校验 manifest 以及 manifest 明确命名的文件：Node、`@playwright/mcp/cli.js`、三个 `package.json`、`playwright-core/browsers.json` 和 Chromium 主程序。校验常规文件、权限、大小、SHA-256 与 pinned package／Chromium 精确版本；Node 版本由 manifest 声明并与其摘要绑定，本阶段不执行 Node 版本探测；manifest 自身也要求摘要匹配。该保证只覆盖这些命名文件，不声称验证整个 worker 树。
- 真实启动通过 `PreparedEnvironment::prepare_command` 与 `ManagedProcess`，stdin／stdout 交给 `McpClient::connect_stdio`，stderr 以固定上限持续排空并保留有界尾部用于 sandbox 证据。宿主生成的固定配置与 argv 使用 `chromiumSandbox: true`、`--sandbox`、headless、isolated、block service workers、no WebMCP、ignore roots、`imageResponses: omit`、固定 output dir、固定 Chromium executable；不设置固定远程调试端口，也不把 `worker.proxy` 注入 launch options 或 `--proxy-server`。worker 自启的 Chromium 与 worker 同属宿主监督的进程组。
- 初始化协商版本固定为 `2025-11-25`。连接后读取目录并校验七个模型映射工具及其 pinned input schema：`browser_navigate`、`browser_snapshot`、`browser_click`、`browser_type`、`browser_press_key`、`browser_wait_for`、`browser_close`；同时要求宿主内部文件上传工具 `browser_file_upload` 存在且参数为 `paths: string[]`，它只接受宿主 staging 路径，不进入模型注册。Evaluate、run-code、install、devtools、network、console、tabs 等目录项不进入模型注册；目录 generation 变化使旧调用返回 stale，不自动重试。
- worker 文本结果解析为 `BrowserAction`／`BrowserSnapshot`：Page URL／Title、YAML snapshot 正文、`role`／`name`／`[ref=...]` 映射为 provider element keys；来源字段继续经过 `BrowserState` 既有长度、控制字符与 URL 校验。snapshot 使用有界行解析器：只从行尾 props 区读取 `ref`，accessible name 中的 `[ref=...]` 文本、引号／转义与 YAML 单引号 key 都不会被当成元素身份；每个列表项都校验 prop 结构与 bracketing。元素引用只接受 pinned provider 的 `e<digits>`／`f<digits>e<digits>` 形式；frame／容器／元素 ref 非法、重复或无法识别时整份 provider snapshot 以 `invalid_provider_result` 失败，不静默丢弃子树。frame 归属仍由 snapshot 树结构决定，iframe 节点自身和全部后代都带 frame 身份，供凭据填充拒绝子 frame。`open` 通过 worker 创建宿主内部 `about:blank`，模型侧仍是现有单页 `BrowserState` 接口。
- MCP 已派发后的 timeout、断连、进程崩溃或未知结果映射为 `BrowserFailure::uncertain`／`outcome_unknown`，绝不自动重放；目录缺失、schema 漂移、协议不符、argv／身份校验失败等派发前失败仅在资源清理已确认时保持已知错误。launcher 返回连接后、首个 catalogue await 之前即注册 connection／workspace 并阻止复用，隔离持续覆盖初始导航与后续 catalogue 刷新，直到 sandbox 核验完成才允许复用；启动拒绝、外层丢弃核验／清理 future 或清理失败均保留重试句柄。初始导航后的 catalogue 重同步失败若无法确认清理，同样返回 unknown，不因尚未建立 provider session 而吞掉残留资源状态。
- `BrowserFacts` 声明 downloads／uploads 为 true（文件 broker 已完成）；screenshots／screenshot_masking 在 pinned worker 为 false，模型图片注入由 DEV-082 按 profile 能力接入。`provider.shutdown` 依次关闭 MCP、终止并 reap worker 进程组、删除临时 config／output／profile／work 目录；失败保留可重试状态并明确 unknown。
- 配置新增严格 `[browser.worker]`，默认关闭；启用时必须显式提供全部身份与路径输入。离线测试使用可注入 launcher／connection／environment lease／sandbox inspector，覆盖身份错误、固定 argv、目录／schema 校验、工具映射、取消／断连 unknown 与 shutdown 重试，不访问网络或真实浏览器。
- 生产装配由 `saya-app::assemble_browser_provider` 完成：`browser.enabled` 必须同时启用 `browser.worker`，否则返回 `invalid_config`；安装根固定包含 Node 父目录、worker root 与 Chromium 包目录，并保持只读。不绑定 workspace、HOME、`.saya` 或凭据目录。`Host` backend 与 macOS Seatbelt 在构造 provider 前通过 `NativeBrowserEnvironmentFactory` 预检返回 `browser_sandbox_unavailable`，不会启动 worker 或 Chromium；Linux 缺少所需嵌套 namespace／procfs／seccomp 能力时保持既有 fail-closed 错误。
- 每个 worker 生命周期都从 factory 取得全新 `PreparedEnvironment`；profile／output／tmp／work／config 全部位于该 environment 的 Run 私有 scratch。PLAN 清理或显式 shutdown 后再次 `browser_open` 会新建 environment，不复用已关闭的实例。shutdown 先关闭 MCP、终止并 reap worker／Chromium 进程组，再 `wait_for_process_cleanup` 与 `environment.cleanup`；任一步失败保留可重试状态并返回 unknown，下一次启动前必须先完成旧 workspace 清理。
- 真实 Chromium sandbox 证据：首次真实 `open` 以及每次经统一页面通道成功返回的 MCP 调用后均重新采样，不以已有证据跳过核验。provider 以 worker PID 为根读取宿主 `/proc`，只接受具有匹配 PPid 的后代，在 512 进程与 5 秒上限内等待至少一个 `--type=renderer`。该 renderer 的 argv 不得包含 `--no-sandbox` 或 `--disable-setuid-sandbox`，`/proc/<pid>/status` 必须报告 `NoNewPrivs: 1` 与 `Seccomp: 2`。由于宿主 Bubblewrap 会固定赋予这两个属性，它们不能单独证明 Chromium 初始化了自己的 sandbox；native inspector 还要求 Chromium 自有的 `InitializeSandbox` stderr 证据，否则返回 `browser_sandbox_unavailable` 并清理 worker 与 environment。`sandbox_evidence()` 仅表示最近一次完成的采样，不是持续或全部 renderer 的瞬时证明；close 和 shutdown 清除证据。核验开始即隔离生命周期，核验失败、取消或 future 被外层 timeout 丢弃后，不得继续派发普通页面操作；显式 close／shutdown 确认清理后才能重建，清理不能确认则返回 unknown 并继续保留句柄。上述条件未满足时，native 浏览器不会被报告为已验收。macOS 因外层 Seatbelt 限制不会到达该步骤。
- 本阶段浏览器 environment 在 DEV-080 固定为 `NetworkPolicy::Deny`；DEV-081A 已接入专用 VM broker。`[browser.worker].proxy` 仅校验为无凭据、显式端口的 loopback HTTP 地址，不直接参与启动。当前 downloads／uploads facts 为 true；screenshots 与 screenshot_masking 为 false（pinned worker 无遮蔽输入）。

身份 manifest 由宿主生成，文件名可自选，摘要写入 `identity_sha256`。结构为：

```json
{
  "schema_version": 1,
  "node": {"sha256": "<64-hex>", "size": 112928848, "version": "v22.23.0"},
  "worker_cli_js": {"sha256": "<64-hex>", "size": 1234},
  "mcp_package": {"sha256": "<64-hex>", "size": 1234, "version": "0.0.85"},
  "playwright_package": {"sha256": "<64-hex>", "size": 1234, "version": "1.66.0-alpha-2026-11-09"},
  "playwright_core_package": {"sha256": "<64-hex>", "size": 1234, "version": "1.66.0-alpha-2026-11-09"},
  "playwright_core_browsers_json": {"sha256": "<64-hex>", "size": 1234},
  "chromium": {"sha256": "<64-hex>", "size": 1234, "version": "158.0.8127.18", "revision": 1254}
}
```

worker 文件路径固定为 worker root 下的 `node_modules/@playwright/mcp/cli.js`、三个 `package.json` 与 `node_modules/playwright-core/browsers.json`；Node 与 Chromium 使用设置中的绝对路径。

本机真实集成入口为 `crates/saya-core/tests/browser_worker_real.rs`，默认 `#[ignore]`，通过 `SAYA_TEST_NODE`、`SAYA_TEST_WORKER_ROOT`、`SAYA_TEST_CHROMIUM_BIN`、`SAYA_TEST_IDENTITY_MANIFEST` 显式启用。测试 fixture 使用固定 headless-shell，而不是本环境会崩溃的完整 Chrome：macOS 为 `target/n5/browser-cache-mirror/chromium_headless_shell-1254/chrome-headless-shell-mac-arm64/chrome-headless-shell` 与 `target/n5/mac-worker-manifest.json`，Linux ARM 为 `/opt/browsers/chromium_headless_shell-1254/chrome-headless-shell-linux-arm64/chrome-headless-shell` 与容器内 `/opt/browsers/worker-manifest.json`。这些是测试输入，不是产品默认值；产品只接受宿主显式配置的绝对路径与 manifest。

## 托管捕获引擎扩展（D-068，待实现）

项目负责人已确认允许扩展托管引擎的捕获与固定身份契约，必要时引入定制 Chromium。普通 CDP 的 renderer 暂停、DOM／CSS 遮蔽原语不等于导航提交、绘制与捕获之间的完整事务屏障，不能单独据此开启生产截图。

后续实现必须在生成图片前完成字段遮蔽，并证明捕获没有跨越未经授权的文档、页面激活或绘制帧；closed-shadow、子 frame 与独立合成内容不能因普通选择器不可见而漏过。私有协议只接受宿主构造的固定捕获请求，不接受模型脚本、selector 或自定义遮蔽规则。超时、取消或恢复状态不能确认时，应隔离并关闭 worker、返回 unknown，而不是返回待事后修补的图片。D-066 的双层 sandbox、现有网络／凭据／PLAN／审批边界不变。

固定身份需关联精确上游源码、审核后的补丁、DEPS／工具链／构建参数与配方，以及各架构产物及分发摘要。现有 v1 manifest 只校验明确命名的文件，不声称覆盖整个安装树或证明定制构建来源；最终协议、身份格式与分发产物仍待实际实现和验收，不以配置声明代替构建证明。

当前未构建或部署定制引擎，生产 `screenshots`／`screenshot_masking` 保持 false。固定源码的[Linux 构建要求](https://github.com/chromium/chromium/blob/6449d9f9a6a906edff724b7b220199b3fde4615c/docs/linux/build_instructions.md#system-requirements)列出 x86-64 构建机与至少 100 GB 空闲空间；本轮 ARM 宿主的 Akari 卷独立测量约 31 GiB 可用，且默认 Linux 工具链不能直接假定为原生 ARM 构建路径。尚未确认有满足条件的独立构建环境，也没有清理归属不明的 Rust 或用户缓存。实际生产事务与四平台成功证据仍是未完成项。

## DEV-080 VM 后端扩展

原生 macOS Seatbelt 无法在 Chromium 重初始化其 sandbox 时继续保持外层限制，Linux Bubblewrap browser profile 的 `setgroups=deny` 也会阻止 Chromium 内层 namespace 初始化。按 D-066，DEV-080 增加显式 `colima` VM 后端：worker 与 Chromium 在专用 VM 内运行，宿主只运行受控 `ssh` 客户端，因此 Chromium 自有 sandbox 不被削弱。该扩展不改变 native 组件或既有失败边界；`browser.enabled` 选择 VM 时不会回退到 native／Host。

- 配置：`[browser.vm]` 默认关闭，启用时必须显式提供 `colima`、`ssh`、已有专用 `profile`、固定 `host`（字面回环地址）、`user`、`port`、`identity_file`、`known_hosts`、远端 `node`／`worker_root`／`chromium`／`identity_manifest`／`identity_sha256` 与 `remote_temp_root`。`browser.vm.enabled` 与 `browser.worker.enabled` 互斥。宿主路径必须是绝对路径；远端路径必须是安全 POSIX 绝对路径，拒绝 `..`、控制字符与 shell 元字符。
- 宿主通道：`SshVmCommandRunner` 与长驻 launch 都经 `PreparedEnvironment`／`ManagedProcess`，按 Auto 在 macOS 选择 Seatbelt、Linux 选择 Bubblewrap。`read_files` 只读绑定 `ssh`、identity 与 known-hosts 三个精确文件，写范围仅一个 0700 Run 私有目录，不继承宿主环境。`NetworkPolicy::LoopbackPort { host, port, proxy_port }` 保留 SSH 回环字面地址，并可增加本次 broker 的 `127.0.0.1` 端口；Linux 精确桥接这些地址，Seatbelt 继续按 localhost 端口限制。通用默认仍为 `Deny`。SSH 配置固定 `BatchMode`、`IdentitiesOnly`、`StrictHostKeyChecking`、`PublicKeyAuthentication`、`ForwardAgent no`、`ClearAllForwardings yes`、`ProxyCommand none`、`ProxyJump none`，不包含秘密值。
- Linux 宿主边界：保持私有 netns 和通用 payload Seccomp，只在启动期由固定镜像、device／inode 复查的 socat 建立最多两个 TCP-to-Unix 桥。私有启动记录最多 16 KiB；IPv4 字面地址与 `::1` 不改写或回退。宿主受管 relay 只连接固定目标，共享最多 32 个在途连接（含连接中）、每方向 16 KiB 缓冲及 3 秒连接期限。SSH 桥保持服务端先发 banner；HTTP broker 桥收到首字节后才连接上游，空就绪探测不消耗 Run 的累计连接预算。私有目录对完整 UUID 做 URL-safe 无填充 Base64 编码，socket 使用短名，为 Unix 路径上限保留空间；64 字节 TMPDIR 下的真实 prepare 已覆盖，不以切换后端或扩大文件授权解决路径限制。清理停止并 join relay 后，按 socket 类型、device／inode 核对所属路径再删除，部分启动失败与 Drop 同样回收，重复清理或路径替换不会直接删除其他目录项。只生成最小只读 passwd／group／NSS 视图，不映射宿主账号库；命名 Unix socket 与未声明 FD 仍不向 payload 开放。缺少原生隔离前提时明确失败，不回退 Host。受控夹具与验证限制见 [Linux 宿主通道验收](n5-validation.md#linux-vm-宿主通道增量)。
- 身份校验：`VmWorkerIdentityVerifier` 通过同一受控 VM 通道读取远端 manifest 与七个命名文件，在本地核对大小、SHA-256、权限、Node 实际 `--version`、`@playwright/mcp 0.0.85`、`playwright`／`playwright-core 1.66.0-alpha-2026-11-09`、Chromium `158.0.8127.18`／revision 1254 以及 `chrome-headless-shell` 路径。仅远端命令退出成功不构成通过；远端返回的字节与解析结果必须与 manifest 和 pinned 版本一致。native 路径继续使用现有 `verify_worker_identity` 与测试。
- 生命周期：每次 launch 生成唯一远端 Run 根（0700，basename 保持较短以适配 Unix socket 路径上限），通过受控通道创建目录并写入 worker config；`PREPARE_RUN_SCRIPT` 同时创建 `home`、`home/cache`、`home/data`、`home/state`、`home/runtime`、`s` 等 LAUNCH_SCRIPT 导出的全部 XDG／HOME／TMPDIR／socket 目录，模式均为 0700。worker 以 `setsid -w` 建立独立 session／process group，PID 文件与 `/proc/<pid>/cmdline` 必须在本地核对。MCP 继续复用现有 `McpClient::connect_stdio`、协议版本与七工具 catalogue 校验；worker 在首次启动浏览器后发布新的 catalogue，`open` 与 `close` 在生命周期步骤中重新发现并校验该 catalogue，普通工具调用仍对 stale catalogue 失败关闭。远端 cleanup 将已退出或 zombie 的 PID 视为已消失，按进程组有界清理并删除 Run 根；PID 文件缺失、为空、损坏或对应进程已退出时，以唯一 Run 根为匹配在 `/proc` 中有界扫描相关进程，排除清理 shell 自身（含 `saya-browser-vm` argv），确认终止或证明不存在后才删除 Run 根，`expect_worker` 只作观测，不再产生永久 `worker=unknown`。启动在生成 Run 根后、首个 prepare／launch await 之前就把 cleanup 句柄注册到 launcher 的 pending 队列；成功时由 connection 接管并移除，失败、取消或外层丢弃 launch future 时保留。重试逐项取队首，确认成功后才移除该项；外层 timeout／cancel／drop 保留当前及剩余全部句柄。除显式 shutdown 外，`VmWorkerConnection` 还监视本地 SSH 子进程并实现 Drop 清理：SSH 断连或 worker 退出时自动触发有界远端清理；没有 Tokio runtime 时记录 unknown 而不是假装成功。启动失败或清理失败时，provider 与 `VmWorkerLauncher` 保留可重试清理句柄，下一次 launch／shutdown 必须先确认旧 Run 根清理，不能把第二次尝试报告为新成功。SSH 断连、取消或 Run 结束的清理无法确认时返回 `outcome_unknown`，不把本地 SSH 退出当作远端已清理。
- 身份／prepare／sandbox／清理调用区分派发前后：命令在派发前被取消返回 `cancelled`；已派发后取消或超时返回 `outcome_unknown`，不会冒充普通身份或清理失败。Zombie／已退出 worker 的 Run 根清理由 `CLEANUP_SCRIPT` 的存活判定处理，并有真实脚本回归覆盖。
- Sandbox 证据：`VmChromiumSandboxInspector` 使用远端 worker PID 在 VM `/proc` 中查找 `--type=renderer` 后代，要求 argv 未禁用 sandbox、`NoNewPrivs=1`、`Seccomp=2`；宿主 `/proc` 不参与该判断。VM 内普通进程没有这两个属性，因此它们在该环境是可判别证据，而不是宿主 Bubblewrap 的固定赋予。DEV-080 的 VM 网络在本次扩展时仍不是受控 Browser 出口，`NetworkPolicy::LoopbackPort` 只覆盖 SSH；DEV-081A 已接入 VM broker（见下一节）。当前 downloads／uploads facts 为 true；screenshots 与 screenshot_masking 为 false（pinned worker 无遮蔽输入）。
- 显式工具：`scripts/n5/browser_vm.py` 提供 `create`／`provision`／`check`／`destroy`。`create` 固定 `--mount none` 与 `--ssh-agent=false`；`provision` 只复制调用者给出的 Node、worker 树与 headless Chromium，从远端文件生成 manifest 并上传，不执行 `npm`／`npx`／下载或版本自动更新；`check` 重新核对 manifest 与全部命名文件，`provision` 在上传后复用同一套校验。两者都要求 manifest 自身是常规文件、大小不超过 64 KiB、组／其他不可写，并要求七个命名文件均为常规文件、各自不超过 512 MiB 且大小、摘要与权限符合运行时规则。工具回归在 `scripts/n5/test_browser_vm.py`，由 `just test-tooling` 执行。
- 实现状态：`crates/saya-core/src/browser/vm.rs`、`crates/saya-app/src/lib.rs` 的显式装配、上述工具与离线测试已完成；`cargo` 单元测试覆盖 SSH 配置、路径注入（含符号链接到含换行目标的反向测试）、回环地址归一化、身份不符、远端 PID／unknown、取消清理、断连／Drop 清理、失败清理句柄保留与重建、无宿主挂载与 loopback-only 网络。宿主通道的 `read_files` 固定为 `ssh`、identity、known-hosts 三个文件，不再包含 `/etc/passwd`。真实 VM 验收入口为 `crates/saya-core/tests/browser_vm_real.rs`，默认 `#[ignore]`，必须提供 `SAYA_TEST_EXECUTABLE` 及全部必需 `SAYA_TEST_VM_*` 输入；缺失、程序不可固定或装配失败均明确失败，不使用默认 release 路径或成功跳过。2026-11-13 在专用 Colima VM（profile `saya-browser`，Lima `colima-saya-browser`，`127.0.0.1:58780`）上实际执行该入口通过：MCP 启动、初始 `browser_navigate`、`browser_close` 与 `shutdown` 完成；VM 内 renderer 证据为 `NoNewPrivs=1`、`Seccomp=2`，argv 未包含禁用 sandbox 的选项；运行后远端 Run 根与 worker／Chromium 进程均已清理。该结果只覆盖这一显式 provisioned 环境，不构成 native 浏览器、四平台或 DEV-081 至 DEV-083 的验收。

真实 VM 验收步骤见本文末节与 [开发进度](progress.md)。VM 内 Chromium 与 Playwright 的实际启动、sandbox 证据、取消／断连清理和 Run 根删除必须在显式 provisioned 环境取得证据。

## DEV-081A Browser 网络阶段

DEV-081A 的阶段性范围仅包含网络出口、策略与验收；登录、文件 broker 与后台生命周期随后已由 DEV-081 完成。

- 新增严格、默认关闭的 `[browser.network]`：`allowed_origins`、`public_web = false`、`localhost_development_origin` 与现有 `BrokerLimits`。启用时必须至少有一个精确 origin 或显式 `public_web = true`；空 allowlist 加 `public_web = false` 直接拒绝。`public_web` 仍对每个连接重新规范化 origin、执行完整 DNS 解析、公开地址、内网／元数据／混合答案检查，不会绕过任何地址规则。开发 origin 必须是回环地址且同时出现在 allowlist 中。通用 `BrokerPolicy` 默认仍是 `public_web = false` 和 `NetworkPolicy::Deny`，现有 broker 行为不变。
- VM 浏览器不再使用 VM 默认出口。`BrowserNetworkFactory` 在 Run 开始时只创建一个 `Arc<RunNetworkBudget>`，所有 worker 生命周期与 broker 重启共用同一组累计 bytes／连接尝试和固定 deadline；重启不会清零已耗尽的上限。宿主每次 worker 生命周期启动一个 `NetworkBroker` 监听器；长期 SSH 通道只增加一条严格的反向 streamlocal 转发到该 broker 的回环端口；VM 内在 `sudo -n unshare -n` 创建的私有网络命名空间中启用 `lo`、确认不存在额外路由，并以原用户启动 `socat`，把 `127.0.0.1:31281` 接到上述 Unix socket。worker 随后 `setpriv` 回原 uid／gid 启动，Chromium 只使用该 loopback 代理。宿主 Seatbelt 放行两个不同的回环目标：Colima 转发的 VM SSH 端口与本次 Run 的 broker 临时端口；VM 内的 `31281` 属于私有 netns，不是宿主通道端点。通用默认不变。
- Chromium 保持自有 sandbox。launchOptions 同时在 `proxy.server`、`--proxy-server` 和固定 argv 中给出代理；`--proxy-bypass-list=<-loopback>` 禁止隐式回环直连；禁用 QUIC／HTTP3、非代理 WebRTC UDP 与后台网络；`--host-resolver-rules=EXCLUDE 127.0.0.1, MAP * ~NOTFOUND` 只允许代理 IP 字面量本地解析，页面主机名全部交给 broker；`--block-service-workers`、`--no-pings` 与既有 `--sandbox` 保持。WebSocket 与页面内其他连接同样只能经过该代理；特殊 URL 仍由 Provider 拒绝。
- 新增有界 `NetworkObservation`（最多 256 条），只记录 Run／worker 关联、规范化 `scheme://host:port`、结果、字节数与耗时。路径、query、headers 和凭据从不记录；CONNECT 隧道明确标记 `tunneled = true`，不声称隧道内 path 可见：broker 只能看到新连接（不同 origin 需要新的 CONNECT）与 worker 上报的最终 origin，不能看到隧道内的明文 redirect。malformed／oversize、监听器容量与共享预算／固定 deadline 拒绝即使无法规范化 origin 也会记录，origin 明确为未知而不是伪造；连接任务被 Run 取消或 shutdown 中止记录 `cancelled`，只有真实连接／空闲超时才记录 `timeout`，固定 deadline 到期记录 `limit`。CONNECT 中途 timeout／drop 读取共享计数，报告已实际转移并计费的 bytes，而不是 0。`browser_navigate` 在派发前核对规范化目标；worker 返回的最终 URL 允许合法 fragment（OAuth 式 redirect）但只接受 HTTP(S)、无 userinfo、有效 host，来源仍以脱敏后的 origin 表示。`about:blank` 通过 URL parser 接受合法 query／fragment，`chrome-error:` 明确分类为浏览器错误页；两者都是非网络状态，不参与 origin cross-check。重定向链的每一跳仍由 broker 实际检查；当 `network.enabled = false`（native 路径）时不对最终 URL 执行无关的 origin cross-check。
- 网络隔离失败时返回 `browser_network_unavailable`，不会回退到 VM 默认网络。`sudo -n`／`unshare`／`socat`／`setpriv`／`ip` 缺失、`lo` 无法启用、出现额外路由、代理未监听或反向 socket 缺失都会在启动脚本内失败关闭；sshd 拒绝反向 streamlocal 转发时客户端以 `ExitOnForwardFailure` 失败，其固定诊断同样映射为 `browser_network_unavailable`。预算耗尽或固定 deadline 到期则返回独立的 `browser_network_budget`，不与隔离能力失败混淆。读取 stderr 前先终止启动进程，并对有界 drain 任务做有界等待，避免只读到即时 tail；只有固定的 sentinel 映射为网络分类，其他启动失败仍是受控的传输错误。
- broker、反向转发与 VM 内 `socat` 纳入同一取消、unknown 与清理：launch 失败或取消时关闭 broker；`VmWorkerConnection::shutdown` 在远端清理后停止监听；SSH 退出使反向转发消失；`CLEANUP_SCRIPT` 以唯一 Run 根关联扫描并终止 worker、socat 与包装进程，再删除 Run 根。既有 pending cleanup、Drop、SSH 断连与 unknown 语义保持不变。
- `scripts/n5/browser_vm.py` 的 `provision`／`check` 在远端验证 `sudo -n`、`unshare -n` 与 `ip route`、`socat`、`setpriv`、`ip`、`nsenter`（含 `sudo -n nsenter`）、Chromium 所需的 unprivileged user namespace 以及当前 `kernel.apparmor_restrict_unprivileged_userns` 值；两者还通过 `sshd -T` 验证服务端有效配置允许 `AllowStreamLocalForwarding`（并拒绝显式的 `AllowTcpForwarding no`），而不是只检查客户端 argv。`provision --configure-apparmor-userns` 是显式 opt-in，只在专用 VM 内写入 `kernel.apparmor_restrict_unprivileged_userns=0` 的 sysctl drop-in，不修改宿主；`check` 始终验证当前值与工具，缺失时受控失败。
- 真实验收入口为 `crates/saya-core/tests/browser_vm_network_real.rs`，默认 `#[ignore]`。它在宿主启动纯本地静态／重定向／动态／慢页面 fixture，VM 浏览器只经 broker 访问。旁路 canary 建在**浏览器 worker 实际所在的私有 netns** loopback 上：测试先从 broker observation 取得 worker PID，用 `sudo -n nsenter -t <pid> -n` 在该 netns 启动 TCP／UDP canary，再从**同一 netns** 用原始 TCP／UDP 正控制确认两者确实可达并重置计数。动态页随后通过子资源请求与真实 `RTCPeerConnection` STUN 尝试 canary，并在可见 DOM 中记录 `STUN_STARTED` 及最终的 `STUN_DONE`／`STUN_FAILED`；snapshot 必须看到这些标记，证明脚本实际执行。若 Chromium 忽略代理直连，请求会真实命中 canary，因此零命中是有意义的负控制。测试还断言来源、最终 URL、redirect、metadata 拒绝观测、慢页面取消；canary 计数与观察条数随环境变化，不写成固定常量。清理覆盖 Run 根、worker、socat 与 netns canary，leftover 扫描要求全部消失。默认不访问真实外网。该入口已在专用 Colima VM 上实际执行通过；同一环境下的 `browser_vm_real` 也通过，renderer 为 `NoNewPrivs=1`、`Seccomp=2`。

## DEV-081 登录、文件 broker 与后台阶段

本节记录 DEV-081 交付。DEV-082 已接入截图宿主链路、模型图片注入与 CLI／TUI 展示；worker／VM 仍因缺少真实遮蔽能力而禁用截图，联合验收属 DEV-083。

- 凭据槽：新增严格 `[browser.credentials]`（`enabled`、私有 `store`、`slots`）。每个 slot 只有别名、`CredentialRef`、精确规范化 origin、`form_fill` 用途和 `max_uses`；origin 仅允许 HTTPS，显式回环开发 origin（`localhost`／回环 IP）才允许 HTTP。`browser_fill` 的 credential 路径只接受顶层 document 元素：`BrowserElement` 保留 frame 身份，任何子 frame（包括 `fNeM` 形式引用）都以 `credential_frame_denied` 拒绝，普通 `text` 填充仍可使用子 frame 引用。模型 `browser_fill` 必须且只能给出 `text` 或 `credential_slot` 之一；slot 路径在派发前核对当前 document token、page origin、用途、撤销状态和剩余次数，值只在最终 `BrowserProvider::fill` 边界暴露，并注册到共享 `SecretRegistry`。工具意图、结果、日志、事件、OutputStore、Compact 与导出都不含明文；填充不会自动 submit，`press`／`click` 仍是独立 BrowserWrite 审批。次数在派发前消耗，unknown 不重放且不返还。
- 文件 broker：`browser_download` 必须给出 `page`、`document`、`element_ref`，映射为对该元素的受控点击。路径只从固定 `### Download` 事件段落解析，并要求点击前后下载目录恰好多出一个同名文件；页面 title、snapshot 正文或其他段落中的伪事件、重复事件、旧文件与目录外路径都拒绝。`provider.download` 的任何 `Err`（包括 `download_missing`、`invalid_provider_result`、`file_path_denied`、`file_too_large`、`file_broker`）都保守作废该 document：已知失败保留可重试观察、不自动重放，unknown 仍按 unknown 处理，只有新 snapshot 才能恢复。宿主校验路径位于 Run 输出目录、常规文件、大小、声明类型、magic 与 SHA-256，净化文件名后保存为有界 `File` artifact；结果带 `artifact_ref`／`media_ref`、摘要和元数据，File 永不作为模型图片／音频／视频注入。`browser_upload` 只接受当前 Session 的 artifact 引用；宿主解析摘要、大小和归属，复制到 Run 私有 0600 staging，再由 worker／VM 通过受控通道写入执行侧 staging 并调用固定 `browser_file_upload`。执行侧 staging 使用原名作为页面可见 basename（不添加合成前缀），放入唯一私有目录；创建、写入、sync、chmod、mv 失败或取消时删除半成品，动作后删除文件与目录。任意宿主路径、路径穿越、跨 Session 引用、超限和类型不符均在派发前或落盘前拒绝。
- 媒体生命周期：`CapturedToolResult` 持有本次创建对象的 lease 至结果提交；历史 `ToolResult.media_refs` 只由宿主写入，GC pin 使用 `media::history_references`（完整历史与 Compact 证明），页面文本或模型输入中的引用不产生 pin。恢复后引用可读取，缺失时明确 `media_unavailable`。`media::load_history` 只加载实际进入本次 request history 的 `UserText` Image／Audio／Video 引用及未被 Compact／retirement 丢弃的 `ToolResult.media_refs`；工具引用只接受 Image，最多 8 个，聚合原始字节与 base64 均不超过 64 MiB；超出预算或能力关闭的引用记录为 omitted，不清空 GC pin，也不读取正文。
- 后台：`browser_navigate`、`browser_wait`、`browser_download` 支持显式 `background: true`，返回真实 job ID，并复用现有 `BackgroundJobs` 的并发、超时、PLAN、Run 清理、`background_status` 与 `background_cancel`。`browser_navigate` 与 `browser_download` 的已派发后台取消映射 `outcome_unknown`，无副作用的 `browser_wait` 取消为 `cancelled`；均不自动重放。未配置 supervisor 时明确 `unsupported`。`BrowserState` 的 revision／unknown、OutputRefs、Run 网络预算与 VM pending cleanup 语义保持不变，文件／凭据错误不绕过权限、PLAN 或 artifact ACL。浏览器业务失败只形成 job failed／cancelled，不触发 supervisor fatal 或关闭整个 Run；`browser_download` 的 artifact lease 保存在 job 状态，`background_status` 重投影保留合法 `media_refs`，结果在写入状态前按动态 `SecretRegistry` 脱敏。`tool_timeout` 分支即使在 deadline 之后才观察到完成的 `Ok` 结果，也把 `SealedMedia` lease 放入 job 状态并保留 `observed_result`，job 失败期间 artifact 不被 GC，直到 job 状态和调用方不再需要。`BrowserState` 只持弱引用 supervisor；PLAN 清理只解除当前生命周期并使 `background=true` 明确失败，显式 `browser_open` 会幂等重新绑定同一 Run 的存活 supervisor，已关闭的 jobs 永不复用。`BrowserState` 的 lifecycle 取消信号同时唤醒在途 provider 调用（尤其 `wait`），PLAN 清理／Run shutdown 不必等满 wait deadline 或页面锁；该信号不取消 cleanup 自身的 close／shutdown 调用。
- 受控执行的 worker 侧变化：`WorkerConnection` 增加 Run 私有 staging 写入与输出目录内下载读取；VM 使用同一严格 SSH 通道，以 `stat`／`cat` 和 0600 staging 写入实现，拒绝 `..` 与 Run 输出目录之外的路径。宿主本地的上传 staging 位于 OutputStore 保护目录下。
- 真实入口为 `crates/saya-core/tests/browser_vm_files_real.rs`，默认 `#[ignore]`，只使用宿主本地 fixture 与合成测试凭据。2026-11-13 在专用 Colima VM 上实际通过：表单凭据填充且未自动提交、错误 origin／stale document／次数耗尽、子 frame 凭据引用拒绝、accessible name 中的伪 `[ref=...]` 不改变真实元素定位、实际下载并写入 artifact、实际上传 `downloaded bytes` 且页面 multipart 收到原名 `invoice.txt`、路径穿越拒绝、provider `download_missing` 与宿主 `file_too_large` 后旧 document 失效并可由新 snapshot 恢复、后台下载 artifact 在 GC 后仍可上传、超时后完成的下载 artifact 在 job failed 状态期间仍被 lease 固定并在 job 状态释放后可回收、后台结果在回报前脱敏、后台慢导航取消为 unknown、snapshot 恢复、PLAN 清理后显式 open 再执行真实后台导航，以及 shutdown 后无 Run 根／worker 残留；artifact 在重新打开 OutputStore 后仍可读取。同环境 `browser_vm_real` 与 `browser_vm_network_real` 一并通过。该结果不构成 native 浏览器、四平台或 DEV-082／DEV-083 验收。

## 范围与首个实现

浏览器能力包含 URL 读取、网页观察与交互、截图、下载以及本地网页验证。首版候选为宿主管理的临时 Chromium 会话，通过受控 Playwright MCP worker 接入。Rust core 负责工具、权限、生命周期和预算，Playwright 运行在可选隔离环境中。

| 增量 | 内容 |
| --- | --- |
| B0 受限网页读取 | 显式 URL／已配置搜索服务、正文提取、来源、重定向校验及长输出引用 |
| B1 托管浏览器 | 创建／关闭、导航、ARIA 快照、定位、点击、填写、按键、截图与等待 |
| B2 受控会话资源 | 下载／上传、授权登录、有限 profile 保存与本地开发服务 |
| 后续连接方式 | 用户现有浏览器、远程浏览器或其他引擎，分别验证权限与生命周期 |

Playwright MCP 提供基于结构化 accessibility snapshot 的工具接口。工具清单由 Saya 的注册与策略层筛选，特定 provider 的完整工具集合在兼容测试中记录。[Playwright MCP](https://github.com/microsoft/playwright-mcp)

## 宿主与 worker 边界

```mermaid
flowchart LR
    M[模型工具请求] --> R[Tool Registry 与操作描述]
    R --> P[工作模式 权限 AST或参数检查]
    P --> B[BrowserProvider]
    B --> S[受控 MCP 通道]
    S --> W[隔离 worker 中的 Playwright]
    W --> C[Chromium sandbox]
    C --> N[受控网络出口]
    W --> O[OutputStore 与产物 broker]
```

BrowserProvider 管理会话 ID、页面 ID、文档版本、能力、任务归属和清理。每个 Run 默认获得私有临时会话，同一页面的动作串行执行。调用通过稳定的内部操作类型映射到经验证的 MCP 工具名。

浏览器控制使用受管理通道，控制端点和认证引用由宿主持有。模型接收页面／元素引用与操作结果。扩展工具、任意脚本执行、浏览器安装及通用代码入口按独立能力注册和验证。

## DEV-082 实现状态

DEV-082 交付截图宿主链路、模型图片注入、CLI／TUI 展示与输出检索验证，不改变权限、PLAN、低信任数据、unknown、OutputStore／GC 或 VM sandbox 边界。

- 截图工具名仍为 `browser_screenshot`，provider 固定调用 `browser_take_screenshot`。宿主生成唯一文件名并写入 Run 私有 OutputStore，`result.data.screenshot` 返回 `page`、`url`、`url_observation`、`document_revision`、`mime`、`filename`、`bytes`、`sha256`、`masked`、`artifact_ref`、`model_image_capability` 与 `model_image_status`；`source` 为 `untrusted_browser`。`url_observation = "last_confirmed"` 表示 URL 是最近一次 provider 确认值，不是截图瞬时来源；open 保存已验证 URL，后续动作在同页锁内更新，缺少 URL、失败或未知结果时清空不可靠缓存。动作不能恢复文档授权，仍需新 snapshot。`model_image_capability` 只声明截图时所选 profile 的图片能力（`enabled`／`disabled`），不是随后每次模型请求的注入事实；`model_image_status` 同步说明该声明。宿主字段 `media_ref` 本身不序列化，公开的 `artifact_ref` 供模型与界面定位产物；二者可以使用同一不透明存储引用值。引用不包含本地路径或凭据，也不能绕过 Session 归属和摘要校验。`ToolResult.data` 不含 base64。
- 截图必须同时满足 `facts.screenshots`、`facts.screenshot_masking`、文件 broker 与产物存储可用；只满足 facts 不足以注册或执行。provider 未确认 `masked=true` 时不持久化、不返回成功。pinned `@playwright/mcp 0.0.85` 的 `browser_take_screenshot` schema 没有 `mask` 输入，固定 worker 没有在截图生成前遮蔽密码、one-time-code、凭据槽元素与已标敏感输入的真实契约，因此 worker／VM provider 声明截图能力关闭，`browser_screenshot` 不注册；直接调用返回 `browser_capability_disabled`。这属于固定 worker 的预捕获遮蔽缺口，不是 DEV-082 图片／产物功能未实现。具备真实遮蔽能力的受控 provider 用于验证成功路径、lease、GC pin、重新打开 OutputStore 后读取及下一请求注入（含能力开关）。
- `ToolResult.media_refs` 中只有宿主写入且类型为 Image 的对象可进入下一次 Execution 模型请求；File 永不为模型媒体，工具产生的音频／视频也不注入。加载只针对本次 request history 中未被 Compact／retirement 丢弃的引用，最多 8 个，原始字节与 base64 聚合均不超过 64 MiB；多工具结果合并后的 wire 图片总量同样受该上限约束。三协议分别使用相邻 user `image_url`、`function_call_output` 后的 `input_image`、以及 `tool_result` 内的 base64 `image` block。关闭 `profile.multimodal.image` 时不产生 image block，截图元数据明确记录 `disabled`；Compact／Review 等非 Execution 请求不注入。
- 截图与下载的 artifact 引用随历史保存并参与 `media::history_references` GC pin；Compact 投影保留 `media_refs`，恢复后仍可读取。普通 transcript 与事件使用 `model_view()`，不含 `media_refs`；Full 导出保留经校验的不透明引用。界面显示公开的 `artifact_ref`、大小与 SHA-256，不额外显示宿主字段 `media_ref`／`media_refs`；相同引用值不等于宿主 GC pin 或跨 Session 读取授权。`browser_upload` 继续只接受模型可见的 `artifact_ref`，保持上传链路语义。
- CLI 审批显示 operation summary、具体浏览器动作、page／URL／credential slot 与 fill 字节数（不回显填充文本）；TUI 工具摘要与详情使用集中 i18n 文案显示 action、final URL、page／document revision、output_ref、artifact mime／size／SHA-256、masked、后台 job 与 unknown。真实 PTY 预览场景验证了详情渲染、信号取消与 alternate-screen 恢复。
- 本机验证：`cargo test -p saya-core --lib`、`cargo test -p saya-core --tests`、`cargo test -p saya-app -p saya-tui -p saya-cli`、三协议 wire 测试、PTY 测试、`cargo fmt --all -- --check`、workspace 严格 Clippy 与 check。受控遮蔽 provider 覆盖截图成功路径、artifact／lease／GC、`model_image_capability` 开关、真实嵌套结果形状与三协议图片注入；网络与 VM 固定路径仍不产生截图。专用 VM 的 `browser_vm_files_real --ignored` 已实际重跑通过：截图能力关闭与 `browser_capability_disabled` 拒绝、下载／上传、后台 artifact 跨 GC 上传及 Run 清理均取得证据，运行后无 Run 根或浏览器进程残留；本次结果不构成四平台或 DEV-083 验收。

## 工具与权限草案

| 模型工具族 | 参数与返回 | 策略类别 |
| --- | --- | --- |
| `fetch_url`／`web_search` | URL 或查询、正文／摘要、来源与 output_ref | 受限网络读取，搜索使用显式配置的服务 |
| `browser_open`／`browser_close` | 会话、页面、归属及状态 | 浏览器资源生命周期 |
| `browser_navigate` | URL、最终来源；建立新文档并返回 page／url | 浏览器交互与网络 |
| `browser_snapshot` | 页面、子树／查询、元素引用、水位 | 页面观察 |
| `browser_click`／`browser_fill`／`browser_press` | 当前文档元素引用、动作与结果 | 可能的外部副作用 |
| `browser_wait` | 有界条件、时间及状态 | 有界等待 |
| `browser_screenshot` | 范围、遮蔽、图片产物 ID | 图像产物访问 |
| `browser_download` | page／document／element；只接受该动作产生的文件、来源与摘要 | 文件传输 |
| `browser_upload` | 当前 Session 的 artifact 引用、page／document／element | 文件传输 |

首版 PLAN 工具面限定为受限 URL 读取及已保存观察结果，主动浏览器交互在 BUILD／CODE／WORK 中按策略执行。URL 读取使用受控 GET／HEAD 和目标范围，记录实际网络行为。服务端 GET 的真实效果依赖目标服务语义。

`auto` 根据宿主分类、规则和必要核查决定交互许可。`manual` 包括快照和后台查询在内逐工具批准。提交、上传、下载和凭据使用具有独立操作描述。`bypass` 受选定浏览器 profile 的实际隔离与网络边界约束。

元素引用绑定 page／document revision；`navigate` 建立新文档，`click`／`fill`／`press`／`upload` 使用当前文档，页面导航或关键结构变化后重新取得引用。确认绑定来源、页面版本、参数与策略版本，执行前重查。

## 隔离与网络

浏览器 worker 通过 SeatbeltBackend 或 BubblewrapBackend 启动，使用独立临时目录及受控产物交换范围。Linux 使用 Bubblewrap + Seccomp + socat，Chromium 以普通用户运行，单独验证允许其自有 sandbox 所需的 syscall。macOS 验证外层 Seatbelt 与 Chromium 子进程限制的组合。Playwright／Chromium 版本和运行文件身份固定，启动时验证实际 sandbox 能力。[原生执行设计](sandbox.md)

适配器显式设置 `chromiumSandbox = true`，并验证所选 worker 的实际启动状态。Playwright 的该启动选项默认关闭。[BrowserType 启动选项](https://playwright.dev/docs/api/class-browsertype#browser-type-launch-option-chromium-sandbox)

Playwright BrowserContext 提供 cookies 和存储层的会话隔离，原生执行后端负责宿主资源边界。[Browser contexts](https://playwright.dev/docs/browser-contexts)

URL 读取使用独立的受控 HTTP 客户端，认证信息按用途绑定。网络经受控代理和实际出口限制，逐连接检查最终地址、重定向、内网／元数据地址和 DNS 变化。首版关闭 service workers，WebSocket、直连 UDP／QUIC、代理旁路及浏览器特殊 URL 按明确能力处理。允许的导航 scheme 为 HTTP／HTTPS，内部空白页由宿主创建。

本地开发服务采用用户显式绑定的目标转发，记录主机、端口和生命周期。provider 的 origins／hosts 选项作为辅助配置，出口约束由 Saya 的执行环境和代理承担。Playwright MCP 文档说明其 origins 过滤具有适用边界。[MCP 配置与安全说明](https://github.com/microsoft/playwright-mcp#security)

## 登录、文件与输出

- 临时 profile 按 Run 清理。登录状态保存或复用需要单独范围、来源和期限，用户级凭据由 F-18 管理。
- 认证字段使用绑定 origin 的 credential slot，实际值通过宿主 broker 注入。工具意图、日志和上下文保存引用与遮蔽后的信息。
- 上传使用已授权 artifact ID，下载经 broker 生成文件名、检查来源／大小／类型并保存摘要。写入项目文件时经过工作区写回策略。
- 默认观察使用有界 ARIA／文本快照，返回子树、匹配项与文档水位。完整可保留内容按[输出策略](context-management.md)进入 OutputStore。
- 截图作为有界图片产物保存，文字结果返回元数据。图片注入依据模型 profile 的实际多模态能力，敏感输入遮蔽在截图生成前执行。
- 长导航、等待及下载可按 provider 声明显式后台运行，返回 job ID。后续依赖动作等待已完成状态，取消与 Run 结束清理由宿主协调。

## 交互与恢复

CLI／TUI 展示最终 URL、来源、页面／文档版本、当前动作、确认原因、后台状态和产物。网页文本、下载内容、脚本和插件说明以低信任来源数据进入模型上下文。

Run 结束或取消时停止调度并清理浏览器、worker、临时 profile 和下载任务。恢复保留观察与产物记录，重新建立的浏览器会话具有新身份。提交类动作的未知结果等待核对，后续操作使用新的显式调用。

## 验收

离线夹具提供静态页、动态 DOM、重定向、本地服务、表单、下载和长页面。覆盖陈旧元素引用、隐藏跳转、service worker／WebSocket 尝试、内部地址、特殊 URL、文件路径、输出配额、密码遮蔽、浏览器崩溃、取消、后台清理和 unknown 提交结果。

报告按平台记录实际浏览器 sandbox、网络策略、原生后端及资源能力。任务依赖及版本安排见[后续开发计划](post-mvp-plan.md)。

## 专用 VM 真实验收入口

以下步骤只在显式 provisioning 时执行网络与安装；模型 Run 不会触发它们。

1. 创建专用 VM（不挂载宿主目录、不转发 SSH agent）：

   ```text
   python3 -B scripts/n5/browser_vm.py create \
     --colima /opt/homebrew/bin/colima --profile saya-browser
   ```

2. 显式提供固定 Node、worker 树与 headless Chromium 源，复制进 VM 并生成远端 manifest：

   ```text
   python3 -B scripts/n5/browser_vm.py provision \
     --profile saya-browser --ssh /usr/bin/ssh \
     --host 127.0.0.1 --user saya --port 2222 \
     --identity-file ~/.ssh/saya-browser-vm --known-hosts ~/.ssh/known_hosts \
     --node-source /path/to/linux-node/node \
     --worker-source /path/to/worker \
     --chromium-source /path/to/chrome-headless-shell-linux64 \
     --remote-root /opt/saya
   ```

   工具会输出 `identity_sha256`、远端路径和 `remote_temp_root`；它不下载、不更新版本，源 package／Chromium 版本与 pinned 值不一致时失败。

3. 使用输出值配置 `[browser.vm]` 与 `[browser.network]`，显式启用 `[browser]`，然后运行真实入口。以下为当前专用环境使用的测试输入（均为显式测试值，不是产品默认）：

   ```text
   SAYA_TEST_EXECUTABLE=$PWD/target/release/saya \
   SAYA_TEST_VM_COLIMA=/opt/homebrew/bin/colima \
   SAYA_TEST_VM_SSH=/usr/bin/ssh \
   SAYA_TEST_VM_PROFILE=saya-browser \
   SAYA_TEST_VM_HOST=127.0.0.1 \
   SAYA_TEST_VM_USER=ariakage \
   SAYA_TEST_VM_PORT=58780 \
   SAYA_TEST_VM_IDENTITY_FILE=$HOME/.colima/_lima/_config/user \
   SAYA_TEST_VM_KNOWN_HOSTS=$HOME/.colima/_lima/colima-saya-browser/ssh.known_hosts \
   SAYA_TEST_VM_NODE=/opt/saya-browser/bin/node \
   SAYA_TEST_VM_WORKER_ROOT=/opt/saya-browser/worker \
   SAYA_TEST_VM_CHROMIUM=/opt/saya-browser/chromium_headless_shell-1254/chrome-headless-shell-linux-arm64/chrome-headless-shell \
   SAYA_TEST_VM_MANIFEST=/opt/saya-browser/worker-manifest.json \
   SAYA_TEST_VM_MANIFEST_SHA256=e6af5a229682028f9be237a96c1e1f4699292d08b3b5bce88822e56a3d679b46 \
   SAYA_TEST_VM_TEMP_ROOT=/tmp/saya-runs \
   SAYA_TEST_VM_NETWORK_ORIGINS=https://example.com \
   cargo test -p saya-core --test browser_vm_real -- --ignored --nocapture

   SAYA_TEST_EXECUTABLE=$PWD/target/release/saya \
   SAYA_TEST_VM_COLIMA=/opt/homebrew/bin/colima \
   SAYA_TEST_VM_SSH=/usr/bin/ssh \
   SAYA_TEST_VM_PROFILE=saya-browser \
   SAYA_TEST_VM_HOST=127.0.0.1 \
   SAYA_TEST_VM_USER=ariakage \
   SAYA_TEST_VM_PORT=58780 \
   SAYA_TEST_VM_IDENTITY_FILE=$HOME/.colima/_lima/_config/user \
   SAYA_TEST_VM_KNOWN_HOSTS=$HOME/.colima/_lima/colima-saya-browser/ssh.known_hosts \
   SAYA_TEST_VM_NODE=/opt/saya-browser/bin/node \
   SAYA_TEST_VM_WORKER_ROOT=/opt/saya-browser/worker \
   SAYA_TEST_VM_CHROMIUM=/opt/saya-browser/chromium_headless_shell-1254/chrome-headless-shell-linux-arm64/chrome-headless-shell \
   SAYA_TEST_VM_MANIFEST=/opt/saya-browser/worker-manifest.json \
   SAYA_TEST_VM_MANIFEST_SHA256=e6af5a229682028f9be237a96c1e1f4699292d08b3b5bce88822e56a3d679b46 \
   SAYA_TEST_VM_TEMP_ROOT=/tmp/saya-runs \
   cargo test -p saya-core --test browser_vm_network_real -- --ignored --nocapture
   ```

   `browser_vm_real` 在 `SAYA_TEST_VM_NETWORK_ORIGINS` 缺失时使用合法且不会实际访问的默认 fixture origin（`https://example.com`），不会因空 allowlist 而 panic；`browser_vm_network_real` 自建宿主 fixture 与 VM loopback canary，不使用 `public_web` 或默认 VM 直连。

4. 随时以 `python3 -B scripts/n5/browser_vm.py check --manifest-sha256 <摘要> ...` 复核远端 manifest、命名文件大小／SHA-256／权限、Node 版本、Chromium revision，以及 `sudo -n`／`unshare`／`socat`／`setpriv`／`ip route` 与 user namespace／AppArmor 当前值；验收完成后以 `destroy` 删除专用 profile。需要让专用 VM 允许 Chromium 创建 unprivileged user namespace 时，使用 `provision --configure-apparmor-userns` 显式设置该 VM 的 sysctl drop-in；该选项不改宿主。

验收记录应包含 VM 内 renderer PID、`NoNewPrivs=1`、`Seccomp=2`、合法 sandbox argv、取消／断连后远端 PID 消失与 Run 根删除结果。native 浏览器若无法取得 Chromium 自有的 `InitializeSandbox` 证据，必须保持 `browser_sandbox_unavailable`，不能以 Bubblewrap 固定赋予的属性替代。本轮已在上述专用 VM 取得 VM 后端运行、sandbox、DEV-081A 网络/清理、DEV-081 登录／文件／后台证据，并重跑 DEV-082 的截图禁用、下载／上传／后台与清理路径；受控 provider 的截图成功与模型图片能力开关在本机离线／PTY 覆盖。生产固定 worker 仍无遮蔽契约，不应虚构真实截图；DEV-083 联合验收和四平台验收仍未完成，不能宣称 N5 或完整真实平台验收已经完成。
