# 桌面端设计（soyo-desktop）

**依据 D-072（2026-11-18）确立的 F-14 GUI 工程与安全契约维护本文；D-073（2026-11-18）确立共享 `soyo helper` 守护进程架构；D-075／D-076（2026-11-19）确立 Soyo Dark / Soyo Light 双主题与界面规范；D-077／D-078（2026-11-19）确立多客户端隔离、配置指纹路由、随包 sidecar 与跨平台交付边界，并部分取代 D-072／D-073／D-074 中对应旧条款。实际实现与验证状态以[开发进度](progress.md)与代码为准，不复用 TUI 验收状态。**

`soyo-desktop` 是 Soyo Harness 的桌面图形界面（当前版本 `0.3.0`），位于顶层 `desktop/` 目录，作为与 `soyo-cli`、`soyo-tui` 并列的独立交付物和薄界面层。桌面端不再进程内链接运行时：所有运行时能力经本机 `soyo helper` 守护进程（`crates/soyo-host` + `crates/soyo-ipc`，Unix domain socket + NDJSON）提供，CLI/TUI/desktop 共享同一后端服务实现。

## 工程布局与 Workspace 契约

```text
desktop/
  package.json          React + Vite + TypeScript + Tailwind + Radix + zustand + Streamdown/Shiki/Mermaid
  src/                  前端源码与由 tauri-specta 生成的 bindings.ts
  src-tauri/
    Cargo.toml          crate `soyo-desktop`（独立 workspace member）
    tauri.conf.json     CSP、打包目标与 Updater 端点配置
    src/                Tauri 命令、Channel 桥接、交互往返与自定义附件协议
```

- **不混入核心 crates**：`crates/` 目录不存放任何 GUI 或 Tauri 代码；运行时逻辑不得依赖 Tauri 或前端类型。`crates/soyo-ipc`（协议信封、NDJSON 帧、`HelperClient`、DTO）与 `crates/soyo-host`（`HostService`、`HelperServer`、附件存储、prefs）为无 Tauri 依赖的共享后端 crate。
- **保持默认构建不变**：根 [Cargo.toml](../Cargo.toml) 通过 `default-members` 保留既有 crate 集合（含 `soyo-ipc`、`soyo-host`），常规 `cargo build` / `cargo test` / `scripts/check.sh` 行为不变；`soyo-desktop` 仅通过 `cargo tauri` 或 `-p soyo-desktop` 显式构建与测试。
- **独立安装、共享数据**：用户可单独安装 CLI/TUI、单独安装桌面端或两者并存；两端共享同一 XDG 配置（`config.toml`）、SQLite 会话库（`sessions.sqlite3`）、模块存储（`modules.sqlite3`）、OutputStore、全局记忆与宿主私有凭据存储（`state_dir/credentials`）。
- **图标与界面组件**：所有桌面端界面图标统一使用已固定版本的 `lucide-react`；窗口与应用安装包图标由 1024x1024 RGBA 源生成为 `desktop/src-tauri/icons/` 与 `desktop/public/app-icon.png`。
- **浏览器实时预览**：执行 `just desktop-preview` 启动 Vite HMR 开发服务，在常规浏览器访问 `http://127.0.0.1:1420/?preview=1&scene=conversation`（支持 `home`、`conversation`、`approval`、`effort`、`usage`、`browser`、`subagent`、`markdown`、`clarification`、`peripheral`、`settings` 场景，支持 `&theme=dark|light`、`&tab=<settingsTab>` 与 `&chrome=clean`，通过 `@tauri-apps/api/mocks` 脱敏替身预览和调整界面）。
- **视觉体系**：遵循 [UI/UX 设计规范](design/desktop-ui.md) 中的 `Soyo Dark` 与 `Soyo Light` 双主题（`desktop/src/theme/tokens.css` 单一来源：`#201f1d`/`#1b1a18` 暖炭黑与 `#f7f5ef`/`#efece5` 暖米白、单一可折叠 `248px`/`48px` 侧栏、三档响应式断点、`Source Serif 4` 问候语、`SquaresLoader` 2×2 方块加载指示器、合并式 Composer 胶囊与三组 9 项 Settings）。

