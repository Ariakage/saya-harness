# Saya Harness N5 Browser

**开发线说明，当前保持 `version = 0.2.0` 且未创建／发布新版本标签。生产 Masked 截图已在定制 `linux-arm64` 引擎、三补丁 worker、`colima-n5cap` 联合验收（`--require-screenshot` 22/22 passed）及 `n5arm22` Linux ARM64 原生实跑通过；固定源码 `5fff1c4` 已完成四平台远端终验（`CI 44044030536`、`N5 native acceptance 44044034857`、`Release candidates 44044045558` 全 4/4 通过，含 `linux-x86_64` 与 `linux-aarch64` 原生 Chromium sandbox 及三补丁 worker 协议实跑），非 `linux-arm64` 目标架构定制 Chromium 构建分发继续作为显式边界保留。**

## 中文

N5 将受控网页读取与托管 Chromium 接入共享运行时。CLI、文本会话与 TUI 复用工具注册、权限、审批、OutputStore、后台任务和恢复契约，不提供绕过运行时的独立浏览器入口。

### 已实现

- 默认关闭的 `fetch_url` 与显式 endpoint `web_search`；有界正文、重定向与 DNS／地址检查、低信任来源和输出检索。
- 固定版本 Playwright MCP worker、schema v2 安装身份校验、Run 私有临时会话及有界关闭。除七个命名文件外，绑定 Node、worker、Chromium 三棵树的成员、权限、链接及文件内容；文件在同一 FD 上双读复核，保留取消、期限和元数据边界。模型只看宿主注册的操作，不能调用任意脚本或安装工具。
- 成功页面调用后重新采样 sandbox 证据，close 后作废旧证据；启动核验或清理超时／失败时保留句柄并阻止连接复用，未确认清理明确返回 unknown。
- 显式专用 Colima VM 后端，保留宿主隔离与 Chromium 自有 sandbox。创建工具不激活宿主全局客户端上下文；浏览器通过 VM 私有网络命名空间、SSH 反向通道和宿主 broker 出网，不回退默认网络。
- Linux VM 宿主通道使用 Bubblewrap、原有 Seccomp 与固定目标双桥，保留 IPv4／IPv6 字面地址，只读暴露三个 SSH 文件及合成账号视图。空 broker 就绪探测不消耗 Run 连接额度，relay 清理核对 socket 身份；紧凑私有目录与短 socket 名已通过 64 字节 TMPDIR 的真实准备回归，保留完整 UUID 位。缺原生前提时失败关闭，不回退 Host。
- 固定 worker 的可重放补丁使受限 isolated stdio 直接使用进程内 Browser，提供不进入模型工具目录的固定 `saya/browser/processInfo` 进程角色查询与 `saya/browser/captureScreenshot` 私有预捕获遮蔽通道（仅声明于 isolated 进程内通道，`browser_take_screenshot` 等模型工具均不开放遮蔽或脚本）；取消释放、严格 MCP 保留元数据、13 MiB wire 上限、PNG/IHDR 核验、observed URL/origin/陈旧文档校验及 sandbox 失败清理均已通过回归与四平台实跑验证。
- Rust 宿主连接提供同一固定私有角色查询与预捕获遮蔽请求，严格校验版本、PID、namespace、Base64/PNG/IHDR、observed URL 与字节边界；取消／超时通知绑定同一请求，通知不明或 future 丢弃使连接失效，不重放。生产 launcher 在实际生成进程前保留已认证 Node／Chromium 文件描述符与 SHA-256，使用有界定位读取与前后完整 `WorkerImageIdentity` 复核（覆盖同 tick 时间戳不变的本地改写）。native 与 VM 的 `ChromiumSandboxInspector` 已通过 `inspect_connection`（及 `NativeSandboxContext`）接入强角色与内核隔离联合核验，并已在固定源码 `5fff1c4` 完成四平台远端终验（`ubuntu-22.04`／`ubuntu-22.04-arm` 各 1730 passed 含 `browser_vm_host_linux` 与 Playwright rev 1254 原生 Chromium sandbox `renderer_pid=73 no_new_privs=true seccomp=2 stderr_initialize_sandbox=true`、`macos-15`／`macos-15-intel` 各 1715 passed，四端 tooling 172 passed 及 candidate/RC 二进制 `a3e640ee…`/`09a13bae…`/`8b67b35b…`/`fa525f8e…` 逐字节一致），叠加本机专用 `colima-n5cap` VM（`browser_screenshot_real` 12/12、`--require-screenshot` 22/22 passed）与独立 `n5arm22` ARM64 VM（`browser_worker_real` 2/2 含 `1280x720` Masked PNG）实跑。macOS Seatbelt 仍不支持原生浏览器（继续经专用 Linux VM 使用）。
- 固定 Chromium `6449d9f9a6a906edff724b7b220199b3fde4615c` 加封存 23 文件捕获补丁（`df4c0fb6...`，`patch-identity-263.json` `53db522d...`）已完成 Mac ARM 到 Linux ARM64 的构建与闭包打包，输出 `headless_shell` ELF `501,816,112 B`（SHA-256 `d3c691a5...`），已部署至 `colima-n5cap`（`/home/ariakage.guest/saya-capture-fixed-265`）与 `n5arm22`（`/home/saya-test/native-capture-265`）并完成像素级遮蔽与错误边界实跑；当运行时遇到未修补 Chromium、非 isolated 通道或未确认 `masked=true` 时，仍失败关闭返回 `browser_capability_disabled`。
- 精确 origin 绑定的凭据槽、次数与撤销限制、顶层文档校验；凭据只在最终填充边界解析，填充不自动提交。
- 点击绑定的下载与同 Session artifact 上传；文件名、路径、大小、类型、摘要、私有 staging、产物 lease 与 GC pin 均受限。File 不注入模型媒体。
- 有界后台导航、等待和下载。已派发副作用的未知结果不自动重放，恢复后重新观察页面并使用新的文档引用。
- CLI／TUI 的动作、审批、来源、文档版本、输出引用及产物展示。填充审批只显示字节数，详情保留按键、按钮和上传引用。
- 宿主截图产物及三协议原生图片链路：仅具备真实截图前遮蔽能力的 provider 可用。按 profile 图片能力启用，工具图片最多 8 个，原始字节与 base64 聚合各不超过 64 MiB；上下文投影后解析，计数与派发共享快照。

