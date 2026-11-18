# 桌面端设计（soyo-desktop）

**依据 D-072（2026-11-18）确立的 F-14 GUI 工程与安全契约维护本文；D-073（2026-11-21）将后端拓扑由进程内链接切换为共享 `soyo helper` 守护进程，D-072 其余条款继续有效。实际实现与验证状态以[开发进度](progress.md)与代码为准，不复用 TUI 验收状态。**

`soyo-desktop` 是 Soyo Harness 的桌面图形界面，位于顶层 `desktop/` 目录，作为与 `soyo-cli`、`soyo-tui` 并列的独立交付物和薄界面层。桌面端不再进程内链接运行时：所有运行时能力经本机 `soyo helper` 守护进程（`crates/soyo-host` + `crates/soyo-ipc`，Unix socket + NDJSON）提供，CLI/TUI/desktop 共享同一后端。

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
- **浏览器实时预览**：执行 `just desktop-preview` 启动 Vite HMR 开发服务，在常规浏览器访问 `http://127.0.0.1:1420/?preview=1&scene=conversation`（支持 `home`、`conversation`、`approval`、`effort`、`usage`、`browser`、`subagent`、`markdown`、`clarification`、`peripheral` 场景，通过 `@tauri-apps/api/mocks` 脱敏替身预览和调整界面）。
- **视觉体系**：遵循 [UI/UX 设计规范](design/desktop-ui.md) 中的 Soyo Dark（`soyo-dark`：四栏布局、近黑底色、`#d9a08e` 强调色与 `#8aa8d8` 信息蓝分工、统一 pill、内联审批卡片、项目分组会话与 `EffortPopover` WebGL2 能量网格滑块）。

## Helper 后端与 IPC 契约

1. **共享 helper 守护进程（D-073）**：`soyo helper` 以 `crates/soyo-host` 的 `HelperServer` 承载 `HostService`（原 `runtime_bridge` 全部运行时逻辑：会话/Run 管理、事件流、审批/澄清 oneshot 往返、外围管理与附件存储）。桌面端 `DesktopState`（`src-tauri/src/client_state.rs`）是 `HelperClient` 的薄封装，方法名与旧进程内服务一一对应；首次使用即 `connect_or_spawn`——socket 不可达时自动以 `soyo helper` 分离子进程拉起并轮询至就绪。
2. **NDJSON 协议**：本机 Unix socket（`~/.soyo/helper.sock`，`SOYO_HELPER_DIR` 可覆盖；Windows 为命名管道 `soyo-helper`）上逐行 JSON 帧，上限 8 MiB/帧。客户端发送 `Request{id,method,params}`、`CancelRequest{id}`、`Shutdown`；服务端首帧 `Hello{version,pid}`，随后按请求 id 回 `Event{id,data}`（流式）、`Response{id,result}` 或 `Error{id,code,message}`，停机前广播 `Bye`。`PROTOCOL_VERSION=1`。
3. **Run 事件流**：`submit_prompt`/`compact_session`/`run_prompt` 为流式方法——服务端为每个请求 id 把 `DesktopStreamEnvelope` 逐条作为 `Event` 帧推送，客户端 `call_streaming` 解复用到 mpsc，`soyo-desktop` 再转发进原有 `tauri::ipc::Channel`，前端消费不变。`run_prompt`（D-074，供 headless `soyo run` 使用）额外在每个事件上推送 `RawSchemaEvent{event}` 变体（`#[specta(skip)]`，不进 TS union）——`--output jsonl` 打印的序列化 schema-v1 `Event` 与原进程内 `StdoutSink` 逐字节一致；`--config`/`--data-dir` 或 helper 不可达时 CLI 回退进程内执行。
4. **生命周期**：`soyo kill` 通过 socket `Shutdown` 帧优雅停止守护进程（不可达时回退 `helper.pid` + SIGTERM）；桌面退出仅断开连接，不杀 helper。过期 socket 文件在绑定时回收；`SOYO_HELPER_BIN` 可覆盖客户端拉起的目标二进制名。
5. **附件预览代理**：`soyo-attachment://` 的草稿与会话媒体存于 helper 进程，Tauri URI 协议处理器经 `fetch_attachment` 方法代取（base64 body），响应安全策略（`nosniff`、`no-store`、白名单 MIME）不变。
6. **请求／响应（`#[tauri::command]`）**：所有前端触发的控制、查询、设置、会话恢复、模块开关、记忆管理与导出调用均通过 Tauri command 提供，由 `specta`／`tauri-specta` 生成强类型 `desktop/src/bindings.ts`；命令实现转发为 helper NDJSON `Request`，返回的 `Error{code,message}` 直接映射回 `DesktopError`。
7. **Run 事件流（`tauri::ipc::Channel`）**：每次运行通过命令参数传入专用 `tauri::ipc::Channel`，按有序信封向前端推送 schema v1 事件（`Event`）、实时思考快照（`ThinkingLog`）、上下文分项估算（`ContextSnapshot`）与人工确认／澄清请求，不使用全局 `app.emit` 广播事件。消费者断开或前端取消时立即触发 `RunCancellation` 并等待后台清理屏障；helper 侧断线时客户端 `fail_all_pending` 以 `helper_disconnected` 收口。
8. **人工交互闭环**：helper 的 `HostService` 实现 `ApprovalHandler` 与 `InputHandler`（基于 `soyo_app::interaction::input_channel`），审批/澄清请求以 `Event` 帧推送、客户端 `reply_approval`/`reply_input` 请求闭环；运行中切换工作模式立即使旧审批以 `stale_approval` 失效，最终权限仍由 `soyo-core` 派发前重查决定。
9. **非阻塞隔离**：打开 SQLite、扫描存储、哈希及文件系统发布等阻塞操作在 helper 内统一通过 `soyo_app::offload_boundary` / `offload` 调度，不阻塞 socket 事件循环与窗口响应。

