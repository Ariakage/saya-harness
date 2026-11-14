# Saya Harness N5 Browser

**开发线说明，尚未发布；正式版本号和新增四平台发行证据未冻结。生产截图仍禁用，不能据此宣称 N5 全范围验收完成。**

## 中文

N5 将受控网页读取与托管 Chromium 接入共享运行时。CLI、文本会话与 TUI 复用工具注册、权限、审批、OutputStore、后台任务和恢复契约，不提供绕过运行时的独立浏览器入口。

### 已实现

- 默认关闭的 `fetch_url` 与显式 endpoint `web_search`；有界正文、重定向与 DNS／地址检查、低信任来源和输出检索。
- 固定版本 Playwright MCP worker、身份摘要校验、Run 私有临时会话及有界关闭。模型只看宿主注册的操作，不能调用任意脚本或安装工具。
- 成功页面调用后重新采样 sandbox 证据，close 后作废旧证据；启动核验或清理超时／失败时保留句柄并阻止连接复用，未确认清理明确返回 unknown。
- 显式专用 Colima VM 后端，保留宿主隔离与 Chromium 自有 sandbox。浏览器通过 VM 私有网络命名空间、SSH 反向通道和宿主 broker 出网，不回退默认网络。
- Linux VM 宿主通道使用 Bubblewrap、原有 Seccomp 与固定目标双桥，保留 IPv4／IPv6 字面地址，只读暴露三个 SSH 文件及合成账号视图。空 broker 就绪探测不消耗 Run 连接额度，relay 清理核对 socket 身份；紧凑私有目录与短 socket 名已通过 64 字节 TMPDIR 的真实准备回归，保留完整 UUID 位。缺原生前提时失败关闭，不回退 Host。
- 精确 origin 绑定的凭据槽、次数与撤销限制、顶层文档校验；凭据只在最终填充边界解析，填充不自动提交。
- 点击绑定的下载与同 Session artifact 上传；文件名、路径、大小、类型、摘要、私有 staging、产物 lease 与 GC pin 均受限。File 不注入模型媒体。
- 有界后台导航、等待和下载。已派发副作用的未知结果不自动重放，恢复后重新观察页面并使用新的文档引用。
- CLI／TUI 的动作、审批、来源、文档版本、输出引用及产物展示。填充审批只显示字节数，详情保留按键、按钮和上传引用。
- 宿主截图产物及三协议原生图片链路：仅具备真实截图前遮蔽能力的 provider 可用。按 profile 图片能力启用，工具图片最多 8 个，原始字节与 base64 聚合各不超过 64 MiB；上下文投影后解析，计数与派发共享快照。

### 兼容性与限制

- Web／Browser 必须显式配置，不自动安装、下载或更新浏览器运行时。配置、版本与 VM 前提见 [Browser](browser.md) 和[配置说明](configuration.md)。
- Rust 的 `NetworkPolicy::LoopbackPort` 现在要求显式 `host` 字段；Linux 保留该回环字面地址，Seatbelt 仍按 localhost 端口限制。CLI 的显式 VM 配置字段不变。
- PLAN 在全部权限模式下拒绝主动 Browser 工具。`auto` 的写动作经过独立核查；Manual Approval 逐工具确认；bypass 不绕过 PLAN、显式 deny、网络和 sandbox 硬边界。
- 当前真实 Browser 证据来自 macOS ARM 宿主与显式预置的 Linux ARM 专用 VM。native Seatbelt／Bubblewrap 无法完成 Chromium 内层 sandbox 初始化时明确失败，不使用 `--no-sandbox` 绕过。VM 证据不等于 native 或四平台 Browser 支持。
- 固定 worker 的截图接口没有真实截图前遮蔽契约，生产 `browser_screenshot` 不注册，直接调用返回 `browser_capability_disabled`；受控 provider 的成功测试不能冒充生产截图验收。固定脚本或页面 CSS 也不能被当成动态页面的完整隐私保证。
- 截图元数据的 `url_observation = last_confirmed` 表示最近一次 provider 确认 URL，不保证页面自主导航后的瞬时来源。
- `artifact_ref` 是公开的不透明句柄，可与存储引用值相同，但不包含宿主路径或凭据，不授予跨 Session 访问。宿主 `media_refs` 字段只承担可信媒体选择与 GC pin。
- 未接入个人现有浏览器、持久登录 profile 复用、远程浏览器服务或其他浏览器引擎。真实外网、付费模型和新增四平台发行验收不由本机受控矩阵替代。