### 兼容性与限制

- Web／Browser 必须显式配置，不自动安装、下载或更新浏览器运行时。配置、版本与 VM 前提见 [Browser](../browser.md) 和[配置说明](../configuration.md)。
- 安装身份 v2 是不兼容变更：旧 v1 manifest 被拒绝，必须在新远端根重新 provision 并更新显式路径与摘要；已有根不会被覆盖。树校验不是原子／不可变安装快照，也不证明定制 Chromium 的构建来源；历史验收继续归属其原源码与输入。
- Rust 的 `NetworkPolicy::LoopbackPort` 现在要求显式 `host` 字段；Linux 保留该回环字面地址，Seatbelt 仍按 localhost 端口限制。CLI 的显式 VM 配置字段不变。
- PLAN 在全部权限模式下拒绝主动 Browser 工具。`auto` 的写动作经过独立核查；Manual Approval 逐工具确认；bypass 不绕过 PLAN、显式 deny、网络和 sandbox 硬边界。
- 当前真实 Browser 证据涵盖四平台固定源码 `5fff1c4` 远端矩阵（`ubuntu-22.04`／`ubuntu-22.04-arm`／`macos-15`／`macos-15-intel`：`CI 44044030536`、`N5 native acceptance 44044034857`、`Release candidates 44044045558` 全通过）、显式预置的 Linux ARM 专用 `colima-n5cap` VM，以及独立 Linux ARM64 `n5arm22` 上的 Masked PNG 原生实跑（通过 `NativeSandboxContext`＋`saya/browser/processInfo`＋procfs／namespace／递增 Seccomp／`InitializeSandbox` 联合核验）。native Seatbelt／Bubblewrap 无法完成 Chromium 内层 sandbox 初始化时明确失败，不使用 `--no-sandbox` 绕过；Linux 双架构远端 Chromium sandbox 验证基于 Playwright rev 1254 stock Chromium。
- `linux-x86_64` 与 `linux-aarch64` 已在 `5fff1c4` 各自通过 `1730 passed`、`172 passed` 工具回归、三补丁 worker、`browser_vm_host_linux` 与真实 Playwright rev 1254 Chromium sandbox（`no_new_privs=true seccomp=2 stderr_initialize_sandbox=true`），`macos-15` 与 `macos-15-intel` 通过 `1715 passed` 及 Seatbelt／CLI／TUI／worker 回归；定制 Chromium 捕获引擎构建与像素遮蔽实测目前仍仅覆盖 `linux-arm64`（`colima-n5cap` 与 `n5arm22`），非 `linux-arm64` 目标架构定制引擎构建分发及新 semver 发行标签均未执行。
- 固定 DEPS 的 Mac ARM 宿主 Clang／LLD 24、Rust 工具链及 Linux ARM sysroot／运行库已通过独立 C++／Rust 跨目标执行探针。Rust 探针为 `no_core`，不证明目标 std；项目 Rust 版本不变。
- `browser_screenshot` 的生产遮蔽依赖受限私有 `saya/browser/captureScreenshot` 与配套定制 `chrome-headless-shell`；若在未打补丁的 stock 引擎或非 isolated 通道使用，运行时仍失败关闭并返回 `browser_capability_disabled`，不会保存或返回未遮蔽截图。固定脚本或页面 CSS 不替代原生渲染遮蔽。
- 截图元数据的 `url_observation = last_confirmed` 表示最近一次 provider 确认 URL，不保证页面自主导航后的瞬时来源。
- `artifact_ref` 是公开的不透明句柄，可与存储引用值相同，但不包含宿主路径或凭据，不授予跨 Session 访问。宿主 `media_refs` 字段只承担可信媒体选择与 GC pin。
- 未接入个人现有浏览器、持久登录 profile 复用、远程浏览器服务或其他浏览器引擎。真实外网与付费模型不由上述受控矩阵替代。