## 安全与显示边界

- **严格 CSP 与禁远程 URL**：`tauri.conf.json` 配置严格 `app.security.csp`，禁止加载外部脚本或连接远程页面，窗口只加载本地打包资源，不开放 Tauri `fs` 或 `assetProtocol` scope。
- **凭据脱敏与禁止前端落盘**：凭据仅通过 `soyo-core` 的 `FileCredentialStore` 与 `SecretRegistry` 管理；禁止接入 Tauri keyring 插件，禁止将任何凭据、令牌或私钥写入 `localStorage` / `sessionStorage` / IndexedDB。前端凭据管理页面仅获取并展示不含明文密钥的脱敏元数据（`CredentialMetadata`）。所有跨 IPC 的事件、思考快照、工具输出和错误提示均先经 `SecretRegistry` 脱敏。
- **自定义附件预览协议（`soyo-attachment://`）**：通过 `tauri::Builder::register_uri_scheme_protocol` 注册 `soyo-attachment`（并保留兼容别名 `saya-attachment`），仅允许读取当前活动会话已登记的草稿附件白名单或已验证归属当前会话的 `OutputStore` 媒体／文件对象，校验规范路径、禁止符号链接与路径穿越，并附加 `X-Content-Type-Options: nosniff` 与 `Cache-Control: private, no-store`。
- **流式 Markdown 渲染**：前端采用 Streamdown + Shiki + mermaid.js，按 [Markdown 说明](markdown-rendering.md) 的语义支持 `render`/`source` 切换、控制字符净化、HTML 源码安全呈现、代码块与 Mermaid 预算回退（`source fallback`）及脚注双向跳转；四工作模式颜色与 Soyo Dark 统一 pill 对齐（CODE `#4ade80`、WORK `#d9a08e`、PLAN `#a096eb`、BUILD `#fac454`）。

## 打包、签名与自动更新

- **安装包矩阵**：`bundle.targets` 配置 `dmg`、`app`（macOS aarch64/x86_64）、`nsis`（Windows，明确不发 MSI）与 `appimage`（Linux），同时开启 `bundle.createUpdaterArtifacts: true`。
- **自动更新**：集成 `tauri-plugin-updater` 与 Ed25519 签名校验，使用 GitHub Releases 发布的 `latest.json` 作为更新端点；前端 `UpdaterBanner` 发现新版本后展示提示条，需用户显式点击确认才执行 `downloadAndInstall()`（浏览器预览及离线/空开发公钥下自动静默跳过）。
- **CI Secrets 隔离与回归**：Ed25519 签名私钥及 Apple 签名／公证（notarization）凭据全部通过 GitHub Actions Secrets 注入 [.github/workflows/desktop-release.yml](../.github/workflows/desktop-release.yml)；仓库内 [test_desktop_packaging.py](../scripts/release/test_desktop_packaging.py) 作为 `just test-tooling` 的离线回归检查 CSP、零明文密钥、依赖精确锁版、`lucide-react` 约束与工作流矩阵契约。
