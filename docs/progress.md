# 开发进度

**更新于2026-11-19。当前 workspace 版本为 `0.3.0`，已完成第二轮审查报告（`R-01`–`R-32`）及决策 `D-075`–`D-078` 全部整改：包含 `soyo helper` 多客户端／权限／配置指纹隔离、桌面端断线重连与 `externalBin` sidecar 随包、Soyo Dark／Soyo Light 双主题与 `Source Serif 4`／`SquaresLoader` 桌面端视觉重构、`soyo-tui` 模式色表对齐，以及 `desktop-ui.pen`（28 frame）和 10 张桌面实机截图同步；本机 macOS ARM／Rust 1.99.1 完成全仓 `just check`（81 组测试共 1790 项通过、0 失败、27 项条件忽略＋release 构建）、`soyo-desktop` Clippy／Rust 单测、`just desktop-check`（`tsc --noEmit` 零错、Vitest 2 文件 23 项通过、Vite 生产构建通过）与 `just test-tooling`（13 组 177 项通过）。历史基线继续按固定源码保留：N5 归属 `5fff1c4`（非 `linux-arm64` 定制 `headless_shell` 未覆盖）、N3 归属 `e1da373`、N2 归属 `aaec8c5` 与已发布 `0.2.0`。**

本文件是开发状态的唯一记录入口。[路线图](roadmap.md)定义阶段，[实施任务](implementation-plan.md)定义任务，[完整基线](mvp-baseline.md)定义产品行为。状态更新需要实际证据，不根据计划或估算自动推进。

> **更名说明**：项目自提交 `1452a0b851ad40b385bf00e3765a4186509c4dd0` 之后由 **Saya Harness** 更名为 **Soyo Harness**。二进制 `saya`→`soyo`，crate `saya-*`→`soyo-*`；数据目录 `.saya`→`.soyo`（`.soyo` 优先、`.saya` 自动回退），环境变量 `SAYA_*`→`SOYO_*`（`SOYO_*` 优先、`SAYA_*` 自动回退）。存储与线缆格式标识符（`saya_*_v1`、`SAYAENV1`/`SAYAOUT1`/`SAYAMED1`、`saya_capture_*`、`saya/browser/*`、`saya-media://`、`__saya_*` 等）保留旧拼写以维持既有数据与协议兼容。本文件及各分卷中分界提交之前的历史记录保留原 `saya` 名称，不作为现状描述。


## 分卷记录索引

逐次交付与验收证据按阶段分卷保存，本文件只保留当前状态、任务跟踪与更新规则。

- [N5 Browser 阶段](progress/n5.md)
- [N4 Goal 与同期增量](progress/n4.md)
- [N3 与 0.2.0 开发线](progress/n3.md)
- [0.1.0 MVP 与发行](progress/mvp.md)

## 当前状态