## Helper 后端与 IPC 契约

1. **按配置指纹隔离的 helper 守护进程（D-073／D-077）**：`soyo helper` 以 `crates/soyo-host` 的 `HelperServer` 承载 `HostService`（会话／Run 管理、最多 4 个不同会话并发 Run、每会话同时至多 1 个活跃 Run、`RunOwner` 连接归属与交互闭环、外围管理与附件存储）。桌面端 `DesktopState`（`src-tauri/src/client_state.rs`）是 `HelperClient` 的薄封装，首次使用即 `connect_or_spawn`；当版本（`CARGO_PKG_VERSION`）或 `PROTOCOL_VERSION` 不匹配且 helper 空闲（无活跃 Run 且无其他客户端）时自动重启切换，断线后 `call_rpc` 与桌面 UI 重连入口自动重建连接。
2. **端点坐标、权限与访问控制**：默认 socket 位于 `<state_dir>/helper/<fingerprint>.sock`（其中 `<fingerprint>` 为规范化 `config_path` 与 `data_dir` 的 10 位小写十六进制哈希，`SOYO_HELPER_DIR` 可覆盖）。`<state_dir>/helper/` 强制 `0700` 且校验属主，socket 与 `<fingerprint>.pid`（记录 `pid`、`started_at_id`、`exe_path`、`fingerprint`）强制 `0600`；并发绑定经 `<fingerprint>.lock` 的 `flock` 排他协调并先探测旧 socket 是否存活；`accept` 后通过 `getpeereid`／`SO_PEERCRED` 校验对端 euid 与当前用户一致；读取循环在流式解码阶段即强制执行 8 MiB 单帧上限。客户端仅在无法连接或无法拉起 helper 时才回退进程内执行；`--config` 与 `--data-dir` 同样按配置指纹路由到对应 helper 实例。`PROTOCOL_VERSION=1`。
3. **Run 事件流与调用方环境快照**：`submit_prompt`、`compact_session`、`run_prompt` 为流式方法——每个 Run 绑定发起连接的 `(conn_id, req_id)`，转向、压缩、`reply_approval`、`reply_input` 仅接受所属连接；每次提交与运行携带调用方环境快照（含 `cwd`、环境变量与代理/凭据引用），独立配置多客户端互不串扰。桌面端每次 `submit_prompt` 显式携带 `session_id`（不一致返回 `session_changed`，禁止跨会话静默转向）。`run_prompt` 与 `raw_events:true` 推 `RawSchemaEvent{event}` 与辅助 Thinking/Context 信封，保证 CLI `--output jsonl`/json/text 与 TUI `RemotePrepared`/`RemoteHandle` 行为一致。
4. **生命周期、全局记忆与密钥隔离**：`soyo kill` 经 `<fingerprint>.sock` 发送 `Shutdown` 帧停机（PID 回退停机前校验 `/proc` 或 `ps` 进程启动时间与可执行路径）；子进程句柄显式回收避免僵尸进程；`helper` 绑定后定期比对 socket 路径 `(dev,ino)`，删除或重绑即自退。向导写入的环境叠加文件位于 `<state_dir>/secrets/env`（`0700`/`0600`，纳入文件工具与原生沙箱私有受保护路径，启动加载并注册入 `SecretRegistry`；若存在旧 `~/.soyo/env` 则一次性迁移清理）。全局记忆根判定改为仅在 `.soyo` 包含 `MEMORY.md` 或 `memory/` 时优先选用，否则兼容已有 `.saya` 记忆根，helper 启动不再空建 `~/.soyo`。用量统计 `get_usage_stats` 接受客户端传入 `utc_offset_minutes` 按本地日分桶。
5. **附件预览代理**：`soyo-attachment://` 的草稿与会话媒体存于 helper 进程，Tauri URI 协议处理器经 `fetch_attachment`（含正确百分号解码与边界校验，无内存泄漏）代取 base64 负载；暂存超限返回明确错误，不静默清空既有草稿。
6. **请求／响应（`#[tauri::command]`）与频道流**：所有前端控制、设置（含 `get_helper_status`、`update_desktop_prefs`）、会话、模块、记忆与导出均由 `specta`／`tauri-specta` 生成 `desktop/src/bindings.ts`；Run 事件走专用 `tauri::ipc::Channel`，消费端断开或前端取消触发 `CancelRequest{id}` 取消所属 Run，所属连接意外断开时宿主取消其名下全部 Run 并以拒绝／不作答收口挂起审批与澄清。
7. **非阻塞隔离**：打开 SQLite、扫描存储、读写 `desktop_prefs.json`／全局指令与读写 `fetch_attachment` 均经 `soyo_app::offload_boundary` / `spawn_blocking` 调度，不阻塞 socket 事件循环。