测试结果与未满足项以 [N5 验收记录](../n5-validation.md) 为准；本说明不创建发行或修改版本号。

## English

N5 connects controlled web reads and managed Chromium to the shared runtime. CLI, text chat, and TUI reuse tool registration, permissions, approvals, OutputStore, background jobs, and recovery contracts; there is no separate browser entry point that bypasses those controls.

### Implemented

- Opt-in `fetch_url` and explicitly configured `web_search`, with bounded content, redirect and DNS/address checks, low-trust provenance, and retained-output retrieval.
- A version-pinned Playwright MCP worker, schema v2 installation identity verification, Run-private temporary sessions, and bounded shutdown. In addition to seven named files, it binds members, permissions, links, and file contents in the Node, worker, and Chromium trees. Files are read twice through the same descriptor, retaining cancellation, deadline, and metadata checks. The model receives only host-registered operations, not arbitrary scripting or installation tools.
- Sandbox evidence is sampled again after successful page calls and invalidated on close. Startup verification or cleanup timeouts/failures retain cleanup handles and block connection reuse; unconfirmed cleanup is explicitly unknown.
- An explicit dedicated Colima VM backend preserving host isolation and Chromium's own sandbox. The creation tool leaves the host's global client context unchanged. Browser egress uses a private VM network namespace, a reverse SSH channel, and the host broker, with no default-network fallback.
- The Linux VM host channel uses Bubblewrap, the existing Seccomp filter, and two fixed-destination bridges. It preserves IPv4/IPv6 literals and exposes only three read-only SSH files plus synthetic account data. Empty broker readiness probes do not consume the Run connection budget, and relay cleanup checks socket identity. Compact private directory names and short socket names retain every UUID bit and pass real preparation with a 64-byte TMPDIR. Missing native prerequisites fail closed without falling back to Host.
- Replayable pinned-worker patches use an in-process Browser for restricted isolated stdio and expose host-private `saya/browser/processInfo` and `saya/browser/captureScreenshot` handlers (never listed in `tools/list`, with `playwright-masked-capture.patch` delegating pre-capture masking through Blink/HeadlessExperimental without exposing custom scripts or selectors). Cancellation cleanup, strict MCP progress metadata, 13 MiB stdio wire framing, PNG/IHDR verification, observed URL/origin/staleness checks, and sandbox failure cleanup pass unit and protocol regressions.
- Rust host connections wire both private queries with strict schema, PID, namespace, base64/PNG/IHDR, observed URL, and byte bounds. Production `ManagedProcessLauncher::launch` retains authenticated Node/Chromium descriptors and SHA-256 digests (`PinnedWorkerImage`), re-hashing bounded positioned reads alongside full before/after checks immediately before spawn. Both native and VM `ChromiumSandboxInspector` connect via `inspect_connection` across the four-platform fixed-source `5fff1c4` acceptance (`CI 44044030536`, `N5 native acceptance 44044034857`, `Release candidates 44044045558` all passed 4/4: `ubuntu-22.04`/`ubuntu-22.04-arm` 1730 passed including `browser_vm_host_linux` and native Playwright rev 1254 Chromium sandbox with `renderer_pid=73 no_new_privs=true seccomp=2 stderr_initialize_sandbox=true`; `macos-15`/`macos-15-intel` 1715 passed; 172 Python tooling tests and byte-identical RC binaries `a3e640ee...`/`09a13bae...`/`8b67b35b...`/`fa525f8e...`), plus `colima-n5cap` (`browser_screenshot_real` 12/12 passed, `acceptance.py --require-screenshot` 22/22 passed with 0 disabled) and `n5arm22` (`browser_worker_real` 2/2 passed including `1280x720` native Masked PNG). Native macOS Seatbelt remains unsupported for this browser bundle and continues to run via the dedicated Linux VM backend.
- The pinned Chromium `6449d9f9a6a906edff724b7b220199b3fde4615c` build with the sealed 23-file fixed capture patch (`df4c0fb6...`, `patch-identity-263.json` `53db522d...`) built cleanly into the Linux ARM64 `headless_shell` ELF (`501,816,112 B`, SHA-256 `d3c691a5...`), was provisioned into `colima-n5cap` (`saya-capture-fixed-265`) and `n5arm22` (`native-capture-265`), and passed pixel masking and credential-hiding regressions while failing closed as `browser_capability_disabled` if unpatched engines or unmasked payloads are encountered.
- Exact-origin credential slots with usage and revocation limits and top-level document checks. Values are resolved only at the final fill boundary; filling never submits a form automatically.
- Click-bound downloads and same-Session artifact uploads, with filename, path, size, type, digest, private staging, lease, and GC-pin boundaries. File artifacts never become model media.
- Bounded background navigation, waits, and downloads. Dispatched side effects with unknown outcomes are not replayed automatically; recovery requires a fresh page observation and new document references.
- CLI/TUI action, approval, source, document revision, output-reference, and artifact views. Fill approvals show byte counts instead of content, while details retain keys, buttons, and upload references.
- Host-side screenshot artifacts and native image blocks for all three protocols, available only with a provider that truly masks sensitive fields before capture. Profile-gated tool images are limited to eight per request, with both raw and base64 totals capped at 64 MiB. Media resolves after context projection, sharing one snapshot between counting and dispatch.