- 产品基线：D-027，经 D-029 补充运行内后台工具调用，D-030 确定提示词定制与语言，D-032 明确 Manual Approval，D-033 纳入用户级／工作区指令与 Skill 发现，D-034 补充动态上下文与前缀缓存，D-035 将最低支持版本调整为 macOS 15。
- 仓库基础文件：MIT、.gitignore、Rust 1.99.1 workspace、saya-core／saya-app／saya-tui／saya-cli 四个 crate 与 Cargo.lock 已建立。
- 开发计划：已准备，包含 P0 至 P7、DEV-001 至 DEV-037；工作量估算不是发布日期承诺。
- core 运行协调器、三协议适配、本地持久会话与恢复已接入 CLI。工具往返、取消、超时、预算、权限、模式切换、结构化输出和四类任务通过受控验证，指定模型三协议文本与工具往返完成小规模联调。正式源码 `eaade53` 的[四平台 CI](https://github.com/Ariakage/saya-harness/actions/runs/42383106399)与[原生发行验收](https://github.com/Ariakage/saya-harness/actions/runs/42383135600)均通过。
- [v0.1.0 已发布](https://github.com/Ariakage/saya-harness/releases/tag/v0.1.0)，四个平台安装包、校验和、验证记录及双语说明已上传，七项资产的公开下载内容均与验收产物一致。
- 后续规划包含 F-01 至 F-20，N0 至 N5／DEV-038 至 DEV-089 的依赖与验收见[开发计划](post-mvp-plan.md)。本轮细化 Browser、原生 Sandbox／AST、上下文／长输出和插件规范，运行内后台工具已有 D-029 的 MVP 实现。
- DEV-079 已完成宿主侧 Browser／Web 组件并通过本轮审查修复。`BrowserState` 校验 Run／Session 归属、每 Run 私有会话、同页串行、不透明文档与元素引用；`click`／`fill`／`press`／`upload` 显式核验当前 document token 并返回 `document_revision`，`wait` 后保守作废文档。unknown 绑定具体页面，跨页 snapshot 或新 open 不会清除；open 自身 unknown 使用独立全局保守状态，仅完整 close／shutdown 可清。Provider 全部方法接收取消，慢 wait／navigate 可中断；close 在 provider 确认前不丢弃 session／pages 并可重试，shutdown 幂等、失败可重试且有界超时。PLAN 清理保留可重启状态，回到 WORK 后显式 `browser_open` 创建全新 provider 生命周期。受控 `fetch_url` 支持 GET／HEAD、最多五次手工重定向、逐跳 origin／公开地址／DNS 检查与 IP 固定、禁用系统 proxy；DNS 与 body 读取响应 timeout／取消；非 HTML 截断、UTF-8 多字节截断与 HTML script／style／noscript／template 排除已覆盖，捕获失败返回有界正文前缀，截断捕获使用 `limit_reached` 而非 `eof`。新增 `WebRead`／`BrowserRead`／`BrowserWrite` 权限边界（open／close／navigate 为 BrowserWrite；snapshot／wait／screenshot 为 BrowserRead），只读运行时不注册也不执行 Browser 工具。本机验证：`cargo test -p saya-core --lib` 532 项通过，`cargo test -p saya-core --test config --test runtime_services --test registry --test permissions` 通过，`cargo test -p saya-app --test goal_fingerprint` 通过，`cargo clippy -p saya-core --all-targets -- -D warnings` 与 `cargo fmt --all` 通过；未运行四平台或真实浏览器。真实 Chromium／Playwright worker、登录、文件 broker、模型图片和四平台验收仍属 DEV-080 至 DEV-083，N5 整体未完成。
- DEV-080 worker 与生产装配组件已完成；native 浏览器未取得真实平台验收。`saya-core::browser::worker` 的 `PlaywrightMcpProvider` 固定 `@playwright/mcp 0.0.85`、`playwright`／`playwright-core 1.66.0-alpha-2026-11-09`、Chromium `158.0.8127.18`（revision 1254），使用宿主显式提供的绝对 Node、worker root、Chromium、身份 manifest 与摘要；启动前以 `O_NOFOLLOW`、锁定目录句柄、常规文件、权限、大小与 SHA-256 校验 manifest 命名的 Node、`cli.js`、三个 `package.json`、`browsers.json` 和 Chromium 主程序并核对精确版本，不声称验证整个 worker 树。真实启动经 `PreparedEnvironment::prepare_command` 与 `ManagedProcess`，stdin／stdout 交给 `McpClient::connect_stdio`，stderr 有界排空并保留有界尾部；固定 config／argv 使用 `chromiumSandbox: true`、`--sandbox`、headless、isolated、block service workers、no WebMCP、ignore roots、`imageResponses: omit`、固定 output dir 与 Chromium executable，不使用固定远程调试端口或 `npx`／`npm`。`worker.proxy` 只校验为无凭据、显式端口的 loopback HTTP 地址并留给 DEV-081，不注入 launch options／argv；浏览器 environment 网络固定 Deny。初始化协议为 `2025-11-25`，目录校验七个必需工具及 pinned schema，只内部映射 navigate／snapshot／click／type／press_key／wait_for／close；evaluate、run-code、install、devtools、network、console、tabs、file upload 等不进入模型注册，目录 generation 变化使旧调用 stale。worker 文本结果解析 Page URL／Title、YAML snapshot 与 `role`／`name`／`[ref=...]`；open 创建宿主内部 `about:blank`。已派发后的 timeout、断连与崩溃映射 `outcome_unknown`，不自动重放；派发前失败保持已知并清理。`BrowserFacts` 的 screenshots 保持 false，下载／上传由后续 DEV-081 文件 broker 启用。生产装配由 `saya-app::assemble_browser_provider` 完成：`browser.enabled` 必须同时启用 `browser.worker`，安装根固定为 Node 父目录、worker root 与 Chromium 包目录并只读，不绑定 workspace／HOME／`.saya`／凭据；`Host` backend 与 macOS Seatbelt 在构造 provider 前返回 `browser_sandbox_unavailable`，不会启动 worker 或 Chromium。每个 worker 生命周期从 factory 取得全新 `PreparedEnvironment`，profile／output／tmp／work／config 位于 Run 私有 scratch；PLAN 清理或 shutdown 后再次 open 不复用已关闭实例。shutdown 先关闭 MCP、终止并 reap worker／Chromium 进程组，再 `wait_for_process_cleanup` 与 `environment.cleanup`，失败保留可重试状态并拒绝假重启；外层超时丢弃 shutdown future 时，connection 与 workspace 仍由共享句柄保留，重试可继续。首次真实 open（以及 navigate 的幂等复查）后，provider 以 worker PID 为根在宿主 `/proc` 中有界等待 `--type=renderer` 后代，要求 argv 无 `--no-sandbox`／`--disable-setuid-sandbox`、`NoNewPrivs=1`、`Seccomp=2`，并必须取得 Chromium 自有的 `InitializeSandbox` 判别证据；宿主 Bubblewrap 固定赋予的 NoNewPrivs／Seccomp 不能单独作为证据，缺失时返回 `browser_sandbox_unavailable` 并清理。native 浏览器当前在 macOS Seatbelt 与 Linux Bubblewrap 下都无法完成 Chromium 内层 sandbox 初始化，因此不得写成已验收或可用。本机验证：`cargo test -p saya-core --lib` 通过（含 worker 与 vm 子集），`cargo test -p saya-core --test config --test execution_backend` 与 `cargo test -p saya-app` 通过，`cargo fmt --all`、workspace 严格 clippy／check 通过；`browser_worker_real` 为 `#[ignore]` 显式入口，未在本机 darwin 运行真实 Chromium／Playwright 端到端或四平台验收。
- 2026-11-13，按D-066扩展DEV-080专用VM Browser后端，保留宿主与Chromium双层sandbox。新增`[browser.vm]`显式配置与`browser::vm`：Colima专用profile、严格SSH配置与主机key校验、精确只读`ssh`/identity/known-hosts文件、`NetworkPolicy::LoopbackPort`只允许VM SSH端口、远端Run根0700、`setsid -w`独立session/process group与PID文件、远端manifest和七个命名文件的大小/SHA-256/权限/Node实际版本/pinned package/Chromium revision与headless-shell路径校验、有界远端终止与Run根删除、VM内`/proc` renderer证据。`scripts/n5/browser_vm.py`提供显式create/provision/check/destroy，`create`固定`--mount none`与`--ssh-agent=false`，provision只复制显式输入并生成远端manifest，不下载或自动更新版本。本机验证：`cargo test -p saya-core --lib browser::vm` 通过（含真实macOS Seatbelt host policy准备、取消清理、身份不符、远端PID/unknown、远端cleanup确认与重建），`browser::`子集与`browser_assembly`通过，`scripts/n5/test_browser_vm.py` 18项通过。2026-11-13审查整改后新增覆盖：canonicalize 后 identity／known-hosts 路径复验（含符号链接到含换行目标的反向测试）、`[::1]`→`::1` 归一化、身份／prepare／sandbox／cleanup 的 cancelled／outcome_unknown 映射、SSH断连／Drop 清理与无 runtime 时的 unknown、启动失败清理句柄保留与重试、PREPARE_RUN_SCRIPT 创建并保持全部 XDG 目录 0700；Python 工具增加 `.`／`..` 拒绝、生成路径复验、manifest 结构受控错误与 worker-source 目录名校验。Linux browser seccomp 的 mount flags 改读 `args[3]`（offset 40），测试把 flags 放在第四个参数并覆盖合法／非法值。真实Colima VM入口为`crates/saya-core/tests/browser_vm_real.rs`（`#[ignore]`，`SAYA_TEST_VM_*`），本轮以 host `127.0.0.1:58780`、user `ariakage`、profile `soyo-browser` 实际重跑通过。2026-11-13修复后在该专用VM上实际执行通过：远端脚本统一使用POSIX `sh`（dash）可接受的`kill`形式，PID存活校验不再误报；Playwright socket目录改为Run根内较短的`PWTEST_SOCKETS_DIR`并缩短Run根basename，避免超过103字节`sun_path`上限；远端cleanup将已退出/zombie PID视为已消失，按进程组有界清理并删除Run根；provider在`open`/`close`生命周期步骤重新发现并校验worker在浏览器启动后发布的新catalogue，普通工具调用仍对stale catalogue失败关闭。审查整改后的实跑结果：`NoNewPrivs=1`、`Seccomp=2`、sandbox argv合法，运行后远端Run根与worker/Chromium进程均已清理。该结果仅覆盖这一显式provisioned环境，不构成native浏览器、四平台或DEV-081至DEV-083验收。
- DEV-081A 网络阶段最终门禁在 macOS27.2 ARM／Rust1.99.1 完成：`RUST_TEST_THREADS=1 just check` 通过1578项Rust测试、11项条件忽略、格式、workspace全目标严格Clippy和release构建，日志为 `target/n5/dev081-network-final-check.log`；`just test-tooling` 104项通过，日志为 `target/n5/dev081-network-final-tooling.log`，同一release的离线smoke与28组TUI PTY通过，日志为 `target/n5/dev081-network-final-smoke.log`，程序SHA-256为 `8855ffa96536601016362ff1c5d6ed07835b4025298c5cafeaf437d708ea2d98`。真实专用VM的 `browser_vm_real` 与 `browser_vm_network_real` 均实际通过；同一浏览器netns内TCP／UDP原始正控制命中、页面STUN标记与broker拒绝观察得到核验，运行后无Run根、worker、socat或canary残留。该证据不构成完整DEV-081、native浏览器或四平台验收。
- DEV-080 最终门禁在 macOS27.2 ARM／Rust1.99.1 完成：`RUST_TEST_THREADS=1 just check` 通过1556项Rust测试、10项条件忽略、格式、workspace全目标严格Clippy和release构建，日志为 `target/n5/dev080-signoff-check.log`；`just test-tooling` 101项通过，日志为 `target/n5/dev080-signoff-tooling.log`。同一release程序完成离线smoke、文本journey、5项历史审查回归和28组TUI PTY，日志为 `target/n5/dev080-signoff-smoke.log`，程序SHA-256为 `1582518e4a46335a53a2c7ff8677b783faf68d8bac933cbfdd721e875cecfc18`。真实专用VM验收日志为 `target/n5/dev080-final-vm.log`，renderer为 `NoNewPrivs=1`、`Seccomp=2`，运行后无远端Run根或worker／Chromium残留；这些证据不覆盖native浏览器或其他平台。
- 2026-11-13，DEV-080最终复核四项问题修复完成，未提交。`VmWorkerLauncher` 在生成Run根后、首个prepare／launch await之前把`RunnerRemoteControl`注册进launcher持有的pending cleanup，成功时由connection接管并移除，失败、取消或外层丢弃launch future后仍由`ensure_launched`／`shutdown`重试；`retry_pending_cleanup` 改为逐项取队首、确认成功后才移除该项，不再`mem::take`整个队列，外层timeout／cancel／drop保留当前及剩余全部句柄。`CLEANUP_SCRIPT` 在PID文件缺失、为空、损坏或进程已退出时，以唯一Run根做有界`/proc`扫描，终止相关进程或证明不存在后再删除Run根；清理shell使用独立的`soyo-browser-vm-cleanup` argv标签排除自身，避免把argv中含`soyo-browser-vm-<hex>`的worker／Chromium一并排除；`expect_worker`仅作观测，不再返回永久`worker=unknown`。`scripts/n5/browser_vm.py` 的`check`与`provision`统一校验manifest自身为常规文件、≤64 KiB、组／其他不可写，以及七个命名文件均为常规文件、≤512 MiB并核对大小、摘要和权限。验证结果：`cargo fmt --all -- --check`、workspace全目标严格Clippy、`RUST_TEST_THREADS=1 cargo test --workspace --locked --no-fail-fast`（1556项通过、10项条件忽略、0失败）与workspace release构建通过，`target/release/saya` SHA-256为`1582518e4a46335a53a2c7ff8677b783faf68d8bac933cbfdd721e875cecfc18`；`just test-tooling` 101项通过（含22项VM工具回归）。专用Colima VM上`browser_vm_real --ignored --nocapture`实际通过：renderer `NoNewPrivs=1`、`Seccomp=2`、sandbox argv合法（本次`stderr_initialize_sandbox=false`，未作为断言）；运行后`/tmp/saya-runs`为空且无worker／Chromium进程。另在该VM上用同一`CLEANUP_SCRIPT`对无PID文件、忽略TERM的测试进程及其子进程实跑，Run根采用生产形态`/tmp/saya-runs/soyo-browser-vm-<hex>`，结果为`worker=gone`、Run根删除、进程退出，随后确认无远端残留。该结果仍只覆盖这一显式provisioned环境，不构成native浏览器、四平台或DEV-081至DEV-083验收。

- 2026-11-13，完成 DEV-081A Browser 网络阶段（未提交）。新增严格 `[browser.network]`（`allowed_origins`、`public_web`、`localhost_development_origin`、`BrokerLimits`）：启用时空 allowlist 且 `public_web=false` 拒绝启动，`public_web` 仍逐连接执行规范化、完整 DNS、公开地址、内网／元数据与混合答案检查；通用 `BrokerPolicy` 默认仍为 `public_web=false`，`NetworkPolicy` 默认仍为 `Deny`，既有 broker 用户不受影响。新增有界 `NetworkObservation`（最多 256 条，仅 Run／worker、规范化 origin、结果、字节与耗时，CONNECT 标记 `tunneled`）并接入 broker；`browser_navigate` 在派发前核对目标、返回后核对最终来源。VM 浏览器不再使用默认出口：宿主为每个 worker 生命周期启动共享 `RunNetworkBudget` 的 `NetworkBroker`，SSH 通道只增加一条反向 streamlocal 转发，VM 内在 `sudo -n unshare -n` 的私有网络命名空间启用 `lo`、确认无额外路由，由原用户运行 `socat` 桥接 `127.0.0.1:31281`，worker 再 `setpriv` 回原 uid／gid；Chromium 保持自有 sandbox，并固定 proxy、`--proxy-bypass-list=<-loopback>`、禁用 QUIC／非代理 WebRTC UDP、`EXCLUDE 127.0.0.1` 的 host resolver 规则与 `--block-service-workers`。网络隔离失败映射 `browser_network_unavailable`，无默认网络回退；broker、反向转发与 socat 纳入取消、unknown、shutdown 与 Run 根关联清理。真实入口 `crates/saya-core/tests/browser_vm_network_real.rs`（`#[ignore]`）在宿主本地 fixture 上验证静态页、redirect、动态页、metadata 拒绝观测、代理旁路 TCP／UDP canary 零连接、慢页面取消和清理；2026-11-13 在专用 Colima VM 上实际通过（观察条数随环境变化，不作固定断言；shutdown 后无 Run 根／worker／socat 残留）。同一 VM 上的 `browser_vm_real` 在 broker／netns 路径下也实际通过，renderer 为 `NoNewPrivs=1`、`Seccomp=2`。本机验证：`cargo fmt --all -- --check`、workspace 全目标严格 Clippy、`cargo check --workspace --all-targets --locked`、`RUST_TEST_THREADS=1 cargo test --workspace --locked --no-fail-fast`（69 个测试二进制、退出码 0）与 `just test-tooling` 通过；并行运行 workspace 测试时 `saya-cli --test chat` 曾因时序竞争失败一次，单独及串行运行均通过。DEV-081A 审查修复（2026-11-13，未提交）：`BrowserNetworkFactory` 改为每个 Run 只创建一个共享 `RunNetworkBudget`，worker 生命周期与 broker 重启共用累计 bytes／connections 与固定 deadline，重启不会重置已耗尽上限；最终 URL 校验允许合法 fragment（OAuth 式 redirect），仍只接受 HTTP(S)、无 userinfo、有效 host，并把 `about:blank`／`chrome-error://` 作为明确的非网络状态，native（`network.enabled=false`）不再执行无关 origin cross-check；`NetworkObservation` 对 malformed／oversize 请求以未知 origin 记录（不伪造），CONNECT 中途 timeout／drop 读取共享计数报告已计费 bytes；`scripts/n5/browser_vm.py` 的 provision／check 新增 `sudo -n`、`unshare -n`、`socat`、`setpriv`、`ip route`、user namespace／AppArmor 前提验证，并提供仅专用 VM 的 `--configure-apparmor-userns`；真实网络验收改为在浏览器 worker 的实际私有 netns 内启动 TCP／UDP canary：先从 broker observation 取得 worker PID，用 `sudo -n nsenter -t <pid> -n` 启动 canary，再从同一 netns 用原始 TCP／UDP 正控制确认可达并重置计数；动态页通过子资源与真实 STUN 尝试并在 snapshot 中留下 `STUN_STARTED`／`STUN_DONE` 或失败标记，零命中才是有意义的负控制；启动失败在终止进程后有界 drain stderr，固定 sentinel 映射 `browser_network_unavailable`。真实 VM 复验：`browser_vm_network_real` 以 worker netns TCP／UDP canary 正控制执行通过（观察条数不作固定断言、canary 零命中、无 Run 根／worker／socat／canary 残留），`browser_vm_real` 同环境通过（renderer `NoNewPrivs=1`、`Seccomp=2`）。登录、文件 broker、截图媒体与后台阶段仍属 DEV-081 后续，DEV-082／DEV-083 未开始，N5 未完成。
- 2026-11-13，DEV-081A 最新复核问题修复完成（未提交）。真实网络验收不再把 canary 放在 VM 默认 namespace：测试先从 broker observation 取得 worker PID，用 `sudo -n nsenter -t <pid> -n` 在浏览器实际私有 netns 启动 TCP／UDP canary，再从同一 netns 用原始 TCP／UDP 正控制确认命中并重置计数；动态页以子资源和真实 `RTCPeerConnection` 尝试 canary，并在 DOM 中记录 `STUN_STARTED`、`STUN_DONE`／`STUN_FAILED`，snapshot 断言脚本确实执行；清理与 leftover 扫描覆盖 Run 根、worker、socat 与 canary。预算耗尽／固定 deadline 到期改用独立 `browser_network_budget`，不再与隔离能力失败的 `browser_network_unavailable` 混淆。`NetworkObservation` 新增 `cancelled`：Run 取消／shutdown 与真实 `timeout` 分开，固定 deadline 记为 `limit`；监听器容量与共享预算拒绝即使 origin 未知也会观察，新增回归覆盖。最终 URL 用 URL parser 接受 `about:blank` 的合法 query／fragment，`chrome-error:` 明确分类，HTTP(S) 的 userinfo 拒绝不变；CONNECT 注释与 `docs/configuration` 对宿主 SSH／broker 双端口和 VM 内 `31281` 的说明已修正。新增 `-R`、`ClearAllForwardings=no`、`ExitOnForwardFailure`、`StreamLocalBindUnlink` 的纯单元测试；`scripts/n5/browser_vm.py` 的 provision／check 新增 `nsenter`／`sudo -n nsenter` 与 `sshd -T` 反向 streamlocal forwarding 验证，sshd 拒绝转发的固定诊断在启动时仍分类为 `browser_network_unavailable`。验证：`cargo fmt --all`、`cargo test -p saya-core --lib`（615 项）、`just test-tooling`（25 项）通过；专用 Colima VM 上 `browser_vm_network_real --ignored` 与 `browser_vm_real --ignored` 均实际通过（renderer `NoNewPrivs=1`、`Seccomp=2`），运行后 `/tmp/saya-runs` 为空且无 worker／socat／canary 进程。观察条数随环境变化，不作固定断言。登录、文件 broker、截图媒体与后台阶段仍属 DEV-081 后续，DEV-082／DEV-083 未开始，N5 未完成。
- 2026-11-13，完成 DEV-081 登录凭据槽、文件 broker、媒体生命周期、后台取消与 unknown 提交（未提交）。新增严格 `[browser.credentials]`／`[browser.files]` 与 `BrowserCredentialBroker`、`BrowserArtifactStore`：slot 绑定别名、`CredentialRef`、精确 origin、`form_fill` 与 `max_uses`，`browser_fill` 只接受 `text` 或 `credential_slot` 二选一；派发前核对 document／page origin／用途／撤销／次数，值只在 provider 边界暴露并注册 `SecretRegistry`，结果、日志与 OutputStore 无明文，填充不自动 submit。`browser_download` 改为显式 page／document／element，只接受该点击产生的 download 事件路径；宿主校验 Run 输出目录、常规文件、大小、类型、magic、SHA-256 与净化文件名，保存为有界 `File` artifact；`browser_upload` 只接受同 Session artifact，解析摘要与归属后复制到 0600 staging，VM 经同一受控 SSH 写入 Run 私有 staging 并调用固定 `browser_file_upload`，动作后清理。`MediaKind::File` 可保存／读取／GC pin，但永不注入模型；`CapturedToolResult.media` 持有 lease 至提交，`ToolResult.media_refs` 由宿主写入并纳入 `media::history_references`，页面／工具正文伪造引用不产生 pin，恢复缺失返回 `media_unavailable`。`browser_navigate`／`wait`／`download` 支持 `background=true`，复用 `BackgroundJobs` 的真实 job ID、并发／超时／PLAN／Run 清理，`background_status`／`cancel` 可用；已派发取消为 `outcome_unknown` 且不重放。真实入口 `crates/saya-core/tests/browser_vm_files_real.rs`（`#[ignore]`）在专用 Colima VM 上实际通过：表单凭据填充不自动提交、错误 origin／stale document／次数耗尽、实际下载并写入 artifact、实际上传 `downloaded bytes`、路径穿越拒绝、后台慢导航取消为 unknown、snapshot 恢复、重新打开 OutputStore 后引用可读，shutdown 后 `/tmp/saya-runs` 为空且无 `soyo-browser-vm-` 进程。同环境 `browser_vm_real` 与 `browser_vm_network_real` 继续通过；运行后 `/tmp/saya-runs` 为空且无 `soyo-browser-vm-` 进程。本机验证：`cargo fmt --all -- --check`、workspace 全目标严格 Clippy、`cargo check --workspace --all-targets --locked`、`RUST_TEST_THREADS=1 cargo test --workspace --locked --no-fail-fast`（1592 通过、0 失败、12 条件忽略）、`just test-tooling`（104 项）与 workspace release 构建通过，`target/release/saya` SHA-256 为 `1ae53823ed9383ff4e3485d38a2602d85e423c7a6e4fbfeab660c508a46affe9`。worker／VM 的 `screenshots` fact 为 false，`browser_screenshot` 当前不注册／不可用；图片 artifact 代码路径、模型图片与界面属 DEV-082，联合验收属 DEV-083。

- 2026-11-13，DEV-081B 审查问题修复完成（未提交）。凭据 slot 仅接受 HTTPS，显式回环开发 origin 例外允许 HTTP；`BrowserElement` 按 snapshot 树结构保留 frame 身份，`fill_credential` 只接受顶层 document 元素，任何子 frame（含 `fNeM` 引用）返回 `credential_frame_denied`；`[browser.credentials]` 启用且声明 store 时，Host 后端即使 `browser.enabled = false` 也拒绝启动，其他后端把 store 加入受保护路径。后台 browser job 使用父调用派生的 child id，与 retired output ACL 父子关系一致；下载 artifact 的 `SealedMedia` lease 保存在 job 状态中，`background_status` 重投影保留合法 `media_refs`，GC 在状态查询／上传前不能回收；后台结果在写入 job 状态前经过 dispatcher 的动态 SecretRegistry 脱敏（覆盖页面回显到 filename／final_url），`BrowserState` 改为弱引用后台 supervisor 并在 shutdown 解绑；浏览器业务失败（invalid URL、network deny、stale、类型／大小、unsupported、取消／超时）只形成 job failed／cancelled，不再触发 supervisor fatal。`media::load_history` 只加载 `UserText` 中实际存在的 Image／Audio／Video 引用，`ToolResult.media_refs`（含 File）仅作 GC pin，不预加载或 base64 展开。下载路径只从固定 `### Download` 事件段落或下载目录的新增文件证明，页面 title／snapshot 文本中的伪事件、重复事件、旧文件与目录外路径拒绝；上传清理失败返回 `outcome_unknown`，staging 写入／同步失败删除半成品。文档明确 worker／VM 的 `screenshots` fact 为 false、`browser_screenshot` 当前不注册不可用。验证：`cargo fmt --all -- --check`、workspace 全目标严格 Clippy、`cargo check --workspace --all-targets --locked`、`RUST_TEST_THREADS=1 cargo test --workspace --locked --no-fail-fast` 与 `just test-tooling` 通过；专用 Colima VM 上 `browser_vm_files_real --ignored` 实际通过（含子 frame 拒绝、后台下载 GC 后上传、后台结果脱敏与清理），同环境 `browser_vm_real` 与 `browser_vm_network_real` 继续通过。
- 2026-11-13，DEV-081 最新复核问题修复完成（未提交）。snapshot ARIA 解析改为有界行解析器：只从行尾 props 区读取 `ref`，accessible name 内的伪 `[ref=...]`、引号／转义与 YAML 单引号 key 不再被当作元素身份；只接受 pinned provider 的 `e<digits>`／`f<digits>e<digits>`，frame／容器／元素 ref 非法、重复或无法识别时整份 provider snapshot 返回 `invalid_provider_result`，不再静默丢子树；frame 身份继续按结构保留，任何子 frame 凭据填充仍为 `credential_frame_denied`。`BackgroundJobs` 的 browser `tool_timeout` 分支在 deadline 之后观察到完成的 `Ok` 时，把 `SealedMedia` lease 放入 job 状态并保留 observed result，failed job 期间 artifact 不被 GC，job 状态释放后才可回收。`BrowserState` 保留弱后台绑定并在显式 `browser_open` 时幂等重绑同一 Run 的存活 supervisor，已关闭的 jobs 不复用；PLAN 清理期间 `background=true` 明确失败，回到 WORK 后真实后台动作可用。新增 BrowserState lifecycle 取消信号，PLAN 清理／Run shutdown 先唤醒在途 `wait`／navigate 再取页面锁，不必等满 wait deadline，且不取消 cleanup 自身的 close／shutdown。`provider.download` 的任何错误都作废 document：已知失败（`download_missing`、`invalid_provider_result`、`file_path_denied`、`file_too_large`、`file_broker`）保留新 snapshot 可重试，unknown 仍为 unknown。worker 上传 staging 改为唯一私有目录下的原名 basename（不再添加合成前缀），create／write／sync／chmod／mv 失败或取消时由 guard／trap 清理半成品；VM `UPLOAD_SCRIPT` 增加 trap 与不覆盖已有目标，页面 multipart 实际收到原名。验证：`cargo fmt --all -- --check`、workspace 全目标严格 Clippy、`cargo check --workspace --all-targets --locked`、`RUST_TEST_THREADS=1 cargo test --workspace --locked --no-fail-fast`（70 个测试二进制，1616 通过、0 失败、12 条件忽略）、`just test-tooling`（104 项）与 release 构建通过，`target/release/saya` SHA-256 为 `9f957c3c19d253cf1a157457f61249920b1aaa499a701cdc8c3e7efd81f72f17`。专用 Colima VM 上 `browser_vm_files_real --ignored` 实际通过新增覆盖（伪 ref 名称、`download_missing`／`file_too_large` 后旧文档失效、上传原名、超时 artifact lease、PLAN 清理后 open 再后台导航、慢导航 unknown 与最终清理），同环境 `browser_vm_real`（renderer `NoNewPrivs=1`、`Seccomp=2`）与 `browser_vm_network_real` 继续通过；运行后 `/tmp/saya-runs` 为空且无 worker／Chromium／socat 残留。该结果仍不构成 native 浏览器、四平台或 DEV-082／DEV-083 验收，N5 未完成。
- 2026-11-13，完成 DEV-082 Browser CLI／TUI 与模型能力适配（未提交）。`BrowserFacts` 新增 `screenshot_masking`；pinned `@playwright/mcp 0.0.85` 的 `browser_take_screenshot` schema 没有 `mask` 输入，worker／VM 两项截图 fact 均为 `false`，`browser_screenshot` 不注册，直接调用返回 `browser_capability_disabled`，provider 即使返回字节也因 `masked=false` 被拒绝，不持久化未遮蔽图片。具备真实遮蔽能力的 provider 由宿主生成文件名并保存有界 Image artifact，结果包含 page、url、document_revision、mime、filename、bytes、sha256、masked、artifact_ref 与 model_image_status；内部 `media_ref` 不再序列化到模型或界面，`ToolResult.data` 无 base64。`media::load_history` 现在只把宿主 `ToolResult.media_refs` 中 Image 类型对象纳入快照（File／工具音频／视频记录为 omitted，能力关闭时也不加载）；Chat Completions 在 tool 消息后附加相邻 `image_url` user 内容，Responses 在 `function_call_output` 后附加 `input_image` user 内容，Anthropic Messages 把 base64 `image` block 放入同一 `tool_result`；tool 文本始终使用不含 `media_refs` 的 `model_view`。图片关闭时三协议 wire 无 image block。Compact 投影保留 `media_refs`，`history_references` 继续 pin，重新打开 OutputStore 后可读取并从历史重建注入。事件流使用 `model_view` 去除宿主引用。TUI 新增集中 `browser` i18n，摘要／详情显示 action、URL、page／document revision、output_ref、artifact mime／size／SHA-256、masked、后台 job 与 unknown；审批弹窗显示具体动作与 operation summary，填充文本只显示字节数。CLI 审批同样显示 operation summary、动作、URL／slot 与填充字节数。新增 `browser` 预览场景和真实 PTY 测试（详情渲染、SIGTERM 取消、alternate-screen 恢复）。验证：`cargo test -p saya-core --lib` 650 项、`cargo test -p saya-core --tests` 全部通过（含三协议新增 wire 测试与截图禁用／GC／恢复测试）、`RUST_TEST_THREADS=1 cargo test --workspace --locked --no-fail-fast` 全部通过、`cargo fmt --all -- --check`、workspace 严格 Clippy 与 `cargo check --workspace --all-targets --locked`、`just test-tooling` 与 release 构建通过；`just smoke target/release/saya` 通过 28 组 TUI PTY，release SHA-256 为 `9df63ed8af6ac67f9e7f260e80bd282a416969bbd0b3817138557df27702f24c`。`browser_vm_files_real` 已扩展“无遮蔽能力必须拒绝截图”断言，但本轮未重新执行专用 VM；该结果不构成四平台、native 浏览器或 DEV-083 验收，N5 未完成。
- 2026-11-14，DEV-082 审查问题修复完成（未提交）。首次整改为计数前预载、历史投影变化后重载有界快照；下述最终复核已改为仅在有效投影后装载并共享。最多 8 个工具 Image，聚合原始字节与 base64 均不超过 64 MiB，同一引用只计一次，三协议在合并多个工具结果的 wire 消息时按引用去重；超预算引用记为 omitted 并继续由完整历史的 `history_references` 提供 GC pin，真正选中但缺失的引用明确返回 `media_unavailable`。截图注册现在同时要求 `screenshots`、`screenshot_masking`、文件 broker 与 artifact store，不能只检查 facts；`model_image_attached` 改为 `model_image_capability`（`enabled`／`disabled`），`model_image_status` 只描述本次截图时的 profile 能力声明。普通 transcript 与事件继续使用 `model_view()`，Full 导出只保留语法校验通过的 `media_refs`，TUI／普通导出不显示内部 `media_ref`。CLI chat 与 `saya run --interactive` 共用 `approval` 渲染：`browser_fill` 的 text 只显示字节数，其它 browser 动作显示 page／document／element／url／key／button／milliseconds／artifact／credential slot，未知字段只列名，终端控制字符净化；TUI 详情按真实 `data.screenshot`／`download`／`upload` 嵌套字段读取 page、document_revision、bytes_sent，并保留 snapshot 的 inline text／preview、动作 detail／document_stale 与 artifact size／SHA-256。文档明确固定 worker 的 `screenshots=false` 是缺少截图生成前的真实遮蔽契约，不是 DEV-082 图片功能未实现；生产截图仍不伪造，受控遮蔽 provider 的成功／能力开关路径由离线测试覆盖。验证：`cargo fmt --all -- --check`、workspace 全目标严格 Clippy、`cargo check --workspace --all-targets --locked`、`RUST_TEST_THREADS=1 cargo test --workspace --locked --no-fail-fast`（70 个测试二进制，1642 通过、0 失败、12 条件忽略）、`just test-tooling` 通过；workspace release 构建与 `just smoke target/release/saya`（离线 smoke、journey、5 项审查回归和 28 组 TUI PTY，含 pasted attachments 场景）通过，`target/release/saya` SHA-256 为 `7a6a9019c2656a12855895553f935c8a2132ba569b1e22e94a666b9fecf8fa30`。专用 Colima VM 上 `browser_vm_files_real --ignored --nocapture` 实际通过（截图能力关闭与 `browser_capability_disabled` 拒绝、下载／上传、后台 artifact 跨 GC 上传、Run 清理，85.15s）；运行后无 Run 根或浏览器进程残留。该结果不构成四平台、native 浏览器或 DEV-083 验收，N5 未完成。
- 2026-11-14，DEV-082 最终复核与本机门禁完成。媒体只在有效 Context 投影后装载，局部单槽快照在 provider 计数、输出目标适配与最终派发之间共享；checkpoint／retirement 已移除的缺失旧图片不再阻断请求，当前实际使用的缺失引用仍明确失败。open 保存已验证 URL，navigate／click／fill／press／wait 在同页锁内完成结果验证与缓存更新，缺失 URL／失败／unknown 清空来源缓存；截图用 `url_observation = last_confirmed` 明确区分最近确认值与截图瞬时来源。CLI `/job` 的完成和失败观测结果均使用 `model_view`，TUI 修复重复 output_ref、MIME 和真实预览覆盖；公开 `artifact_ref` 可与宿主存储引用取同一值，但不授予跨 Session 权限，宿主字段不额外输出。定向独立复核未发现新增实质缺陷。`RUST_TEST_THREADS=1 just check` 通过 70 个测试二进制、1650 项 Rust 测试、0 失败、12 项条件忽略、格式、全目标严格 Clippy 与 release 构建，日志 `target/n5/dev082-signoff-check.log`；此前并行压力下超时的 CLI 测试串行独立 21 项通过，日志 `target/n5/dev082-signoff-chat.log`。工具回归 118 项通过（包含当时工作区中的联合验收工具用例），日志 `target/n5/dev082-signoff-tooling.log`；同一程序通过 smoke、journey、5 项审查回归与 28 组 TUI PTY，日志 `target/n5/dev082-signoff-smoke.log`。release SHA-256 为 `86c08738d627d3b54314957675f82f1e23398fab212b4a02fe1410a9f82a0135`。同一源码的 `browser_vm_files_real`、`browser_vm_network_real`、`browser_vm_real` 三个真实 VM 入口实际重跑通过，日志 `target/n5/dev082-signoff-vm.log`；renderer 为 `NoNewPrivs=1`、`Seccomp=2`，同 netns TCP／UDP canary、文件／后台及截图拒绝边界得到验证，验收后无 Run 根／worker／Chromium／socat 残留。固定 worker 的生产截图仍禁用；本轮不代替 DEV-083 联合验收或新增四平台证据。
- DEV-079 最终本机门禁在 macOS27.2 ARM／Rust1.99.1 完成：`RUST_TEST_THREADS=1 just check` 通过1489项Rust测试、8项条件忽略、格式、workspace全目标严格Clippy和release构建，日志为 `target/n5/dev079-signoff-check.log`；`just test-tooling` 81项通过，日志为 `target/n5/dev079-signoff-tooling.log`。同一release程序完成离线smoke、文本journey、5项历史审查回归和28组TUI PTY，日志为 `target/n5/dev079-signoff-smoke.log`，程序SHA-256为 `cd548dcfaf50e707b5fe721c0e0c757b75b776b1f0145cc93f9dd520daeb0255`。这些证据不覆盖真实Chromium或其他平台。
- DEV-081 最终本机门禁在 macOS27.2 ARM／Rust1.99.1 完成：`RUST_TEST_THREADS=1 just check` 通过1616项Rust测试、12项条件忽略、格式、workspace全目标严格Clippy与release构建，日志为 `target/n5/dev081-final-check.log`；`just test-tooling` 104项通过，日志为 `target/n5/dev081-final-tooling.log`，同一release完成离线smoke与28组TUI PTY，日志为 `target/n5/dev081-final-smoke.log`，程序SHA-256为 `9f957c3c19d253cf1a157457f61249920b1aaa499a701cdc8c3e7efd81f72f17`。真实VM的 `browser_vm_files_real` 与 `browser_vm_real` 均实际重跑通过并保存独立日志 `target/n5/dev081-final-files-vm.log` 和 `target/n5/dev081-final-worker-vm.log`；运行后无Run根、worker、Chromium或socat残留。该证据不构成native浏览器、四平台或DEV-082／DEV-083验收。
- 2026-11-18，`docs/reviews/review-2026-11-18.md` 全量审查问题修复完成并按发现分十项提交（`456b5b1` 至 `0231b1b`，含审查报告共十一项，未推送）。F-01 P0：文件工具与原生沙箱新增 Git 控制路径写保护（`.git` config/hooks/info、linked-worktree gitdir、子模块管理目录及可写根内 HOME 隐式执行文件），仓库控制数据保持可读、载荷不可写，端到端复现确认 `.git/config` 改写与 `core.fsmonitor` 利用链失效。F-02：Anthropic 流式 text/thinking/signature 改原地追加。F-03：`build_runtime`、会话存储打开、目标指纹、Browser 启动准备与 worker 身份哈希移至阻塞线程并经取消旗标桥接，current_thread 运行时的 Ctrl-C 与 TUI 刷新不再被阻塞。F-04：模块 SQLite 空库/未初始化在 store 锁内自愈建表。F-05：JSON/JSONL 输出保留 `BoundaryError` 原始错误码并按 B-17 区分退出码（`session_busy` → code `session_busy`、退出码 1），日志查询新增 `invalid_query` 稳定码。F-06：精确预授权在 Ask/ModelReview 之前生效，不再被模型核查推翻或多余消耗请求。F-07：`cargo-audit` 固定 0.22.2，N5 worker 依赖改用官方 registry lockfile + `npm ci`，工具回归统一走 `scripts/test-tooling.sh`。F-08：移除预导出 `#[allow(unused_imports)]`，测试专用名改 `#[cfg(test)]` 重导出，`create_goal` 消除 `unreachable!`。F-09：Mermaid 解析/布局经 unwind 兜底，异常降级为源码显示。F-10：本文件分卷为当前状态加 `progress/` 四卷历史证据。验证：macOS ARM 本机 `just check` 全量通过（格式、workspace 全目标严格 Clippy、77 个测试二进制 0 失败、doc-tests、release 构建），`just test-tooling` 12 套件全过，F-01 直接写复现返回 `permission_denied` 且未产生标记文件；未运行四平台 CI、Linux 原生路径实测或真实模型。
- 2026-11-21，完成 D-073 helper 后端迁移。新增 `crates/soyo-ipc`（NDJSON 帧、`ClientFrame`/`ServerFrame` 信封、`HelperClient`、socket/端点、全部 IPC DTO）与 `crates/soyo-host`（`HostService` 运行时服务、`HelperServer` socket 派发、附件与 prefs），`soyo-desktop` 不再进程内链接 `soyo-app`/`soyo-core`，改为 `client_state.rs` 薄客户端 + `soyo-attachment://` 协议代理；`soyo helper` 前台守护、`soyo kill` 优雅关闭（pid 文件兜底）、CLI TUI/chat/resume 入口与 desktop 首次运行时 `connect_or_spawn` 自动拉起；桌面命令面与 `bindings.ts` 未变。原 `runtime_bridge` 集成测试迁至 `soyo-host`（10 项通过），新增 NDJSON 帧往返/畸形帧/EOF 与真实 socket 往返、未知方法、停机与过期 socket 回收测试。CI 新增 `desktop-frontend`（Ubuntu：pnpm install --frozen-lockfile + typecheck + vitest + build）；打包冒烟仍归 `desktop-release.yml`。CLI/TUI 的 run 循环迁移到 helper 属后续阶段。
- 2026-11-21，完成 D-074 `soyo run` socket 迁移。新增 `run_prompt` 流式方法：`HostService::run_prompt` 复用 `start_run_ex` 运行时路径，`RunPromptParams` 承载逐次覆盖（cwd/profile/mode/permissions/approval-file/interactive/personal-context/anthropic-prompt-cache，不改写共享守护配置）；`DesktopStreamEnvelope::RawSchemaEvent`（`#[specta(skip)]`）直通 schema-v1 `Event`，`--output jsonl` 与进程内 `StdoutSink` 逐字节一致；合成 `session.created`/`session.resumed` 首行、边界错误 `emit_failure` 终止事件、`RunCompleted.needs_input_question` 覆盖 text 澄清输出；`--interactive` 走 socket 审批/澄清往返（`reply_input` 支持拒答），headless 不挂交互句柄由运行时自身产出 `needs_confirmation`/`needs_input`；`--config`/`--data-dir` 与 helper 不可用/启动前拒绝时回退进程内路径。本机验证：真实 `soyo run --output jsonl` 经自动拉起的 helper 输出完整 schema 事件流（session.created→run.finished 序列正确）、text/json 退出码正确、`--session` 恢复事件正常、`soyo kill` 正常；`cargo test -p soyo-host` 15 项通过（新增 raw 事件流/headless needs_confirmation/interactive 审批往返 3 项）、`soyo-cli` 187 项、`soyo-ipc` 3 项、前端 typecheck+vitest 14 项全绿；`cargo clippy --all-targets` 零警告。TUI/chat/resume 交互循环迁移仍属后续阶段。
- 2026-11-21，交互式 `soyo chat`/`soyo resume` 迁移到 helper（D-074 第二阶段，未含 TUI）。IPC 侧：`submit_prompt` 参数升级为 `SubmitPromptParams`（`session_id` 会话钉住——helper 无 prepared 时按需 resume、不一致时报 `session_changed` 而非写错会话；`raw_events` 原始事件直通；`capture_media` 复刻 chat 的 `capture_user` 捕获语义：改写文本入历史、原始文本作运行时 summary；`personal_context`/`anthropic_prompt_cache` 逐次覆盖）；新增 `RunStateDto`（mode/pending/compaction_pending/运行中 revision）、`ResumeSessionDto`（接受 `accept_config_change`/`cwd_explicit`/`profile_explicit`/`mode_override` 全量恢复选项）；`new_session` 接受 `cwd`/`profile` 覆盖以钉住会话工作区与 profile；新增 `get_run_state`/`get_job`/`get_session`/`read_session_snapshot`/`cancel_job` 方法。`raw_events` 语义调整为「原始事件直通 + 辅助通道常开」——Thinking/Context 信封在 raw 模式下也推送（交互式客户端需要），仅 SchemaEvent 投影被抑制；headless `soyo run` 消费者忽略多余信封，线缆契约不变。CLI 侧：`chat_via_helper` 完整复刻进程内 chat 循环——Console/信号/斜杠命令不变，`submit_prompt` 流式调用驱动 Remote（原始 `model.text_delta` 即时打印、`ApprovalNeeded`/`InputNeeded` → `reply_approval`/`reply_input` 往返、`RunCompleted` → `report_result_inner` 收尾）；`/compact` 中跑时经 `compact_session` 返回 `compaction_queued`、`/mode` 经 `set_work_mode`（服务端已处理 stale approval）、`/permissions` 经 `set_permission_mode`、`/jobs`/`/job`/`/cancel-job` 经 `list_jobs`/`get_job`/`cancel_job`；SIGINT/SIGTERM 经 `cancel_run` 转发，两次 Ctrl-C 仍强制退出。`--config`/`--data-dir` 与 helper 不可达时回退进程内 `chat()`（与 `soyo run` 同契约）——22 项 chat 集成测试全部经 `--config` 走进程内路径保持通过。本机验证：`cargo test -p soyo-cli` 全部通过（含 22 项 chat PTY 测试）、`soyo-host` 16 项（run_prompt raw 测试断言更新为「不泄漏 SchemaEvent」）、`soyo-ipc` 3 项、`cargo clippy`/`fmt` 零警告。TUI 交互循环迁移属下一阶段：`App.prepared`/`Active.handle` 约 50 处进程内触点需改造为 RemotePrepared/RemoteHandle 异步后端。
- 2026-11-21，完成 D-074 TUI 迁移（第三阶段，helper 后端 seam 落地）。`crates/soyo-tui/src/controller/backend.rs` 新增 `SessionBackend::{Local,Remote}`（`RemotePrepared` 镜像 helper 会话——`get_session`/`read_session_snapshot`/`set_workspace` 回写镜像）与 `ActiveHandle::{Local,Remote}`（`RemoteHandle`：`mode`/`compaction_pending` 从 `RemoteRunState` 缓存读，`request_mode`→`set_work_mode`、`request_compaction`→`compact_session{raw_events:true}`、`steer`→`submit_prompt`（mid-run 返回 `steered`）、`cancel_background`→`cancel_job`、`cancel_run` 即发即弃）；`spawn_remote_run` 把 `DesktopStreamEnvelope` 翻译回 `Active` 既有通道——`RawSchemaEvent`→`Event`（经 `SummarySink` 同构转发）、`Thinking`→`ThinkingLog` watch、`Context`→`ContextSnapshot` watch、`ApprovalNeeded`/`InputNeeded`→`Request` oneshot→`reply_*` 回话、`RunCompleted`→终态＋compaction_pending 缓存；`mode.*` 原始事件更新模式快照（线缆不推 `ModeState` 信封）。controller 触点改造：`start_run` 远程分支（`submit_prompt{raw_events,capture_media,session_id 钉住,personal_context,prompt_cache}`）、`/new`→`new_session{cwd,profile}`＋`RemotePrepared::load`、`/resume`/`/resume-accept`→`resume_session{ResumeSessionDto}`、`/sessions`→`list_sessions_page`（`PickerSession` 统一本地/远程渲染）、`/tools`/`/jobs`/`/export*`→`read_session_snapshot`（`remote_session_snapshot` 共享）、`/cd`→`set_workspace`、`/profile`/`/model`→`release_session`＋下次 `/new` 重建、`/mode`→`set_work_mode`、`/compact`→`compact_session`（idle 起远程压缩 run 或 mid-run `compaction_queued`）、`/permissions`→`set_permission_mode`、`/effort`→`set_thinking_effort`（失败回滚本地 profile）、`/sandbox`/`/autocompact`→配置同步、`/memory set`→`set_memory_setting`、memory query/prepare/confirm→`query_memory`/`prepare_memory_action`/`confirm_memory_action`（`MemoryPending::{Local,Remote}`）、`/jobs` 远程用 `BackgroundJobDto` 行渲染与本地 `BackgroundEvent` 输出同构。host 侧：`compact_session` 接受 `raw_events` 参数（桌面 `{}` 不受影响）、`RunCompleted.compaction_pending`、`release_session`（`/profile` 释放 helper 会话锁）、`set_thinking_effort` 指纹持久化（失败回滚 profile 设置）、`set_memory_setting`、`SessionSnapshot` 补 `Deserialize`。CLI 入口：`Options::use_helper` 在 `--config`/`--data-dir` 时关闭，`soyo_tui::run` 自动 `connect_or_spawn`、不可达时回退进程内后端。验证：`cargo check --all-targets` 零警告、`cargo clippy --all-targets` 零警告、`cargo fmt --check` 干净；测试因 `/Volumes/Akari` 磁盘满改以 `CARGO_TARGET_DIR=/tmp` 全量执行——`soyo-tui` 318 项、`soyo-cli` 187 项（含 22 项 chat PTY）、`soyo-host` 16 项、`soyo-ipc` 3 项、`soyo-desktop` 1 项、`soyo-core` lib 736 项全部通过（chat 的 `running_background_command_can_be_cancelled_from_chat` 与 core 的 `hook_store` 租赁测试各出现一次并行时序 flake，单测与全量重跑均通过，与本次改动无关）；specta 绑定再生成补出 `compaction_pending` diff 已同步。真实 PTY 端到端冒烟通过（假 OpenAI 端点）：`soyo` TUI 经 `connect_or_spawn` 自动拉起 helper、Hello 握手正常、键入 prompt 经 `submit_prompt{raw_events}` 流式执行（`raw_schema_event`/`context`/`thinking`/`run_completed` 信封齐全）、helper 持有的会话历史同时含 TUI 输入与模型应答（证实 Remote 后端而非本地回退）、Ctrl-D 退出码 0、`soyo kill` 优雅停机无进程残留。未覆盖：真实模型、跨平台、`target/n5` 109G 旧验收产物待清理后恢复主 target 全量门禁。
- 2026-11-22，完成 D-074 收尾：`soyo compact` 迁移到 helper 并修复会话锁死锁。CLI 侧 `compact_via_helper`：`--config`/`--data-dir` 或 helper 不可达时回退进程内 `compact_session`（与 run 同契约）；helper 路径先 `resume_session`（`ResumeSessionDto` 全量选项）钉住目标会话，再 `compact_session{raw_events:true}` 流式——JSONL 逐行原始事件、text/json 从 `run.finished` 还原（`report_result_inner`/`report_json`/`status_code` 与 run 同一套）、启动前被拒（resume 失败或 compact 拒绝且未发事件）回退进程内；SIGINT/SIGTERM 经 `cancel_run` 转发。host 修复：`run_prompt` 对 `session_id` 无条件 `session::resume`，helper 已 prepared 同会话时撞上自身持有的会话锁——`soyo run --session` 连续调用必然 `session_busy`；现同 id 先释放 prepared 再 resume，保留覆盖参数语义，新增 `run_prompt_reuses_held_session_lock_for_same_session_id` 回归。验证：假 OpenAI 端点 + 隔离 HOME 的真实二进制端到端——`soyo run` 自动拉起 helper 建会话、四次连续 `--session` 追加成功、`soyo compact --output jsonl` 经同一 helper 压缩完成（`run.finished: completed`）、text 模式与坏 session 错误契约一致、helper 持锁时 `--config` 进程内回退如实报 `session_busy`、`soyo kill` 后进程内恢复可用、零进程残留；`cargo test -p soyo-host` 17 项、`soyo-cli` 187 项、clippy 零警告、fmt 干净。
- 2026-11-21，修复 helper 孤儿进程泄漏。问题记录：测试等场景下 `connect_or_spawn` 拉起的 detached `soyo helper` 绑定 `SOYO_HELPER_DIR` 临时目录 socket，临时目录被清理后 socket 文件消失，helper 成为永远不可达的孤儿进程（一次 `cargo test -p soyo-cli` 实际观察到 46 个残留）。修复：`HelperServer::run` 绑定后启动看门狗，每 1s 比对 socket 路径的 `(dev,ino)`——文件被删除或被新实例重绑（inode 不匹配）即置 shutdown 标志走正常停机路径退出；Windows named pipe 无文件路径，不受影响。新增 `server_exits_when_socket_file_removed` 回归测试（删除 socket 文件后 5s 内退出），`cargo test -p soyo-host` 16 项通过，clippy 零警告。
- 2026-11-18，完成 D-072 `soyo-desktop`（F-14 GUI）全部六个阶段本机实现。顶层 `desktop/`（React + Vite + TypeScript + Tailwind + Radix + zustand + Streamdown/Shiki/Mermaid + `lucide-react`）与独立 workspace member `desktop/src-tauri`（`soyo-desktop`）直接进程内复用 `soyo-app` 与 `soyo-core`，并保持根 `Cargo.toml` 的 `default-members` 与 `crates/` 核心边界隔离。已交付：tauri-specta 类型绑定、schema v1 `tauri::ipc::Channel` 流式事件／取消／steer、审批与澄清 `oneshot` 往返、工作／权限模式切换与会话恢复、设置／模块开关／脱敏 MCP 与凭据管理／两阶段全局记忆管理／会话导出、受限 `soyo-attachment://`（兼容 `saya-attachment://`）协议与 `nosniff`/`no-store` 策略、自定义应用图标、`just desktop-preview` 浏览器实时预览模式、`UpdaterBanner` 更新确认横幅、`.github/workflows/desktop-release.yml`（macOS ARM/Intel `.dmg`+`.app.tar.gz`、Linux AppImage、Windows NSIS + `latest.json`）及离线打包／安全边界回归 `scripts/release/test_desktop_packaging.py`。远端 GitHub Actions 四平台安装包生成、Apple 签名／公证与真实 GitHub Releases `latest.json` 更新链路尚未在 CI 上触发运行，单测覆盖与本机实际验证以执行后结果为准，不复用 `soyo-tui` 四平台验收证据。
- 2026-11-19，提交第二轮全仓审查报告 `docs/reviews/review-2026-11-19.md`（R-01 至 R-32，尚未修复）。项目负责人批准其中的桌面端 UI 修正方案并追加 Soyo Light 亮色主题，记为 D-075，目标规范见[设计规范](design/desktop-ui.md) §8。本次只更新决策与文档，未修改代码，未做视觉实现验收。
- 2026-11-19，项目负责人授权确定审查中的全部待定事项：D-076（UI 细则、参照主题描述、Light 色值调整）、D-077（helper 多客户端、安全与配置契约）、D-078（桌面端交付、发布与平台范围）。同时把 D-073、D-074 的日期更正为 2026-11-18。本次只更新文档，未修改代码。
- 2026-11-19，完成第二轮审查报告 `R-01`–`R-32` 及 `D-075`–`D-078` 全部整改（提交 `2fda02f` 至 `bcb93af` 及设计资产提交 `88e5357`，未推送）。后端与发布（`2fda02f`、`91c6241`、`b1e56fc`、`95fe092`）：`soyo helper` 迁移至 `<state_dir>/helper/<fingerprint>.sock`（`0700`/`0600`、对端 uid 校验、`flock` 单例＋存活探测、8 MiB 读取期帧限制、多会话至多 4 并发 Run 与连接归属、断线即取消＋挂起审批拒答、`--config`/`--data-dir` 指纹路由与逐次环境快照）、env 密钥迁入 `<state_dir>/secrets/env` 并注册 `SecretRegistry`（一次性迁移 `~/.soyo/env`）、全局记忆根只在含记忆文件时选 `.soyo` 且不空建目录；桌面端新增自动重连、`get_helper_status`、`session_id` 提交钉住、`utc_offset_minutes` 本地日用量分桶、`externalBin: ["binaries/soyo"]` 随包、`0.3.0` 版本同步、`desktop-v*` 发布触发并移除 Windows NSIS。界面与设计系统（`6793a52`、`13f2eef`、`a19e9be`、`bcb93af`、`88e5357`）：`tokens.css` 单一来源双主题、`Source Serif 4`（OFL 1.1 Latin woff2）、`soyo.theme` 首帧初始化、2×2 纯 CSS `SquaresLoader`、`Button`/`Pill`/`DialogShell`、单一可折叠 248px/48px 侧栏、三档断点、不折行合并胶囊 Composer、三组 9 项 Settings、sentence case 与 i18n（`en-US`/`zh-CN`/`zh-TW`）、`Bypass permissions` 常驻 `--danger`+`ShieldAlert` 与 `Manual approval` `--info`+`Hand`、`soyo-tui` 模式色表对齐、`devin_theme_gen.py`（`--theme dark|light`）、28 frame Dark/Light `docs/design/desktop-ui.pen`（48 项绑定变量，Pencil MCP 保存）及 10 张无头 Chrome 渲染截图（`docs/design/screenshots/desktop/`）。本机验证（macOS 27.2 ARM／Rust 1.99.1）：`RUST_TEST_THREADS=1 just check` 通过 81 组测试共 1790 项、0 失败、27 项条件忽略与 release 构建；`cargo clippy -p soyo-desktop --all-targets --locked -- -D warnings` 零警告，`cargo test -p soyo-desktop --locked` 通过（`export_specta_bindings`）；`just desktop-check`（`tsc --noEmit` 零错、Vitest 2 文件 23 项通过、Vite 构建通过）；`just test-tooling` 通过 13 组共 177 项离线回归；headless Chrome CDP 于 `1280x840` 与 `900x620` 实测零控制台报错、零溢出与零工具栏折行并人工检视全部 10 张截图。未验证范围：GitHub Actions 远端三平台桌面打包矩阵（`desktop-release.yml`）、Apple 签名／公证、真实 GitHub Releases `latest.json` 在线更新链路及 Linux 原生运行实测。
- DEV-038至DEV-053已完成并随0.2.0发行，代码审查和发行证据见对应历史记录。当前N2的DEV-054至DEV-061、DEV-075已完成四平台验收，下一阶段为N3上下文与扩展。

## 后续开发状态

开发计划与专项设计按下列任务跟踪，新增功能实际状态如下。0.2.0已交付N0／N1，N2当前源码验收完成，后续版本范围在发行准备时确定。

| 阶段 | 任务 | 实际状态 | 已有基础与下一入口 |
| --- | --- | --- | --- |
| N0 共享基础 | DEV-038 至 DEV-046 | 完成（本机验收） | 共享应用、日志／追踪与会话导出均通过正常 CLI 入口及故障回归，见下表 |
| N1 默认 TUI | DEV-047 至 DEV-053 | 完成（四平台候选验收） | 默认入口、对话／审批／会话／后台／导出已接入共享服务，候选与本机试用程序通过验证，设计与双语说明已交付 |
| N2 隔离与 MCP | DEV-054 至 DEV-061、DEV-075 | 完成（四平台验收） | 原生隔离、受控网络、命令结构事实、MCP与凭据通过最终源码CI、原生／性能／MCP／Markdown与TUI验收，见[N2记录](n2-validation.md#最终四平台验收) |
| N3 上下文与扩展 | DEV-062 至 DEV-068、DEV-076 至 DEV-078、DEV-084 至 DEV-089 | 完成（四平台验收） | 16项共享运行入口通过四端Rust、原生、性能、MCP、插件、CLI与TUI联合矩阵，固定源码 `e1da373`，见[N3最终验收](n3-validation.md#四平台最终验收) |
| N4 Goal 编排 | DEV-069 至 DEV-074 | 完成（本机受控验收） | 独立版本化 Goal 存储、专属 Session、手动／有限自动 Run、租约恢复、预算、证据核验、重规划、CLI／TUI 与报告通过本机 PTY／故障矩阵，见[N4验收](n4-validation.md) |
| N5 Browser | DEV-079 至 DEV-083 | 完成（四平台固定源码验收，固定提交 `5fff1c4`；`linux-arm64` 定制引擎已实测） | `BrowserProvider`／`BrowserState`、受控 `fetch_url`／`web_search`、schema v2 安装树与启动前同描述符定位复核、三补丁固定 Playwright worker（`de88c9fc…` 含 `processInfo` 与 `captureScreenshot`）、显式 Colima VM 后端与宿主 broker／SSH 反向网络通道、origin 凭据槽与下载／上传 broker、三协议截图图片链路、native／VM `ChromiumSandboxInspector`、`linux-arm64` 定制 23 文件捕获引擎（`colima-n5cap` `--require-screenshot` 22/22 passed 与 `n5arm22` 原生 `1280x720` Masked PNG 捕获），以及固定源码 `5fff1c4` 上的四平台 [CI 44044030536](https://github.com/Ariakage/saya-harness/actions/runs/44044030536)、[N5 native acceptance 44044034857](https://github.com/Ariakage/saya-harness/actions/runs/44044034857)（Linux 22 步／macOS 19 步零缺项含 Linux x64/ARM64 原生 Chromium sandbox）与 [Release candidates 44044045558](https://github.com/Ariakage/saya-harness/actions/runs/44044045558) 全绿验证；非 `linux-arm64` 定制 Chromium 构建分发与新版号 tag 发布未执行，详情见[N5 验收](n5-validation.md) |
| F-14 桌面端 GUI（`soyo-desktop`） | D-072 至 D-078 | 本机实现与回归通过（远端 CI 安装包与在线更新未触发） | 已迁移为共享 `soyo helper`（D-073／D-077 `<state_dir>/helper/<fingerprint>.sock` 多会话并发、连接归属与自动重连）、随包 `soyo` sidecar（D-078 macOS `.dmg`/`.app` + Linux AppImage，移除 Windows NSIS）、Soyo Dark/Light 双主题与 `SquaresLoader`/9 项设置（D-075／D-076），通过本机 `soyo-desktop` Clippy/单测、Vitest 23 项、`test_desktop_packaging.py`、10 张 Chrome 截图与 28-frame `.pen` 同步；远端 CI 打包与在线更新未在 GitHub Actions 触发，见[桌面端设计](desktop.md) |
| N6 工具发现、步骤验证与程序化编排 | DEV-090 至 DEV-096 | 进行中（已完成 DEV-090–093；DEV-094–096 待实现） | `DEV-090`（S2 / `D-079`：按需发现）与 `DEV-091`（S3 / `D-080`：`StepVerifier` 结构化模型、`goals.sqlite3` v2 迁移、`goal confirm` 锁定、`set-verifier` CLI 与 `run_step_verifier`）已完成本机验证；`DEV-092`（`run_step` 验证门禁与 `start --auto` 有界重规划）已完成本机验证；`DEV-093`（S5 / `D-081`：`ToolDefinition.examples` 校验与双路径投影）已完成本机验证；`DEV-094`（S1 JS 运行时评估）已产出评估报告并停靠待拍板；`DEV-095`–`096` 待拍板后推进，详见[N6 规划](n6-plan.md) |

### N6 任务跟踪

| 任务 | 来源 | 优先级／轨道 | 状态 | 依赖／前置核验与实际证据 |
| --- | --- | --- | --- | --- |
| DEV-090 | S2 (`D-079`) | P1 实现主线 | 完成 | 依赖 DEV-059、DEV-062；实现 `McpSettings.defer_loading`/`defer_threshold`（默认 `Auto` 与 `8`）、`eager_tools`、`tools/discovery.rs`（`tool_search`/`tool_describe`）、`dispatch.rs`/`runtime/mod.rs` 前缀稳定追加与 `replace_external` 清理及 `prompts/v1/tool-discovery.md`；通过 `tests/tool_discovery.rs` 5 项（含 `≤ 8` 全量、24 工具延迟展开与缓存前缀检验）、`tools::discovery`/`mcp::config` 单测、`RUST_TEST_THREADS=1 just check`（82 组 1799 通过、0 失败、27 忽略）与 `just test-tooling`（13 组 177 项通过） |
| DEV-091 | S3 (`D-080`) | P1 可靠性线 | 完成 | 依赖 DEV-072、DEV-057；实现 `StepVerifier`/`StepVerificationResult`/`confirmed_verifiers_digest`（`goal.rs` 不变量强制 verifier 步骤无通过宿主证据不可 `Completed`）、`goals.sqlite3` v1→v2 原子迁移与 `step_verifications` 表、`exec::execute_host_verifier`、`soyo_app::goal::run_step_verifier`、`soyo goal set-verifier` 与 CLI/TUI/Markdown 展示及 `prompts/v1/goal-plan.md`；通过 `goal::tests` 18 项、`soyo-app --test goal_verifier`（`backend = "host"` 单测确认锁定/`PLAN`/`deny`/通过与失败/`SecretRegistry` 脱敏/超时，原生 Seatbelt/Bubblewrap CLI 端到端留归 `DEV-092`）、`soyo-cli --test goal` 16 项及 `RUST_TEST_THREADS=1 just check`（83 组 1804 通过、0 失败、27 忽略） |
| DEV-092 | S3 (`D-080`) | P1 可靠性线 | 完成 | 依赖 DEV-091、DEV-074；`run_step` 在租约／心跳内执行已确认 verifier 并经 `finish_dispatch_with_verification` 原子落库证据，`start --auto`／`continue`／TUI 仅在退出码 0 后前进并拒绝未确认 verifier，`replan_failed_step` 在 `max_replans` 内回灌脱敏单行宿主输出（`Denied`→`Blocked(Authorization)`、超时／取消／未知→`Waiting(UnknownSideEffect)`）；通过 `goal::tests` 19 项、`soyo-cli --test goal` 19 项（含原生 Seatbelt 端到端：谎报→重规划→通过、`max_replans` 耗尽 `limit_reached`、deny／超时／未确认 fail-closed）、`RUST_TEST_THREADS=1 just check`（1808 通过、0 失败、27 忽略，fmt／严格 Clippy／release 构建通过）与 `just test-tooling`（退出码 0） |
| DEV-093 | S5 (`D-081`) | P2 并行附带 | 完成 | 依赖 DEV-054、DEV-059；实现 `ToolDefinition.examples: Vec<Value>`（≤5 条、8KiB、256 节点、禁占位符与保留字段）、注册期离线 jsonschema＋`SecretRegistry` 校验（`invalid_tool_example`／`credential_exposure`）、MCP `tool_examples` 与插件 manifest 同名字段（宿主覆盖清单）、`tool_describe` 返回样例及三协议描述文本安全投影（不动 schema、兼容 `strict_tools`），为 `tool_search`／`tool_describe`／`exec`／`search_text`／`edit_file`／`apply_patch` 配真实样例；通过 `registry::example_tests` 5 项、`provider` 投影单测、`tool_discovery` 样例链路断言与 `just check`／`just test-tooling` |
| DEV-094 | S1 (`D-082`) | P2 实现主线（S2 之后） | 评估完成待拍板 | 依赖 DEV-090、DEV-057、DEV-067；按 `D-082` 完成「宿主进程内嵌入式引擎（Boa 纯 Rust／QuickJS 含 C）vs 沙箱内 JS worker 子进程（及受控 Node 对照）」六维对比，产出[评估报告](reports/n6-js-runtime-eval-2026-11-20.md)与五项待拍板决策点，**停靠等待拍板补录 `D-083+`** 后再进 `DEV-095` |
| DEV-095 | S1 (`D-082`) | P2 实现主线（S2 之后） | 待开始 | 依赖 DEV-094；按拍板定案实现 opt-in 只读 JS REPL 执行面与逐底层调用 `deny`/Hooks/审批拦截，不以 37% 省 token 为验收线 |
| DEV-096 | N6 联合 | 联合验收 | 待开始 | 依赖 DEV-090、DEV-092、DEV-093、DEV-095；跨能力联合回归、开关对照评估与双语增量说明 |

### 全局记忆规划补充

2026-11-08，按 D-045 将F-07细化为模型可读取、检索、编辑和遗忘的全局记忆。候选入口为 `~/.saya/MEMORY.md`，主题资料放入 `~/.saya/memory/`。新增DEV-084至DEV-089六项N3任务，覆盖可读文件、来源与作用域、上下文预算、三权限模式、并发发布、回滚、人工审阅和跨会话验收。后续计划共52项任务。

[设计草案](memory.md)与路线图、任务表已同步，任务编号、依赖无环及仓库内Markdown链接已检查。上述能力均待开发，当前已有的 `--memory-file` 显式注入见使用指南。

### N1 实施记录

| 任务 | 状态 | 阶段提交 | 证据 |
| --- | --- | --- | --- |
| DEV-053 | 完成（候选验收） | 7b7f2df，验收源码a6c172d | [候选工作流42407739653](https://github.com/Ariakage/saya-harness/actions/runs/42407739653) 的四原生目标全部通过，下载的归档／二进制／源码声明／脚本摘要／权限／凭据检查一致。0.2.0-rc.1的双语说明、迁移、界面设计及本机试用程序已交付。详细环境与SHA-256见[TUI验证](tui-validation.md)，正式发布安排在后续代码review与其他测试之后 |
| DEV-052 | 完成（四平台原生验收） | 87feea8、e098e9a、a6c172d、131ca6b | [CI 42407739097](https://github.com/Ariakage/saya-harness/actions/runs/42407739097) 的Ubuntu22.04 ARM／x86_64和macOS15 ARM／Intel全部通过，每个平台执行445项Rust测试、优化构建、工具smoke、文本会话、5项审查回归及13组真实PTY场景。包含三协议、多行Unicode／粘贴、4模式×3权限、逐工具审批、模式失效、会话／导出、后台／PLAN、unknown、慢消费者和信号。8项输入／6项终端测试含36轮panic与250ms帧预算，实际证据见[TUI验证](tui-validation.md) |
| DEV-051 | 完成（本机正常入口） | 16633ac | 无子命令仅在 stdin／stdout 均为 TTY 时进入 TUI，非 TTY／管道及 JSON／JSONL 冲突提前返回2且 stdout为空。3 项新路由回归和 105 项 CLI 测试通过，原 chat／run／resume／session／config 继续使用显式入口，workspace Clippy通过。日志为 target/n1-dev051-*.log，完整本机PTY矩阵结果见 DEV-052 |
| DEV-050 | 完成（组件与服务验收） | 7e9b2f6 | 新建／浏览／恢复会话、profile 选择、工具／后台快照、取消与 JSON／Markdown 导出复用 saya-app。11 项存储服务回归、36 项 TUI lib 测试和全目标 Clippy 通过，覆盖锁释放、unknown／截断及 run_id＋call_id、PLAN 路径、取消／退出后的发布回执、脱敏、SQLite 初始化不阻塞界面，以及工具轮次后的最终回答位置。详情缓存限制为 2 MiB／10,000 逻辑行。日志为 target/n1-dev050-*.log，正常入口在 DEV-051／052 验收 |
| DEV-049 | 完成 | 3b6da25 | 复用core RuntimeHandle和输入桥，支持实际／待切换模式、空闲权限、逐工具批准、澄清、命令面板和取消。19项lib测试及Clippy通过，含10项真实受控运行／人机输入边界回归，阻断重复键、组合键与隐藏面板误批准 |
| DEV-048 | 完成 | 152ff2d | 有界UTF-8/grapheme编辑、流事件聊天协调器、安全折行／代码块、滚动与窄屏呈现已实现。9项lib组件／草稿回归和Clippy通过，重绘与事件消费有界调度；正常入口集成按DEV-051／052验收 |
| DEV-047 | 完成 | d7a1489 | saya-tui library、Ratatui0.30.2／Crossterm0.29.0已锁定。3项真实PTY生命周期测试、Clippy及格式检查通过，覆盖normal/error/panic/重复进入及termios与屏幕恢复。日志为target/n1-dev047-*.log |


### RC1之后的TUI交互完善

2026-11-08，开发线加入斜杠命令补全与输入建议。24项静态命令／参数前缀与命令面板、帮助共用，支持灰色后缀、候选说明、方向键选择、Tab／末尾→接受及Esc隐藏。Enter执行完整草稿，profile名、会话／任务ID和目标路径由用户补齐。

本机macOS27.2 ARM通过格式、全目标Clippy、466项Rust测试、优化构建、5项发行工具测试和14组真实PTY验收。新增场景覆盖补全时的零副作用、参数前缀、草稿／光标／多行／字面量斜杠、命令面板及人工审批优先级。Shift+Esc关闭建议和缺少导出路径时保留草稿另有针对性回归。日志为 `target/tui-completion-check.log`，完整正常入口报告为 `target/tui-completion-release-matrix.json`。本节证据来自本机，RC1的四平台候选证据对应上方已固定源码。

#### 紧凑界面、会话标题与 thinking effort

2026-11-08，按 D-042 初版实现参考图对应的紧凑窗口。初版将宽度低于100列或高度低于24行的窗口设为黑底外框、顶边工作目录、状态右侧权限、灰色输入与蓝色竖线。底栏显示模式、模型、thinking effort及带环形字符的输入占比，最小20×6仍可编辑。长模型名为effort与用量留出空间。窗口标题从首条用户任务派生为 `Saya | 简短标题`，已知凭据先脱敏，随后合并空白、保留80字符并移除终端控制。新建、profile选择和恢复同步标题，OSC 2与xterm标题栈走有界恢复。

profile新增可选 `thinking_effort`，支持low／medium／high／xhigh／max，三协议分别传入对应effort字段。未配置时保留服务默认及原配置指纹，独立核查按其目标profile取值。界面显示实际请求值，服务内部映射依模型确定。Chat兼容服务的 `reasoning_content` 按assistant轮次以严格协议项持久保存和回传，完整JSON／SSE均受16MiB聚合限额约束，并覆盖恢复、正文／导出隔离及敏感数据事务拒绝。参数及协议边界见[配置](configuration.md#thinking-effort)与[模型接入](model-protocols.md#chat-completions)。

输入占比使用最近一次执行请求的报告用量，独立核查与运行累计值分开。开始新请求及新建／恢复／切换profile时清空旧值，缺失显示 `—`，估算带 `~`。独立复核发现并修复人工审批待清理和最终队列排空时可能沿用旧值的两个边界。

本机macOS27.2 ARM／Rust1.99.1的 `MACOSX_DEPLOYMENT_TARGET=15.0 ./scripts/check.sh` 完整通过格式、workspace全目标Clippy、488项Rust测试和优化构建。该程序通过15组真实PTY、5项发行工具测试、工具smoke、文本会话journey与5项既有审查回归，私有模型配置校验通过。三协议max均在正常TUI入口核对请求字段与显示值，compact覆盖80×24／60×18／20×6、审批／补全、用量、标题注入和恢复。隐藏reasoning通过正常工具调用、SQLite保存、后续请求、终端输出和完整导出的联合验证。验证使用受控本地服务，环境和程序摘要见下方记录。

- 完整检查为 `target/tui-compact-check-final.log`，正常入口报告为 `target/tui-compact-release-matrix.json`。
- 该次程序SHA-256为 `7a71c6aa37f128cff2663ae8a1ca3f05c5b45639e0bd99aa9029f4e11b481a18`，Mach-O部署版本为15.0。
- 视图和交互说明见[TUI设计](tui-design.md)与[TUI接口](tui.md#紧凑窗口与会话标题)，本次新增功能的跨平台及真实服务证据按对应环境后续验收维护。

#### 统一参考布局与全屏信息侧栏

2026-11-08，用户实测指出 `c09fc39` 在大窗口进入旧样式。已删除旧宽屏绘制路径，按 D-043 将主区参考样式应用到全部尺寸，灰色输入区默认4行并采用实心蓝色左条。达到120列×24行时显示会话名、上下文分项、扩展状态和工作目录侧栏。20×6详情保留可滚动内容，翻页依据实际可见高度。

新增共享应用层ContextModel，在执行请求边界推送仅含固定标签、计数和Skill数量的最新快照。System／Instructions／Skills／Preferences／Memory／Conversation／Files／Tools／Protocol九类采用明确标注的字符启发式估算，侧栏支持小数百分比和部分单元格条形。宿主前缀按位置和类型验证，后续同形用户JSON归入对话，原始协议内容按实际匹配关系去重。Review保持独立，新建／恢复／换profile清空旧快照，最终关闭通道保留末次统计。MCP与插件显示当前Not implemented，Skill数量来自实际注入的目录元数据。

本机macOS27.2 ARM／Rust1.99.1通过完整 `MACOSX_DEPLOYMENT_TARGET=15.0 ./scripts/check.sh`，包含格式、全目标Clippy、505项Rust测试和优化构建。独立复核通过40项controller和23项view测试。优化程序通过15组真实PTY，新增110／120／160列、119↔120和23↔24阈值往返、上下文与Skill真实来源、Files结果增长、实心蓝条、极小窗口逐页核查及既有三协议／模式／权限／会话／取消链路。

检查日志为 `target/tui-reference-check.log`，正常入口报告为 `target/tui-reference-release-matrix.json`，程序SHA-256为 `a53f3a266f14e96f270527d55162eef2b6c9d4ed09f17d124aacbe75592a441f`。两组生产组件TestBackend布局PNG位于 `target/tui-reference-preview-110x32.png` 和 `target/tui-reference-preview-160x40.png`，预览使用合成数据。本节验收为本机受控服务和PTY，原生iTerm2截图访问受到工具限制。

#### 视觉层次与空输入Tab模式循环

2026-11-08，按 D-044 参考OpenCode细化配色与正文层次。共享低对比主题区分主区、侧栏、输入区和代码／工具卡片，角色标签与元数据分别着色，宽正文增加左右留白。Assistant呈现基础Markdown标题、列表、引用、强调、行内代码和代码卡片，流式未闭合标记保留字面内容，工具与审批维持安全原文。富文本逐逻辑行生成，片段与嵌套有界，跨样式grapheme折行沿用总行数和10,000行尾缓存。URL／HTML内容只作可见文本，原始会话内容继续保存。

空输入Tab按WORK→CODE→PLAN→BUILD→WORK循环，复用模式命令的检查、持久化与审批失效路径。运行中和UI任务收尾期间从最新请求的模式继续循环，模式应用仍由core执行。普通草稿、空格、多行和命令前缀维持编辑／补全，命令面板独立；空输入的重复／释放事件不会插入Tab或重复切换，常规切换不生成额外对话消息。

本机macOS27.2 ARM的最终完整检查通过格式、全目标Clippy、524项Rust测试和优化构建。优化程序通过16组真实PTY，新增空Tab循环、无模型／存储副作用、编辑与面板边界、活跃模式持久化、人工审批失效、三协议正文样式及原文保存。5项发行工具测试、工具smoke、文本journey和5项既有审查回归均通过。检查为 `target/tui-polish-check-final.log`，PTY报告为 `target/tui-polish-release-matrix.json`，程序SHA-256为 `1a01f8512cd191710c922da49ba23ae64e8e4bf85e689040586907af8fb8f812`。

生产组件的合成状态预览为 `target/tui-polish-preview-160x40.png`、`target/tui-polish-preview-110x36.png` 和 `target/tui-polish-preview-idle.png`。模型服务验证使用本地可控夹具，平台范围为上述本机环境。

#### 任务清单、Thinking与可折叠工具

2026-11-08，按D-046加入模型可维护的 `todo_write` 会话任务清单。成功快照通过调用／结果历史重建，运行上下文提供最新列表，TUI侧栏显示进行中及完成数。参数、64项／64KiB限额、一次性许可、三权限模式、PLAN元数据、失败／取消／存储错误及SQLite恢复有回归，工具与PLAN英文提示词及文档快照同步。

三协议为应用层提供可读Thinking投影，审核、签名、redacted和加密内容保持隔离。watch日志按轮次有序显示，单轮256KiB、合计2MiB／64轮，完整脱敏后截断，已知凭据配置下等待该请求结束后发布正文。Thinking默认折叠，长工具结果超过6行或512字节也默认折叠，可点标题或Ctrl+O展开。同一工具调用复用执行中／完成卡片，鼠标滚轮在指针所在对话／详情内滚动，启停及异常退出恢复鼠标报告。

独立复核补齐最终历史重建Tasks、空回答惰性卡片、日志驱逐后的结束提示、最终事件排空，以及JoinError已消费句柄后的后台清理等待。`RuntimeHandle::shutdown_background`复用现有取消与清理，真实exec在模型panic／任务abort后均等待到PID消失，重复清理有回归。

本机macOS27.2 ARM／Rust1.99.1的 `MACOSX_DEPLOYMENT_TARGET=15.0 ./scripts/check.sh` 完整通过格式、workspace全目标Clippy、562项Rust测试与优化构建。该程序通过18组正常入口PTY、5项发行工具测试、工具smoke、文本会话和5项审查回归，私有配置离线校验通过。PTY使用本地受控服务，覆盖三协议可读推理展开与opaque隔离、任务逐次审批／PLAN拒写／恢复、长工具与伪标题点击、左右分区滚轮、短侧栏、终端恢复及既有流程。观察器为完整差分帧和有界排空补了回归门槛。

日志为 `target/tui-interactive-check-final.log`，正常入口报告为 `target/tui-interactive-release-matrix.json`，程序SHA-256为 `9050acb2510c02e698f199a7aaf3dbb17a4151a46b12e9e60193ed91fb2ea76b`，Mach-O最低部署版本为15.0。`target/tui-interactive-thinking-tasks-160x40.png` 与 `target/tui-interactive-expanded-tool-110x36.png` 来自该程序的受控PTY单元格快照。当前证据为本机离线验收，四平台候选证据仍对应上方固定RC1源码。

#### 输入区与摘要卡片美化

2026-11-08，开发线进一步细化输入区、工具卡片与侧栏。Message采用Strong前景与加粗，发送／换行／折叠提示使用键帽样式。空草稿显示斜体 `Describe a task…` 渲染占位，审批／澄清、补全和粘贴超限按实际状态显示，底部留白加入浅色收边。真实光标、Unicode多行和极小窗口有回归。

可识别的完整结构化工具结果默认收成摘要卡片，展示实际Todo计数、文件行范围／字节数、搜索／目录项数、退出码和错误信息，展开查看原始结果。摘要严格校验数据，输出限制512字节／120个grapheme，覆盖巨型组合簇、控制字符、重复字段、未知／矛盾／不完整结果。Thinking采用精简状态标题，侧栏加入弱分隔、强标题、非零数值层次和任务进行中强调。

本机macOS27.2 ARM／Rust1.99.1通过部署目标15.0的完整格式、workspace全目标Clippy、577项Rust测试及优化构建。正常入口19组PTY全部通过，新增短工具摘要、失败与截断状态、逐卡展开结果与模型所见JSON相等、同名真实占位文本、66KiB粘贴、输入键帽和底部收边等验证。5项发行工具测试、工具smoke、文本journey、5项审查回归及私有配置离线校验通过。

检查日志为 `target/tui-beautify-check-final.log`，PTY报告为 `target/tui-beautify-release-matrix.json`。程序SHA-256为 `aef46edff56e04056f79cf0f2a58636e5ddf349f9fead6a82ce6d1b54865dade`，Mach-O最低部署版本为15.0。`target/tui-beautify-refined-160x40.png`、`target/tui-beautify-refined-110x36.png` 和 `target/tui-beautify-composer.png` 为生产组件合成状态预览，含颜色与字重投影。本轮正常入口PTY另保存4组单元格快照，实际模型服务使用本地受控夹具。

#### 四模式主题色联动

2026-11-08，按D-047为WORK／CODE／PLAN／BUILD分配晴蓝 `#58A6FF`、薄荷绿 `#7DCB9B`、薰衣草紫 `#BB9AF7` 与暖杏橙 `#F6AD55`。输入框实心色条和底栏实际模式共用 `mode_color`，待切换文字分别使用当前／目标色，箭头为Muted，色条按实际模式生效更新。模型名称使用显式正文前景提升可读性，未知模式回退Muted。

部署目标15.0的完整格式、workspace全目标Clippy、579项Rust测试与优化构建通过，独立复核通过128项TUI lib测试。新增回归覆盖四模式与未知值、5种尺寸、极小详情单行输入、待切换与实际生效、模型字段前景。最新程序的19组正常入口PTY全部通过，包含12种启动组合、Tab四色循环、运行中pending及边界后的同步着色。5项发行工具测试、工具smoke、文本journey、5项审查回归与私有配置离线校验通过。

环境为本机macOS27.2 ARM／Rust1.99.1，日志为 `target/tui-mode-colors-check.log`，PTY报告为 `target/tui-mode-colors-release-matrix.json`，程序SHA-256为 `d89d5ca61db05da5d564fff28b51957b9090aa4052e9138365d1c09316cb289a`，Mach-O最低部署版本15.0。四模式与pending单元格快照保存为 `target/tui-mode-colors-*-110x32-cells.json`，局部合成图 `target/tui-mode-colors-composers.png` 根据本轮受控PTY数据渲染。

#### Ctrl+P 弹出命令面板

2026-11-08，按D-048将Ctrl+P改为独立的居中圆角浮层，最多88列×14行。24项共用命令目录显示选中高亮、命令说明与选择计数，方向键／候选行滚轮浏览，鼠标点击选中，Enter执行高亮项。可见窗口保持行位置，缩放后跟随选择，20×6保留三项候选和关闭提示。

浮层与真实详情分别保存，Esc／Ctrl+P关闭保留详情、滚动位置和草稿。模态期间编辑、粘贴及背景折叠／滚动由面板接管，带参数命令填入空草稿，既有斜杠草稿保持原文。提交与关闭要求新的Press。独立复核修复异步详情遮盖澄清请求时可能提交隐藏草稿的边界，审批与澄清统一先核对可见请求，再接受回复。

本机macOS27.2 ARM／Rust1.99.1，部署目标15.0的完整格式、workspace全目标Clippy、590项Rust测试及优化构建通过，独立139项TUI lib复核通过。最终程序完成20组正常入口PTY，新增模态组覆盖选择／执行、草稿、粘贴、鼠标边界、24项滚动、普通／宽屏／极小窗口及缩放、详情恢复、人工审批隔离和Ctrl+D清理。5项发行工具测试、工具smoke、文本会话、5项审查回归与私有配置离线校验通过。

完整检查为 `target/tui-command-palette-check.log`，PTY报告为 `target/tui-command-palette-release-matrix.json`，程序SHA-256为 `094562477a5e9643727213a7276861dd884acb79850f53b745ea7fa8d9d08421`，实际Mach-O最低部署版本15.0。本轮证据来自本机及本地受控协议服务，RC1四平台证据按其固定源码记录。

### N0 实施记录

| 任务 | 状态 | 阶段提交 | 证据 |
| --- | --- | --- | --- |
| DEV-038 | 完成 | `b8c84af` | 新 saya-app 迁移模型组装与会话生命周期，CLI 正常入口回归通过，独立审查通过。`./scripts/check.sh` 的格式、Clippy、全套离线测试及 release 构建通过，日志为本机 target/n0-dev038-check.log |
| DEV-039 | 完成 | `88014a9` | 共享 EventWriter／EventModel、1秒有界 EventSink、人工输入桥与取消句柄已接入 CLI。3项桥接测试、三协议SSE至JSONL参数化正常入口回归、CLI完整回归及 app／CLI Clippy 通过，覆盖队列背压／断开／过期回复，日志为 target/n0-dev039-*.log |
| DEV-040 | 完成 | `97e92b7` | 五级 LogLevel、类型化白名单记录、旧行解码和安全文件边界已实现。日志9项测试及 core Clippy 通过 |
| DEV-041 | 完成 | `b01513a` | tracing安全subscriber、有界普通／优先队列、退出排空及执行／核查／工具／后台关联已实现，CLI安装全局订阅。日志12项、runtime40项及 CLI Clippy 通过 |
| DEV-042 | 完成 | `838bf00` | --log-level／SAYA_LOG_LEVEL优先级、show分页与tail轮转跟随已接入，查询只读且支持JSON／JSONL。查询4项、CLI5项、日志全16项和workspace Clippy通过，覆盖慢pipe中断、文件代际、私有权限、时间与终端控制 |
| DEV-043 | 完成 | `3cab1ae` | 单只读事务快照与 WAL image 校验，包含全集合及真实持久水位。7项快照测试（含合法JSON篡改与可选字段兼容）、5项 WAL 回归（含重建限额、SQLite VM 中断及旧逐文件限额兼容）和 core Clippy 通过，源128MiB／记录32MiB／100k行／5秒默认限额已固定 |
| DEV-044 | 完成 | `ca7f77d` | JSON导出 saya.session schema v1，范围／工具正文／内部态省略／复合调用关联／脱敏截断已实现。4项导出测试及core Clippy通过 |
| DEV-045 | 完成 | `ecf9c0b` | Markdown复用同一脱敏投影，动态围栏、HTML／ANSI／双向控制处理和总限额已实现。6项导出测试及 core Clippy通过 |
| DEV-046 | 完成 | `a463eba` | CLI离线导出和0600排他文件发布已实现，父路径身份复查、私有staging、摘要核验及机器错误码完整。7项发布测试、5项CLI验收和错误确认单测通过，涵盖RLIMIT写失败、路径替换、取消及已有目标保护 |

### N0 最终验收（2026-11-08）

验收源码为 `f1f48e4`，环境为 macOS 27.2 ARM、Rust 1.99.1。`./scripts/check.sh` 完整通过格式检查、workspace Clippy（warnings 为错误）、392项离线 Rust 测试及 release 构建，记录为本机 `target/n0-acceptance-final.log`。

| 验收要求 | 实际证据 |
| --- | --- |
| 应用服务与恢复契约 | CLI完整回归，模式覆盖、配置变更、PLAN目录、锁与恢复行为，以及独立迁移审查 |
| 流事件与输入控制 | Chat／Responses／Anthropic三协议SSE至JSONL正常入口，单一终态、关联ID与递增序号；队列、断开、晚回复、取消及审批失效回归 |
| 日志与追踪 | 日志16项（含查询）、runtime40项及CLI日志5项，覆盖字段白名单、秘密、轮转、优先队列、过载、丢弃、排空与慢pipe取消 |
| 一致性快照 | Snapshot7项及WAL5项，覆盖并发提交、真实水位、只读源、合法JSON损坏、旧schema、限额、VM取消和旧恢复每文件512MiB兼容 |
| 导出与文件发布 | Export6项、发布7项、CLI导出5项及错误确认单测，覆盖两格式、选择／省略／脱敏／截断、路径替换、RLIMIT写失败、取消及目标保护 |
| 原生release程序 | smoke、journey、review_regressions三组离线演练均通过；新日志查询及两种导出由同一个release程序实测，收据哈希与0600权限核对通过 |
| 文档与凭据 | 64个Markdown文件456个相对链接有效，既有私有凭据未出现在待提交文件中 |

新功能release演练报告为本机 `target/n0-release-features.json`，查询与导出的模型请求数为0。该次程序 SHA256 为 `d6a23962d167303bd0c4761e8b1fcff3ec0d5bd855ea625be40b2718b776ffc1`。跨平台记录继续分别依据实际环境验收维护，本次N0证据来自上述本机环境。

阶段提交见上表。独立审查追加 `b51787f`、`758dced`、`31d1cd6` 修复快照边界，`41a1e9f` 补齐三协议直接证据。实现时固定了快照128MiB源总量／32MiB记录／100k行／5秒、每导出32MiB、每字段64KiB等默认限额，具体语义见[导出说明](session-export.md)。

### 2026-11-08 专项设计细化

新增 Browser、原生 Sandbox 和上下文／长输出设计，将 saya-plugin 细化为 v1 草案并提供 JSON Schema 与 TOML 示例。Linux 组合和 Seatbelt 独立后端按 D-040／041 记录，容器／VM 作为统一接口的后续实现路径。该次设计交付时，后续功能 DEV 均为待开始，之后的实现状态见上方 N0 记录。

本机只读检查确认 macOS 27.2 提供 sandbox-exec，系统手册标记 deprecated。原生后端、macOS 15、Linux namespace／Seccomp／代理以及性能尚待对应 DEV 实测。

本轮文档验证通过 64 个 Markdown 文件的 450 个相对链接、46 项后续任务的无环依赖和 20 个能力编号检查。插件 JSON Schema 自校验、TOML 示例及 6 个有效／25 个拒绝案例通过，JSON 示例和已知测试凭据泄露检查通过。验证范围为文档、结构规范与示例，报告位于本机 `target/runtime-design-validation.json`。

### 2026-11-07 初版计划验证

初版规划核对了引用对话、workspace、日志、CLI 路由、流事件和存储查询接口。`./scripts/check.sh` 通过格式检查、Clippy、342 项 Rust 测试和 release 构建。61 个 Markdown 文件的 418 个相对链接、37 项后续 DEV 的无环依赖、20 个能力编号及 JSON 示例检查通过。变更范围为文档及 core 模块说明注释。

## 阶段状态

| 阶段 | 状态 | 退出证据 |
| --- | --- | --- |
| P0 工程基础 | 完成 | DEV-001、DEV-002 完成；四平台 CI 检查通过，见下方记录 |
| P1 核心循环 | 完成（受控验证） | DEV-003 至 DEV-006 与 DEV-037 完成；可复用 core 循环及工具、权限、后台、预算在本地模型替身与受控进程上验证 |
| P2 工具与权限 | 完成（受控验证） | DEV-007 至 DEV-012、七工具派发及 12 组模式验证通过；运行时接线、CLI 与持久化的状态见 P1、P4、P5 |
| P3 模型协议 | 完成（受控验证与指定模型抽样） | DEV-013 至 DEV-017、DEV-034 的本地夹具通过；指定模型三协议文本和工具往返抽样通过，完整证据见下方 |
| P4 CLI 与交互 | 完成（只读与受控验证） | DEV-018 至 DEV-021 完成；连续对话、单次任务、人工输入、终端控制与项目上下文证据见下方 |
| P5 持久化与恢复 | 完成（本机受控验证） | DEV-022 至 DEV-025、DEV-035 和项目指令来源记录已验证 |
| P6 输出与集成 | 完成（本机受控验证） | DEV-026 至 DEV-029 完成；AC-01 至 AC-23 已索引，发行验收归 P7 |
| P7 发行准备 | 完成 | DEV-030 至 DEV-032 完成；四原生目标最低环境、解包演练与审查回归通过 |

## 任务状态

| 任务 | 阶段 | 状态 | 证据／阻碍 |
| --- | --- | --- | --- |
| DEV-001 | P0 | 完成 | workspace、固定工具链与锁文件已建立；本机检查与构建通过，见下方记录 |
| DEV-002 | P0 | 完成 | 本地测试和四平台 CI 检查通过，见下方记录 |
| DEV-003 | P1 | 完成 | 核心数据、schema v1 和适配接口已建立；本机契约测试通过 |
| DEV-004 | P1 | 完成 | 测试专用假循环、内存存储和执行前保存验证通过；不接真实工具 |
| DEV-005 | P1 | 完成 | 原型的取消、超时、预算、重试和结果映射已验证；core 接入见 DEV-037 |
| DEV-006 | P1 | 完成 | 原型模式切换、取消优先与 PLAN 工具重查已验证；core 接入见 DEV-037，CLI 交互待 P4 |
| DEV-007 | P2 | 完成 | 四项只读工具、根目录校验、忽略规则及上限已实现，本地测试通过 |
| DEV-008 | P2 | 完成 | 单文件写入、补丁、意图存储及结果保留已通过本地受控测试 |
| DEV-009 | P2 | 完成 | argv、受控环境、输出限额、超时与进程组清理已在受控进程上验证 |
| DEV-010 | P2 | 完成 | 模式矩阵、路径限制和 TOML 规则优先级本地验证通过；需核查／确认的工具门禁默认拒绝 |
| DEV-011 | P2 | 完成 | 独立无工具核查、人工兜底和共享请求预算经替身测试；三协议 profile 接线见 DEV-017 |
| DEV-012 | P2 | 完成 | 一次性许可、预授权和失效经受控验证；core 调度接线见 DEV-037，CLI 待 P4 |
| DEV-013 | P3 | 完成（受控验证） | HTTP／SSE、鉴权、超时、取消及系统代理通过本地夹具验证 |
| DEV-014 | P3 | 完成（受控验证） | Chat Completions 流、完整响应、工具回传与本地运行时闭环已验证 |
| DEV-015 | P3 | 完成（受控验证） | Responses typed items、无状态历史回传和本地运行时闭环已验证 |
| DEV-016 | P3 | 完成（受控验证） | 两版本 Messages、签名状态、工具回传及本地运行时闭环已验证 |
| DEV-017 | P3 | 完成（受控验证） | 三协议独立核查、默认／指定 profile、预算与人工兜底已验证 |
| DEV-018 | P4 | 完成（core 配置组件） | 严格加载、XDG、权限规则、环境引用与优先级通过本地测试；CLI 接线属 DEV-019 |
| DEV-019 | P4 | 完成（只读 CLI 入口） | run／chat、配置命令、三种输入、预授权文件和控制终端确认经本地夹具验证；真实副作用已在 DEV-035 接通 |
| DEV-020 | P4 | 完成（受控验证） | 伪终端与模型替身覆盖连续交互、确认／澄清、模式与权限切换、取消和信号；人工等待暂停活跃计时 |
| DEV-021 | P4 | 完成（运行时与 CLI 受控验证） | 用户级／工作区指令与 Skill 发现、作用域、哈希和刷新通过本地测试；P5 已补充来源持久化 |
| DEV-022 | P5 | 完成（独立存储与受控验证） | SQLite WAL／FULL／外键、事务、历史、意图／结果及事件序号通过本地验证；CLI 接线属 DEV-023 至 DEV-025 |
| DEV-023 | P5 | 完成（正常会话受控验证） | 列表、详情、交互恢复、再次运行、配置指纹和工作区关联已接入；崩溃中断恢复归 DEV-025 |
| DEV-024 | P5 | 完成（受控验证） | 跨进程会话锁、并行查询、初始化竞争、迁移备份及损坏／高版本拒绝已验证 |
| DEV-025 | P5 | 完成（受控验证） | 事务恢复、unknown／悬空消息、结果保存故障与 PLAN 目录边界已验证；后台任务关联归 DEV-035 |
| DEV-026 | P6 | 完成（本机受控验证） | JSON／JSONL、持久事件、控制终端、退出码与恢复序号已验证 |
| DEV-027 | P6 | 完成（本机受控验证） | 已知凭据遮蔽、日志轮转、执行／审核用量与已确认产物已验证 |
| DEV-028 | P6 | 完成（本机受控验证） | 四类任务、三协议、12 组模式／权限与 AC-01 至 AC-23 证据已索引 |
| DEV-029 | P6 | 完成（文档与本机验证） | 使用指南、无凭据配置和精确命令预授权示例已提供；示例通过 CLI 测试 |
| DEV-030 | P7 | 完成（真实服务小规模联调） | 指定模型三协议直接回答、读取工具及回传通过；见真实模型联调记录 |
| DEV-031 | P7 | 完成 | 原生候选 2840cbb 与正式源码 eaade53 的四目标构建及最低环境解包验收通过，见发行记录 |
| DEV-032 | P7 | 完成 | 24 组基线与 24 项验收证据齐备，双语版本说明及已知限制已整理 |
| DEV-033 | P1／P2 | 完成（受控验证） | 运行内后台管理、七工具、确认、取消、共享预算和假模型续跑经本机测试 |
| DEV-034 | P1／P3 | 完成（受控验证） | 三协议后台调用／结果映射、共享预算、清理与运行内事件快照已验证 |
| DEV-035 | P4／P5 | 完成（受控验证） | CLI 后台控制、持久关联、故障恢复与真实工具经本机受控验证 |
| DEV-036 | P1／P2 | 完成（组件与受控验证） | 英文提示词分层、装配、独立审核、来源对照与离线场景；真实模型效果未评测 |
| DEV-037 | P1 | 完成（受控验证） | 非测试 core 循环、权限及后台接线、取消与持久化故障边界已验证 |

## 更新规则

- 使用待开始、进行中、受阻、待验证、完成五种任务状态；“完成”必须满足路线图的完成标准。
- 每次进入任务时记录当前任务；结束时补充文件／变更定位、检查命令与结果、报告或日志路径及未验证范围。
- 有阻碍时写明事实与影响；待验证或受阻不计为完成。下一项工作按任务依赖选择，不因时间经过自动切换。
- 阶段内所有任务及退出条件满足后，才更新阶段完成。TUI、GUI 和候选扩展不计入 MVP 任务。
- 若任务拆分，在原编号下追加子项并同步依赖；保持任务定义与此处记录一致。

## 交付记录模板

以下是待实际执行后填写的格式，没有任何预填的通过结果。

```text
任务：DEV-xxx
日期：
状态：
变更与定位：
验证命令与结果：
证据路径：
未验证范围／阻碍：
下一任务：
```