## 安全与显示边界

- **严格 CSP 与禁远程 URL**：`tauri.conf.json` 配置严格 `app.security.csp`，禁止加载外部脚本或连接远程页面，窗口只加载本地打包资源，不开放 Tauri `fs` 或 `assetProtocol` scope。
- **凭据脱敏与前端零落盘**：凭据仅通过 `soyo-core` 的 `FileCredentialStore`、`SecretRegistry` 与受保护的 `<state_dir>/secrets/env` 管理；禁用 Tauri keyring 插件，禁止 `sessionStorage`、`indexedDB` 或将凭据写入 `localStorage`（按 D-076，`localStorage` 仅限缓存无敏感性的 `"soyo.theme"` 首帧主题以消除闪烁，由 `desktop/public/theme-init.js` 与 `test_desktop_packaging.py` 强制校验）。前端凭据页面只接收脱敏后的 `CredentialMetadata`。
- **自定义附件预览协议（`soyo-attachment://`）**：通过 `tauri::Builder::register_uri_scheme_protocol` 注册 `soyo-attachment`（兼容 `saya-attachment`），仅允许读取当前活动会话已登记的草稿或已验证归属当前会话的 `OutputStore` 对象，校验规范路径、禁符号链接与路径穿越，并附加 `X-Content-Type-Options: nosniff` 与 `Cache-Control: private, no-store`。
- **流式 Markdown 与语义配色**：前端采用 Streamdown + Shiki（深浅双主题）+ mermaid.js（`dark`/`default`）实时跟随主题切换；四工作模式颜色与 `soyo-tui` 共用 Dark 表（WORK `#6aaefc`、CODE `#86cfa0`、PLAN `#bea3f5`、BUILD `#efb25e`，对应 Soyo Light 为 `#235fbb` / `#2a6e45` / `#6d45c2` / `#88580b`）；权限显示区分 `Auto`（中性 `--text-secondary`）、`Manual approval`（信息蓝 + `Hand` 图标）与常驻危险红标识 `Bypass permissions`（`--danger` + `ShieldAlert`）。

## 打包、签名与自动更新

- **随包 sidecar 与安装包矩阵（D-078）**：桌面端通过 `bundle.externalBin: ["binaries/soyo"]` 随安装包附带同版本 `soyo` 二进制，`HelperClient` 优先复用可执行文件同级目录的 `soyo`，实现开箱即用；`bundle.targets` 收敛为 `dmg`、`app`（macOS `aarch64`/`x86_64`）与 `appimage`（Linux `x86_64`），移除 Windows NSIS 交付承诺，并保持 `tauri.conf.json` 与 `desktop/package.json` 版本号与 workspace（`0.3.0`）一致。
- **自动更新与触发契约**：集成 `tauri-plugin-updater` + Ed25519 签名校验（消费 GitHub Releases 的 `latest.json`）；[.github/workflows/desktop-release.yml](../.github/workflows/desktop-release.yml) 仅由 `desktop-v*` tag 触发，支持手动 `workflow_dispatch` 且手动运行与预发布 tag 一律产出 Draft Release，缺少 `TAURI_UPDATER_PUBLIC_KEY` 时发布作业直接失败（fail-fast）。仓库内 [test_desktop_packaging.py](../scripts/release/test_desktop_packaging.py) 作为 `just test-tooling` 的离线回归校验 CSP、禁止存储策略、`lucide-react` 零 `<svg` 约束、`externalBin`、版本一致性及发布工作流矩阵契约。