测试结果与未满足项以 [N5 验收记录](n5-validation.md) 为准；本说明不创建发行或修改版本号。

## English

N5 connects controlled web reads and managed Chromium to the shared runtime. CLI, text chat, and TUI reuse tool registration, permissions, approvals, OutputStore, background jobs, and recovery contracts; there is no separate browser entry point that bypasses those controls.

### Implemented

- Opt-in `fetch_url` and explicitly configured `web_search`, with bounded content, redirect and DNS/address checks, low-trust provenance, and retained-output retrieval.
- A version-pinned Playwright MCP worker, identity digest verification, Run-private temporary sessions, and bounded shutdown. The model receives only host-registered operations, not arbitrary scripting or installation tools.
- Sandbox evidence is sampled again after successful page calls and invalidated on close. Startup verification or cleanup timeouts/failures retain cleanup handles and block connection reuse; unconfirmed cleanup is explicitly unknown.
- An explicit dedicated Colima VM backend preserving host isolation and Chromium's own sandbox. Browser egress uses a private VM network namespace, a reverse SSH channel, and the host broker, with no default-network fallback.
- The Linux VM host channel uses Bubblewrap, the existing Seccomp filter, and two fixed-destination bridges. It preserves IPv4/IPv6 literals and exposes only three read-only SSH files plus synthetic account data. Empty broker readiness probes do not consume the Run connection budget, and relay cleanup checks socket identity. Compact private directory names and short socket names retain every UUID bit and pass real preparation with a 64-byte TMPDIR. Missing native prerequisites fail closed without falling back to Host.
- Exact-origin credential slots with usage and revocation limits and top-level document checks. Values are resolved only at the final fill boundary; filling never submits a form automatically.
- Click-bound downloads and same-Session artifact uploads, with filename, path, size, type, digest, private staging, lease, and GC-pin boundaries. File artifacts never become model media.
- Bounded background navigation, waits, and downloads. Dispatched side effects with unknown outcomes are not replayed automatically; recovery requires a fresh page observation and new document references.
- CLI/TUI action, approval, source, document revision, output-reference, and artifact views. Fill approvals show byte counts instead of content, while details retain keys, buttons, and upload references.
- Host-side screenshot artifacts and native image blocks for all three protocols, available only with a provider that truly masks sensitive fields before capture. Profile-gated tool images are limited to eight per request, with both raw and base64 totals capped at 64 MiB. Media resolves after context projection, sharing one snapshot between counting and dispatch.

### Compatibility and limitations

- Web/Browser features require explicit configuration. No runtime is installed, downloaded, or updated automatically. See [Browser](browser.md) and [configuration](configuration.md) for pinned versions and VM prerequisites.
- The Rust `NetworkPolicy::LoopbackPort` variant now requires an explicit `host` field. Linux preserves that loopback literal; Seatbelt still restricts localhost ports. The explicit CLI VM configuration fields are unchanged.
- PLAN denies active Browser tools in every permission mode. In auto mode, write actions undergo independent review; Manual Approval confirms every tool. Bypass does not override PLAN, explicit deny rules, network restrictions, or sandbox boundaries.
- Current real Browser evidence covers a macOS ARM host and an explicitly provisioned Linux ARM VM. Native Seatbelt/Bubblewrap paths fail explicitly when Chromium's inner sandbox cannot initialize; `--no-sandbox` is not used. VM evidence is not native or four-platform Browser acceptance.
- The pinned worker has no reliable pre-capture masking contract. Production `browser_screenshot` is not registered, and direct calls return `browser_capability_disabled`. Successful controlled-provider tests do not establish production screenshot support. A fixed script or page CSS is not a complete privacy guarantee for dynamic pages.
- Screenshot metadata uses `url_observation = last_confirmed`: the most recent provider-confirmed URL, not an atomic observation after autonomous page navigation.
- Public `artifact_ref` handles may share the storage-reference value but contain neither host paths nor credentials and grant no cross-Session access. The host `media_refs` field controls trusted media selection and GC pinning.
- Existing personal browsers, persistent login-profile reuse, remote browser services, and other engines are not integrated. Local controlled matrices do not replace real external-network, paid-model, or new four-platform release acceptance.

Actual results and unmet requirements are recorded in [N5 validation](n5-validation.md). These development notes do not publish a release or change the version number.