### Compatibility and limitations

- Web/Browser features require explicit configuration. No runtime is installed, downloaded, or updated automatically. See [Browser](../browser.md) and [configuration](../configuration.md) for pinned versions and VM prerequisites.
- Installation identity v2 is a breaking change: v1 manifests are rejected. Re-provision into a new remote root and explicitly update paths and the digest; existing roots are not overwritten. Tree verification is neither an atomic/immutable installation snapshot nor proof of a custom Chromium build's provenance. Historical acceptance remains bound to its original sources and inputs.
- The Rust `NetworkPolicy::LoopbackPort` variant now requires an explicit `host` field. Linux preserves that loopback literal; Seatbelt still restricts localhost ports. The explicit CLI VM configuration fields are unchanged.
- PLAN denies active Browser tools in every permission mode. In auto mode, write actions undergo independent review; Manual Approval confirms every tool. Bypass does not override PLAN, explicit deny rules, network restrictions, or sandbox boundaries.
- Current real Browser evidence covers the four-platform `5fff1c4` remote matrix (`ubuntu-22.04`, `ubuntu-22.04-arm`, `macos-15`, and `macos-15-intel` across `CI 44044030536`, `N5 native acceptance 44044034857`, and `Release candidates 44044045558`), an explicitly provisioned Linux ARM VM (`colima-n5cap`), and custom Masked PNG `browser_worker_real` execution on `n5arm22` (verifying `NativeSandboxContext`, `saya/browser/processInfo`, procfs/namespace ancestry, strictly increasing Seccomp filters, and `InitializeSandbox` stderr). Native Seatbelt/Bubblewrap paths still fail closed whenever Chromium's inner sandbox cannot initialize (`--no-sandbox` is never used); remote Linux Chromium sandbox runs use Playwright revision 1254 stock Chromium.
- Both `ubuntu-22.04` (`linux-x86_64`) and `ubuntu-22.04-arm` (`linux-aarch64`) pass 1730 Rust tests, 172 tooling checks, the three-patch worker contracts, `browser_vm_host_linux`, and native Chromium sandbox checks on `5fff1c4`, while `macos-15` and `macos-15-intel` pass 1715 Rust tests, 172 tooling checks, Seatbelt isolation, and CLI/TUI/worker regressions; however, custom pre-capture Chromium `headless_shell` binaries and pixel-masking end-to-end runs remain built and verified for `linux-arm64` only (`colima-n5cap` and `n5arm22`), and `version = "0.2.0"` remains unchanged without a new published release tag.
- The fixed DEPS Mac ARM host Clang/LLD 24, Rust toolchain, and Linux ARM sysroot/runtime pass a standalone C++/Rust cross-target execution probe. Its Rust portion uses `no_core`; this does not verify the target std. The project's Rust version is unchanged.
- Production `browser_screenshot` requires the restricted `saya/browser/captureScreenshot` worker patch and matching custom Chromium capture engine; when run against unpatched stock engines or non-isolated transports, calls continue to fail closed with `browser_capability_disabled` rather than exposing unmasked images. Fixed scripts or page CSS are not treated as a complete privacy guarantee without native render masking.
- Screenshot metadata uses `url_observation = last_confirmed`: the most recent provider-confirmed URL, not an atomic observation after autonomous page navigation.
- Public `artifact_ref` handles may share the storage-reference value but contain neither host paths nor credentials and grant no cross-Session access. The host `media_refs` field controls trusted media selection and GC pinning.
- Existing personal browsers, persistent login-profile reuse, remote browser services, and other engines are not integrated. Local and CI controlled matrices do not replace real external-network or paid-model testing.

Actual results and unmet requirements are recorded in [N5 validation](../n5-validation.md). These development notes do not publish a release or change the version number.
