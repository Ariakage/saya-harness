# 桌面端设计（soyo-desktop）

**依据 D-072（2026-11-18）确立的 F-14 GUI 工程与安全契约维护本文。实际实现与验证状态以[开发进度](progress.md)与代码为准，不复用 TUI 验收状态。**

`soyo-desktop` 是 Soyo Harness 的桌面图形界面，位于顶层 `desktop/` 目录，作为与 `soyo-cli`、`soyo-tui` 并列的独立交付物和薄界面层，直接进程内复用 `soyo-app` 与 `soyo-core` 提供的运行时能力。

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

- **不混入核心 crates**：`crates/` 目录不存放任何 GUI 或 Tauri 代码；运行时逻辑不得依赖 Tauri 或前端类型。
- **保持默认构建不变**：根 [Cargo.toml](../Cargo.toml) 通过 `default-members` 保留既有六个 crate（`soyo-contract`、`soyo-fs`、`soyo-core`、`soyo-cli`、`soyo-app`、`soyo-tui`），常规 `cargo build` / `cargo test` / `scripts/check.sh` 行为不变；`soyo-desktop` 仅通过 `cargo tauri` 或 `-p soyo-desktop` 显式构建与测试。
- **独立安装、共享数据**：用户可单独安装 CLI/TUI、单独安装桌面端或两者并存；两端共享同一 XDG 配置（`config.toml`）、SQLite 会话库（`sessions.sqlite3`）、模块存储（`modules.sqlite3`）、OutputStore、全局记忆与宿主私有凭据存储（`state_dir/credentials`）。

## 进程内运行时桥接与 IPC 契约

1. **in-process 链接**：`soyo-desktop` 直接调用 `soyo_app::spawn_build_runtime`、`soyo_app::session`、`soyo_app::events::{EventWriter, EventStore, EventModel}`、`soyo_app::thinking::ThinkingModel`、`soyo_app::context::ContextModel` 与 `soyo_app::interaction::{input_channel, RunCancellation}`，不打包 `soyo` 二进制做 sidecar，不引入额外序列化进程边界。
2. **请求／响应（`#[tauri::command]`）**：所有前端触发的控制、查询、设置、会话恢复、模块开关、记忆管理与导出调用均通过 Tauri command 提供，由 `specta`／`tauri-specta` 生成强类型 `desktop/src/bindings.ts`。
3. **Run 事件流（`tauri::ipc::Channel`）**：每次运行通过命令参数传入专用 `tauri::ipc::Channel`，按有序信封向前端推送 schema v1 事件（`Event`）、实时思考快照（`ThinkingLog`）、上下文分项估算（`ContextSnapshot`）与人工确认／澄清请求，不使用全局 `app.emit` 广播事件。消费者断开或前端取消时立即触发 `RunCancellation` 并等待后台清理屏障。
4. **人工交互闭环**：后端实现 `ApprovalHandler` 与 `InputHandler`（基于 `soyo_app::interaction::input_channel`），通过 `Channel` 将请求推送到前端，并在 `oneshot` 通道上等待 `reply_approval` / `reply_input` 响应；运行中切换工作模式立即使旧审批以 `stale_approval` 失效，最终权限仍由 `soyo-core` 派发前重查决定。
5. **非阻塞隔离**：打开 SQLite、扫描存储、哈希及文件系统发布等阻塞操作统一通过 `soyo_app::offload_boundary` / `offload` 调度，防止阻塞事件循环与窗口响应。

## 安全与显示边界

- **严格 CSP 与禁远程 URL**：`tauri.conf.json` 配置严格 `app.security.csp`，禁止加载外部脚本或连接远程页面，窗口只加载本地打包资源，不开放 Tauri `fs` 或 `assetProtocol` scope。
- **凭据脱敏与禁止前端落盘**：凭据仅通过 `soyo-core` 的 `FileCredentialStore` 与 `SecretRegistry` 管理；禁止接入 Tauri keyring 插件，禁止将任何凭据、令牌或私钥写入 `localStorage` / `sessionStorage` / IndexedDB。前端凭据管理页面仅获取并展示不含明文密钥的脱敏元数据（`CredentialMetadata`）。所有跨 IPC 的事件、思考快照、工具输出和错误提示均先经 `SecretRegistry` 脱敏。
- **自定义附件预览协议（`soyo-attachment://`）**：通过 `tauri::Builder::register_uri_scheme_protocol` 注册 `soyo-attachment`（并保留兼容别名 `saya-attachment`），仅允许读取当前活动会话已登记的草稿附件白名单或已验证归属当前会话的 `OutputStore` 媒体／文件对象，校验规范路径、禁止符号链接与路径穿越，并附加 `X-Content-Type-Options: nosniff` 与 `Cache-Control: private, no-store`。
- **流式 Markdown 渲染**：前端采用 Streamdown + Shiki + mermaid.js，按 [Markdown 说明](markdown-rendering.md) 的语义支持 `render`/`source` 切换、控制字符净化、HTML 源码安全呈现、代码块与 Mermaid 预算回退（`source fallback`）及脚注双向跳转；四工作模式颜色（WORK `#58A6FF`、CODE `#7DCB9B`、PLAN `#BB9AF7`、BUILD `#F6AD55`）与 `docs/tui-design.md` 一致。

## 打包、签名与自动更新

- **安装包矩阵**：`bundle.targets` 配置 `dmg`、`app`（macOS aarch64/x86_64）、`nsis`（Windows，明确不发 MSI）与 `appimage`（Linux），同时开启 `bundle.createUpdaterArtifacts: true`。
- **自动更新**：集成 `tauri-plugin-updater` 与 Ed25519 签名校验，使用 GitHub Releases 发布的 `latest.json` 作为更新端点，更新安装须经用户确认。
- **CI Secrets 隔离**：Ed25519 签名私钥及 Apple 签名／公证（notarization）凭据全部通过 GitHub Actions Secrets 注入 `.github/workflows/desktop-release.yml`，仓库代码中不得出现任何密钥或密码。
