# Soyo Desktop 设计规范（Soyo Dark / Soyo Light）

本文档定义 `soyo-desktop` 的视觉语言、双主题 Token、响应式布局、核心组件、动效规范与设计资产，对应实现位于 [`desktop/src/`](../../desktop/src/) 与 [`desktop/src-tauri/`](../../desktop/src-tauri/)。

> **状态**：D-075 与 D-076（2026-11-19）已全面落地；§1–§7 已按现行实现更新，§8 保留为本轮设计决策基线与参照边界记录。实机渲染截图归档于 [screenshots/desktop/](screenshots/desktop/)。

---

## 1. 设计语言与工程约定

- **双主题名称**：`Soyo Dark`（`soyo-dark`）与 `Soyo Light`（`soyo-light`），均由 [desktop/src/theme/tokens.css](../../desktop/src/theme/tokens.css) 的 `:root, :root[data-theme="dark"]` 与 `:root[data-theme="light"]` 及 Tailwind v4 `@theme` 单一声明，并按用户偏好（`System` / `Light` / `Dark`）切换 `<html data-theme>`、`color-scheme` 与 Tauri 窗口主题。
- **类名前缀**：自定义控件、排版与动效类名统一使用 `.soyo-*`（例如 `.soyo-pill`、`.soyo-input`、`.soyo-diff`、`.soyo-switch`、`.soyo-squares-loader`）；禁止在 TSX 组件中写入任意十六进制色值或未定义 CSS 变量。
- **渲染与存储约束**：
  - 全部图标统一使用 `lucide-react`（默认 `14px`／`16px`、`strokeWidth: 1.75`）或纯 CSS 几何图形；`desktop/src/**/*.{ts,tsx}` 中严格禁止内联 `<svg>`（由 [scripts/release/test_desktop_packaging.py](../../scripts/release/test_desktop_packaging.py) 强制检查）。
  - 所有正在执行／思考／加载状态指示器统一使用纯 HTML/CSS `div`/`span` 的 [SquaresLoader](../../desktop/src/components/SquaresLoader.tsx)（2×2 滑块方块动效），禁用旧脉动圆点与旋转 Unicode/SVG loader。
  - 禁止使用 `sessionStorage` 或 `indexedDB`；`localStorage` 仅限缓存无敏感性的 `"soyo.theme"`（`"dark"` 或 `"light"`），供 [desktop/public/theme-init.js](../../desktop/public/theme-init.js) 在首帧渲染前同步设置 `data-theme` 与 `color-scheme` 以消除启动闪烁，桌面偏好本体持久于 `~/.soyo/desktop_prefs.json`。
  - 默认控件与正文使用系统无衬线字体栈；Home 问候语与可选衬线对话字体使用随包打包、SIL OFL 1.1 授权的 Latin 可变字体 [SourceSerif4-Variable.woff2](../../desktop/src/assets/fonts/SourceSerif4-Variable.woff2)（`Source Serif 4`，中文回退 `"Songti SC", "Songti TC", "Noto Serif CJK SC", "Noto Serif CJK TC", serif`）；代码与 Diff 使用 `ui-monospace` 栈。

---

## 2. 色彩、语义 Token 与排版层级

所有 Token 定义于 [desktop/src/theme/tokens.css](../../desktop/src/theme/tokens.css)、[desktop/src/lib/sanitize.ts](../../desktop/src/lib/sanitize.ts) 与 [desktop/src/index.css](../../desktop/src/index.css)。文字 Token 在本主题全部表面上的 WCAG 最小对比度均 ≥ 4.5:1，交互控件边界 ≥ 3:1；Light 主题各 Token 与参照品牌色值的 RGB 分量差之和均 ≥ 12。

### 2.1 表面、边框、文字与语义色 Token

| Token | Soyo Dark | Soyo Light | 用途 |
| :--- | :--- | :--- | :--- |
| `--surface-sidebar` | `#1b1a18` | `#efece5` | 侧栏、折叠导轨、统一顶栏 |
| `--surface-app` | `#201f1d` | `#f7f5ef` | 主画布与内容底色 |
| `--surface-raised` | `#282725` | `#ffffff` | 卡片、输入框、菜单、胶囊 |
| `--surface-overlay` | `#2d2c29` | `#ffffff`（配合 `--shadow-dialog`） | 模态弹窗、悬浮菜单 |
| `--surface-hover` | `#34322f` | `#e3dfd5` | 悬停、选中行 |
| `--surface-code` / `--surface-code-inline` | `#171614` / `#2e2d2a` | `#efece5` / `#e6e2d8` | 代码块、Diff 与行内代码底板 |
| `--border-subtle` / `--border` / `--border-strong` | `#2f2d2a` / `#3a3835` / `#76716a` | `#dfdbd1` / `#d3cec3` / `#8c877d` | 分隔线、常规描边与控件强边界 |
| `--text-primary` / `--text-secondary` / `--text-tertiary` | `#ecebe6` / `#bdb9b0` / `#a09b92` | `#1f1e1b` / `#4f4c46` / `#66625b` | 正文、标签、时间与占位符（`prefers-contrast: more` 时 `--text-tertiary` 自动升为 `--text-secondary`） |
| `--accent` / `--accent-hover` / `--accent-pressed` | `#d9a08e` / `#e4b3a3` / `#cf9480` | `#a9583f` / `#9a4e36` / `#8c452f` | 主按钮填充、选中竖条、焦点环与开关开启态 |
| `--accent-text` / `--on-accent` / `--accent-surface` | `#d9a08e` / `#1f140f` / `rgba(217,160,142,.14)` | `#964f36` / `#ffffff` / `rgba(169,88,63,.10)` | 强调文字、主按钮文字与柔化底面 |
| `--info` / `--success` / `--warning` / `--danger` | `#9db6e0` / `#86cfa0` / `#e9b762` / `#f08a80` | `#2b5ea7` / `#2a6e45` / `#88580b` / `#b2352b` | 信息／新增／警告／危险语义；背景叠加 Dark 12%、Light 10% |
| `--ultra` / `--ultra-bright` / `--ultra-surface` | `#8a7bd8` / `#a096eb` / `rgba(160,150,235,.16)` | `#6d45c2` / `#5b37a8` / `rgba(109,69,194,.10)` | `EffortPopover` Ultracode 档位与能量网格 |
| `--loader-primary` / `--loader-secondary` | `#fafafa` / `#83838a` | `#1f1e1b` / `#83838a` | `SquaresLoader` 前景主块与轨迹底块 |

### 2.2 工作模式与权限配色（`WORK_MODE_STYLES` / `PERMISSION_MODE_LABELS`）

- 四种工作模式色值与 `crates/soyo-tui/src/theme.rs` 的 Dark 列完全一致，且仅用于模式圆点与文字（不与强调色共用）：
  - `WORK`：Dark `#6aaefc` ／ Light `#235fbb`
  - `CODE`：Dark `#86cfa0` ／ Light `#2a6e45`
  - `PLAN`：Dark `#bea3f5` ／ Light `#6d45c2`
  - `BUILD`：Dark `#efb25e` ／ Light `#88580b`
- 三种权限模式视觉信号：
  - `Auto`：中性 `--text-secondary`，不额外着色。
  - `Manual approval`：`--info` 配 `lucide` `Hand` 图标。
  - `Bypass permissions`：`--danger` 配 `ShieldAlert` 图标、完整文字与 `--danger-surface` 背景，并在顶栏（`data-testid="bypass-mode-badge"`）及 Composer 胶囊中常驻显示。

### 2.3 字号、圆角、阴影与内容适配

- **字号体系（6 级，最小 `11px`，字重仅用 `400`/`500`/`600`）**：caption `11/16px`、small `12/18px`、ui `13/20px`、body `14/22px`（Appearance 支持 `13`/`14`/`16px` 切换）、title `16/24px`、display `28/36px`（衬线 Home 问候语）。
- **圆角体系（5 级）**：`--radius-sm: 4px`、`--radius-md: 8px`、`--radius-lg: 12px`、`--radius-xl: 16px`、`--radius-full: 9999px`（应用图标外框保留 macOS squircle `rounded-[22%]`）。
- **阴影体系（2 级）**：`--shadow-popover` 与 `--shadow-dialog`。
- **随主题联动适配**：[MarkdownRenderer](../../desktop/src/components/MarkdownRenderer.tsx) 随 `resolvedTheme` 同步切换 Shiki（`github-dark` / `github-light`）与 Mermaid（`dark` / `default`）；[EffortPopover](../../desktop/src/components/EffortPopover.tsx) 通过 `GridField.setTheme()` 切换 WebGL2 能量网格配色（减弱动效下改为单帧静态绘制）；Diff 增删色与选中文本背景（30% 透明度 `--accent`）自动适配双主题。

---

## 3. 窗口外壳与响应式布局

### 3.1 Tauri 窗口配置与统一顶栏（`38px`）

[desktop/src-tauri/tauri.conf.json](../../desktop/src-tauri/tauri.conf.json) 配置 `titleBarStyle: "Overlay"`、`hiddenTitle: true`、`trafficLightPosition: { "x": 15, "y": 21 }`、默认尺寸 `1280 × 840`、最小尺寸 `900 × 620`、macOS 最低系统版本 `15.0`。

顶部 `38px`（`h-[38px]`）统一标题栏 `<header data-tauri-drag-region>` 分为左右两区：

1. **左区**：macOS Overlay 避让红绿灯（`pl-[88px]`）；侧栏展开时固定 `w-[248px] justify-between border-r border-[var(--border-subtle)]`，与下方 `248px` 侧栏竖缝对齐。含侧栏折展按钮（`PanelLeft`）与两段视图切换器（`Rendered` / `Source` Markdown 视图）。
2. **右区**：当前会话标题／`Home · {project}`（点击打开 `SessionBrowserDialog`）、中间可拖拽留白、右侧状态与控制簇——浏览预览调试栏（可通过 `?chrome=clean` 隐藏）、Goal 状态胶囊、常驻 `Bypass permissions` 危险徽标、工作模式下拉、`Auto` / `Manual approval` 下拉、`PanelRight`（切换 Inspector）、`Globe`（切换 Split Browser Preview）、`Gear`（打开 Settings）与溢出菜单。

### 3.2 单一可折叠侧栏与三档窗口断点

顶栏下方仅保留**单一左侧导航栏**（`data-testid="sidebar-panel"`，取消旧双栏结构），按窗口宽度自动响应式适配：

| 窗口断点 | 侧栏（Sidebar） | 右侧 Inspector（ContextSidebar） | 说明 |
| :--- | :--- | :--- | :--- |
| `>= 1280px` | 展开 `248px`（含 Home 空态，可用按钮或 `⌘B` 折叠为 `48px` 图标导轨） | 右侧常驻分栏（首启默认展开，记忆于 `desktop_prefs.inspector_open`） | 默认桌面窗口布局 |
| `1024px – 1279px` | 展开 `248px` | 覆盖式右抽屉（默认关闭，仅在非 Home 显式开关展开） | 保证主内容宽度充足 |
| `< 1024px`（最小 `900×620`） | 自动折叠为 `48px` 图标导轨（仍可通过按钮/`⌘B` 临时展开） | 覆盖式右抽屉（默认关闭） | 900px 窄屏不挤压对话与 Composer |

- **248px 侧栏内容**：顶部带文字导航（`New session ⌘N`、`Search ⌘K`、`Sessions`、`Memory`、`MCP & credentials`、`Export`、`Customize`），中部 `Recents` 按项目分组（每组最多 5 条、总计最多 30 条，超出折叠为 `+ N more` / `View all`），底部为用户行（头像、用户名 · 当前 profile），点击向上弹出 User Menu（主题 `System` / `Light` / `Dark` 快捷切换、`Language`、`Settings ⌘,`、`Usage`、`About`）。
- **全局快捷键**：`⌘N` 新建会话、`⌘K` 打开覆盖式搜索弹窗（按标题、工作区或当前会话正文过滤）、`⌘B` 开合侧栏、`⌘,` 打开 Settings。

---

## 4. 核心组件与设置信息架构

### 4.1 基础界面原语（`desktop/src/components/ui/`）

- **[Button.tsx](../../desktop/src/components/ui/Button.tsx)**：统一 `primary`（`--accent` 填充 + `--on-accent` 文字 + hover/pressed 状态）、`secondary`（`--surface-raised` + `--border-strong`）、`ghost`（透明 + hover 底）、`danger`（`--danger` 描边与文字）四种变体；[ApprovalDialog](../../desktop/src/components/ApprovalDialog.tsx) 内 `Allow (Y)` 使用 `primary`，`Deny (N)` 使用 `secondary`。
- **[Pill.tsx](../../desktop/src/components/ui/Pill.tsx)**：8px 圆角（`--radius-md`）、`whitespace-nowrap`，支持 `neutral`/`accent`/`info`/`success`/`warning`/`danger` 与 `interactive` 属性。
- **[DialogShell.tsx](../../desktop/src/components/ui/DialogShell.tsx)**：16px 圆角（`--radius-xl`）、`--surface-overlay` 背景、`--border` 描边、`--shadow-dialog`，统一三档宽度 `sm`（`480px`）、`md`（`640px`）、`lg`（`760px`），支持 `Escape` 与遮罩关闭。

### 4.2 Composer 与消息排版

- **单卡片 Composer（[Composer.tsx](../../desktop/src/components/Composer.tsx)）**：16px 圆角卡片（`--surface-raised` + `--shadow-popover`），底栏严格单行 `whitespace-nowrap shrink-0`。左侧 `+` 弹出菜单统一收纳 `Add files or photos`、`Paste path or media`、`Insert @ mention`、`Insert / command`、`Compact context` 与 `Web search` 开关（开启后在底栏显示可关闭标签）；右侧为合并式 `Mode · Permission` 胶囊、合并式 `Model · Effort` 胶囊、ContextRing 与发送／停止圆形按钮。运行或暂存时左侧显示 `12px` `SquaresLoader`。当 `[data-main-pane]` 宽度 `< 640px` 时，`Model · Effort` 胶囊自动截断模型名前 16 字并隐藏行内 Effort 文字（完整名称仍在 `title` 与 Model 弹出层内展示）。
- **消息排版与 Diff**：用户消息采用右对齐气泡（`--surface-raised`、`12px` 圆角），助手回复直接平铺于 `--surface-app` 主画布上，悬停展示时间戳与复制／重试／编辑操作；Diff 预览使用 `.soyo-diff`（12px 圆角、`--surface-code`、`--success-surface` / `--danger-surface` 增删行）。

### 4.3 三组 9 项 Settings（[SettingsDialog.tsx](../../desktop/src/components/SettingsDialog.tsx)）与辅助弹窗

- **导航结构（三组 9 项，已移除独立 `System` 页）**：
  1. `General` · `Appearance` · `Model & profiles` · `Permissions`
  2. `Modules` · `MCP & credentials` · `Memory`
  3. `Usage` · `Import & export`
- **`General › About`**：内嵌版本信息（`0.3.0`）、实时 `soyo helper` 连接状态（`get_helper_status`：PID、活跃 Run、连接客户端数、手动刷新）与 `Check for updates`。
- **`Appearance`**：集中提供主题（`System` / `Light` / `Dark`）、语言切换（与 `LanguageDialog` 共享状态，`en-US` / `zh-CN` / `zh-TW`）、正文字号（`13` / `14` / `16px`）、对话字体（`Default sans` / `Serif (Source Serif 4)` / `System UI`）及 `Reduce motion` 开关。
- **`Usage`**：由客户端传入本地 `-new Date().getTimezoneOffset()` 作为 `utc_offset_minutes`，按客户端本地日期分桶绘制按用途拆分的卡片与 30 天堆叠柱状图（Tooltip 使用内部亮度层避免遮挡邻居柱子）。
- **全局 `i18n` 与 sentence case**：全部用户可见文本、`aria-label` 与 `title` 均通过 [i18n.ts](../../desktop/src/lib/i18n.ts) 本地化并遵循 sentence case（工作模式简称 `WORK`/`CODE`/`PLAN`/`BUILD` 与专有缩写除外）；[SessionBrowserDialog](../../desktop/src/components/SessionBrowserDialog.tsx) 支持按工作区项目与状态（`All` / `In progress` / `Completed` / `Failed`）过滤。

---

## 5. 动效规范与可访问性

### 5.1 `.soyo-squares-loader` 与窗口动效原语（`index.css`）

- **`SquaresLoader`（`.soyo-squares-loader`）**：3 颗 `39.1304% × 39.1304%`、圆角 `27.778%` 的绝对定位方块（`--loader-secondary` + `--loader-primary`），沿 2×2 网格作 12 步 `4.8s cubic-bezier(0.65, 0, 0.35, 1)` 循环滑行（`soyo-sq-a`/`b`/`c`）；当 `reduceMotion` 为真或匹配 `@media (prefers-reduced-motion: reduce)` 时冻结为静止三格构图，不退化为旋转圆环或脉冲点。

| 类名 | 关键帧 | 时长与曲线 | 用途 |
| :--- | :--- | :--- | :--- |
| `.soyo-squares-loader__sq` | `soyo-sq-a` / `soyo-sq-b` / `soyo-sq-c` | `4.8s cubic-bezier(0.65, 0, 0.35, 1) infinite` | 运行中／思考中／加载态 2×2 方块指示 |
| `.soyo-backdrop-enter` | `soyo-fade-in` | `160ms cubic-bezier(0.16, 1, 0.3, 1)` | 对话框半透明遮罩入场 |
| `.soyo-dialog-enter` | `soyo-dialog-in` | `200ms cubic-bezier(0.16, 1, 0.3, 1)` | 模态弹窗轻位移（`6px`）与缩放（`0.975 -> 1`） |
| `.soyo-popover-up` / `.soyo-popover-down` | `soyo-popover-*` | `150ms cubic-bezier(0.16, 1, 0.3, 1)` | 上下弹出菜单 |
| `.soyo-panel-enter` / `.soyo-msg-enter` | `soyo-panel-in` / `soyo-msg-in` | `160ms` / `180ms` `cubic-bezier(0.16, 1, 0.3, 1)` | 主视图、Goal 胶囊与消息入场 |
| `.soyo-switch__knob` | — | `transform 180ms cubic-bezier(0.34, 1.3, 0.64, 1)` | 开关滑块过冲位移 |

### 5.2 无障碍与键盘约定

- **减弱动效降级**：在 `[data-reduce-motion="true"]` 与 `@media (prefers-reduced-motion: reduce)` 下全面关闭 `.soyo-*` 过渡、冻结 `SquaresLoader` 与 `EffortPopover` WebGL 网格帧。
- **对比度增强与焦点环**：`@media (prefers-contrast: more)` 自动提升弱文字与常规边框；全局交互元素 `:focus-visible` 统一为 `2px solid var(--accent)`（`outline-offset: 2px`）；滚动条宽 `6px`、滑块 `--border-strong`、轨道透明。
- **键盘导航**：非阻塞对话框支持 `Escape` 关闭，`⌘N` / `⌘K` / `⌘B` / `⌘,` 覆盖新建、搜索、侧栏折叠与设置。

---

## 6. 图标资产目录与构建规范

全部设计原稿与主导出成品归档于 [docs/design/assets/](assets/)：

| 资产文件 | 用途与参数 |
| :--- | :--- |
| [assets/icon.json](assets/icon.json) | Xcode Icon Composer 描述清单：背景 `automatic-gradient`（`display-p3:0.00935,0.02207,0.04118,1.00000`），图层引用 `soyo-harness-icon-transparent.png`，`neutral` 阴影 `opacity: 0.5`，半透明开启 `value: 0.3`，`squares: "shared"`。 |
| [assets/soyo-harness-icon-transparent.png](assets/soyo-harness-icon-transparent.png) | `1024 × 1024` 8-bit/color RGBA 透明底矢量导出前景原稿。 |
| [assets/soyo-icon-macos-1024.png](assets/soyo-icon-macos-1024.png) | 使用 Xcode `ictool` 从 Icon Composer `.icon` 工程导出的 `1024 × 1024` macOS 主视觉图。 |

### 6.1 运行时与打包目标规格

由上述母版衍生并受 [scripts/release/test_desktop_packaging.py](../../scripts/release/test_desktop_packaging.py) 校验的运行时图标资产（D-078 暂停 Windows NSIS 分发，`icon.ico` 作为打包清单资源保留于图标目录）：

| 目标路径 | 尺寸 / 格式 | 用途 |
| :--- | :--- | :--- |
| [desktop/public/app-icon.png](../../desktop/public/app-icon.png) | `256 × 256` 8-bit RGBA PNG | 前端 Home 问候标题与左下角用户头像（`rounded-[22%]`） |
| [desktop/src-tauri/icons/icon.png](../../desktop/src-tauri/icons/icon.png) | `512 × 512` 8-bit RGBA PNG | 通用桌面应用图标（Linux AppImage） |
| [desktop/src-tauri/icons/128x128@2x.png](../../desktop/src-tauri/icons/128x128@2x.png) | `256 × 256` 8-bit RGBA PNG | Tauri 高分屏 Bundle 图标 |
| [desktop/src-tauri/icons/128x128.png](../../desktop/src-tauri/icons/128x128.png) | `128 × 128` 8-bit RGBA PNG | Tauri 标准分辨率 Bundle 图标 |
| [desktop/src-tauri/icons/32x32.png](../../desktop/src-tauri/icons/32x32.png) | `32 × 32` 8-bit RGBA PNG | 小尺寸窗口图标 |
| [desktop/src-tauri/icons/icon.icns](../../desktop/src-tauri/icons/icon.icns) | macOS `.icns` 集合 | 遵循 macOS HIG `824 / 1024` 安全区边距 |
| [desktop/src-tauri/icons/icon.ico](../../desktop/src-tauri/icons/icon.ico) | Windows `.ico` 资源 | Tauri Bundle 清单保留图标（当前不产出 NSIS 目标） |

---

## 7. 设计稿同步与实机渲染截图

### 7.1 单源生成器与 `.pen` 设计文档

- **[devin_theme_gen.py](devin_theme_gen.py)**：直接解析 [desktop/src/theme/tokens.css](../../desktop/src/theme/tokens.css) 的 Dark / Light Token 并内嵌 `SourceSerif4-Variable.woff2`，支持 `--theme dark|light` 生成与现行 248px 侧栏、合并胶囊 Composer、三组 9 项 Settings 与 `SquaresLoader` 对齐的 14 帧 standalone HTML：
  ```bash
  python3 docs/design/devin_theme_gen.py --theme dark /tmp/soyo_theme_dark
  python3 docs/design/devin_theme_gen.py --theme light /tmp/soyo_theme_light
  ```
- **[desktop-ui.pen](desktop-ui.pen)**：经 Pencil MCP 维护的仓库内 `.pen` 设计源文件（`mode: ["dark", "light"]` 48 项设计 Token 变量、绑定率 100%，含 14 帧 Soyo Dark 与右移 `+4560px` 并排的 14 帧 Soyo Light 共 **28 个画板**；`.pen` 为加密格式，须通过 Pencil MCP 编辑并调用 `save` 落盘，不可用文本工具直改）。

### 7.2 桌面 UI 实机渲染截图（`docs/design/screenshots/desktop/`）

以下 10 张 PNG 均在无头 Chromium CDP 中以 Vite 预览服务（`?preview=1&chrome=clean`）按 `deviceScaleFactor=2` 实际渲染并完成控制台零错误、零横向溢出、Composer 工具栏零折行／零裁切检查：

| 场景与断点 | Soyo Dark | Soyo Light |
| :--- | :--- | :--- |
| Home 空态（`1280×840`） | [home-dark-1280x840.png](screenshots/desktop/home-dark-1280x840.png) | [home-light-1280x840.png](screenshots/desktop/home-light-1280x840.png) |
| 对话态 + Diff + 常驻 Inspector（`1280×840`） | [conversation-dark-1280x840.png](screenshots/desktop/conversation-dark-1280x840.png) | [conversation-light-1280x840.png](screenshots/desktop/conversation-light-1280x840.png) |
| 窄窗折叠侧栏（`900×620`） | [conversation-dark-900x620.png](screenshots/desktop/conversation-dark-900x620.png) | [conversation-light-900x620.png](screenshots/desktop/conversation-light-900x620.png) |
| 审批请求卡片（`1280×840`） | [approval-dark-1280x840.png](screenshots/desktop/approval-dark-1280x840.png) | — |
| Settings › Appearance（`1280×840`） | [settings-appearance-dark-1280x840.png](screenshots/desktop/settings-appearance-dark-1280x840.png) | [settings-appearance-light-1280x840.png](screenshots/desktop/settings-appearance-light-1280x840.png) |
| Settings › Usage（`1280×840`） | [settings-usage-dark-1280x840.png](screenshots/desktop/settings-usage-dark-1280x840.png) | — |

---

## 8. D-075／D-076 设计基线（已实施）

本节记录 D-075／D-076 确立的目标规范、对比度计算基线、Claude 参照边界与 D-076 定稿细则，已于本轮全部落地至代码与 §1–§7；后续界面迭代须继续遵守下列数值与约束。

### 8.1 参照与原则

- **参照对象**：参照 Claude Desktop 等对话式 AI 桌面应用的设计语言（项目负责人指定），详细描述见 §8.11。只借鉴设计语言，不复制 Logo、专有字体或品牌色值；Soyo 保留自己的强调色系、应用图标和 lucide 图标体系。
- **六条原则**：
  1. 暖调、低饱和的中性色；颜色只用于强调与状态，同一种颜色只表示一种含义。
  2. 导航放在单一的带文字侧栏中，可以收起；内容区居中，留白充足。
  3. 输入框保持极简，任何宽度下都不折行。
  4. 衬线字体只用于首页问候语。
  5. 文案用 sentence case，并全部经过 i18n；危险状态同时显示文字与图标，不只靠颜色。
  6. Soyo Dark 与 Soyo Light 两套主题必须同等完整：任何组件都要在两套主题下通过对比度与截图验收。
- **单一来源**：Token 只在 `desktop/src/theme/tokens.css` 中定义（Tailwind v4 `@theme`）。组件不得再使用任意十六进制色值；设计稿生成器与 Figma Variables 都从这份定义派生。

### 8.2 颜色 Token

“最小对比度”指该文字色在本主题全部 5 个表面上的最小值，按 WCAG 公式计算。文字 Token 一律 ≥ 4.5:1，控件边界 ≥ 3:1。Light 列已按 D-076 调整，与参照对象的品牌色值拉开距离（见 §8.11）。

| Token | Soyo Dark | Soyo Light | 用途 | 最小对比度（Dark／Light） |
| --- | --- | --- | --- | --- |
| `--surface-sidebar` | `#1b1a18` | `#efece5` | 侧栏、导轨 | — |
| `--surface-app` | `#201f1d` | `#f7f5ef` | 主画布 | — |
| `--surface-raised` | `#282725` | `#ffffff` | 卡片、输入框、菜单 | — |
| `--surface-overlay` | `#2d2c29` | `#ffffff`（配合 `--shadow-dialog`） | 对话框、弹出层 | — |
| `--surface-hover` | `#34322f` | `#e3dfd5` | 悬停、选中行 | — |
| `--border-subtle` | `#2f2d2a` | `#dfdbd1` | 分隔线（装饰） | — |
| `--border` | `#3a3835` | `#d3cec3` | 卡片描边（装饰） | — |
| `--border-strong` | `#76716a` | `#8c877d` | 输入框与控件边界（对相邻背景） | 3.08／3.28 |
| `--text-primary` | `#ecebe6` | `#1f1e1b` | 标题、正文 | 10.71／12.53 |
| `--text-secondary` | `#bdb9b0` | `#4f4c46` | 次要文字、标签 | 6.53／6.43 |
| `--text-tertiary` | `#a09b92` | `#66625b` | 时间、元数据、占位符（最弱一级） | 4.62／4.56 |
| `--accent` | `#d9a08e` | `#a9583f` | 主按钮填充、选中指示、焦点环、开关开启态 | — |
| `--accent-text` | `#d9a08e` | `#964f36` | 强调色文字 | 5.70／4.55 |
| `--on-accent` | `#1f140f` | `#ffffff` | 主按钮文字 | 8.04／5.05（相对 `--accent`） |
| `--info` | `#9db6e0` | `#2b5ea7` | 链接、信息提示、`Manual approval` | 6.21／4.84 |
| `--success` | `#86cfa0` | `#2a6e45` | Diff 新增、完成状态 | 6.97／4.62 |
| `--warning` | `#e9b762` | `#88580b` | 警告横幅、待切换标记 | 6.95／4.58 |
| `--danger` | `#f08a80` | `#b2352b` | 错误、Diff 删除、`Bypass permissions` | 5.27／4.59 |

语义背景（横幅、Diff 行）使用对应语义色叠加：Dark 为 12% 透明度，Light 为 10%。叠加后文字对比度仍须 ≥ 4.5:1。

### 8.3 模式与权限显示

| 模式 | Soyo Dark | Soyo Light | 最小对比度（Dark／Light） |
| --- | --- | --- | --- |
| WORK | `#6aaefc` | `#235fbb` | 5.52／4.61 |
| CODE | `#86cfa0` | `#2a6e45` | 6.97／4.62 |
| PLAN | `#bea3f5` | `#6d45c2` | 5.93／4.81 |
| BUILD | `#efb25e` | `#88580b` | 6.82／4.58 |

- **模式色的使用范围**：只用于模式徽标的圆点与文字，不用于大面积背景，也不与强调色共用。模式徽标始终带文字标签：CODE 与 `--success`、BUILD 与 `--warning` 色相接近，单靠颜色无法区分。TUI 的 `mode_color` 采用 Dark 一列。
- **权限显示**：
  - `Auto`：用 `--text-secondary`，不着色。
  - `Manual approval`：用 `--info`，配 `Hand` 图标。
  - `Bypass permissions`：用 `--danger`，配 `ShieldAlert` 图标与完整文字，在顶栏与输入框中常驻显示；开启前仍须先打开设置中的“Allow bypass permissions mode”。

### 8.4 字体、字号、圆角、间距与阴影

- **字体**：
  - 控件与正文使用系统字体栈，不打包字体。
  - 展示字体 `--font-display` 打包一款 SIL OFL 授权的衬线字体（建议 Source Serif 4，子集化为 woff2），用于首页问候语和“对话字体：衬线”选项。
  - 等宽字体使用 `ui-monospace` 字体栈。
  - 生成器与实现共用同一份 `font-family` 定义。
- **字号**：6 级，最小 11px；字重只用 400、500、600。

  | 级别 | 字号／行高 | 用途 |
  | --- | --- | --- |
  | caption | 11／16px | 时间、计数、键盘提示 |
  | small | 12／18px | 次要控件、元数据 |
  | ui | 13／20px | 默认控件文字 |
  | body | 14／22px | 对话正文；Appearance 可选 13、14、16px |
  | title | 16／24px | 对话框与分区标题 |
  | display | 28／36px（衬线） | 首页问候语 |

- **圆角**：4px（复选框、小标签）、8px（按钮、输入框）、12px（卡片、菜单）、16px（对话框、输入框卡片）；徽标用全圆角。
- **间距**：按 4px 网格取 4、8、12、16、24、32。
- **阴影**：只保留两级。

  | Token | Soyo Dark | Soyo Light |
  | --- | --- | --- |
  | `--shadow-popover` | `0 8px 24px rgba(0, 0, 0, 0.45)` | `0 8px 24px rgba(31, 30, 27, 0.12)` |
  | `--shadow-dialog` | `0 24px 64px rgba(0, 0, 0, 0.55)` | `0 24px 64px rgba(31, 30, 27, 0.18)` |

### 8.5 主题切换与适配

- **偏好设置**：Appearance 提供“主题：System（默认，跟随系统外观）／Light／Dark”，偏好随桌面偏好保存（`desktop_prefs`）。为避免启动时闪烁，可在 `localStorage` 缓存上次生效的主题；主题不是敏感数据，不违反 D-072 第 (4) 条。
- **切换机制**：
  - 用 `<html data-theme="dark|light">` 切换。System 模式按 `prefers-color-scheme` 决定，并监听系统切换。
  - 同步 CSS `color-scheme`，让滚动条和原生表单控件跟随主题。
  - 同步 Tauri 窗口主题，让标题栏与红绿灯按钮跟随主题。
- **增强对比度**：`prefers-contrast: more` 时，`--text-tertiary` 提升为 `--text-secondary` 的取值，`--border` 提升为 `--border-strong` 的取值。两套主题都适用。
- **减弱动效**：`prefers-reduced-motion` 覆盖全部动效，包括 Tailwind `transition-*`、`animate-spin`，以及 EffortPopover 的 WebGL 动画（改为静态帧）。
- **内容适配**：以下内容随主题切换：
  - Shiki 代码高亮（深浅各一套主题）；
  - Mermaid（`dark`／`default` 主题）；
  - Diff 新增与删除底色；
  - EffortPopover 能量网格配色；
  - 附件与媒体预览底板；
  - 选中文本颜色与焦点环。

  应用图标和头像不随主题变化。

### 8.6 布局与断点

- **侧栏**：
  - 宽 248px，顶部是带文字的导航：New session、Search、Sessions、Memory、MCP & credentials、Export、Customize。
  - 中部是 Recents（按项目分组）与 View all；底部是用户行（头像、名称 · 当前 profile）。
  - 可收起为 48px 图标导轨；Home 空态不隐藏侧栏。
- **断点**：
  - 窗口 ≥ 1280px：侧栏展开，Inspector 可常驻。
  - 1024–1279px：Inspector 改为覆盖式抽屉，默认关闭。
  - < 1024px（最小 900px）：侧栏自动收起为导轨。
- **Composer**：
  - 16px 圆角的单张卡片。
  - 左侧是 `+` 菜单，包含附件、粘贴、@ 提及、斜杠命令、Compact 和 Web search 开关；已启用的工具显示为可移除的标签。
  - 右侧依次是“模式 · 权限”胶囊、“模型 · 思考强度”胶囊、发送／停止按钮。
  - 所有胶囊 `whitespace-nowrap`；主区宽度 < 640px 时，模型胶囊只显示简称。

### 8.7 组件与文案

- **`Button`**：所有变体统一悬停、按下、禁用和焦点状态。
  - primary：`--accent` 填充，`--on-accent` 文字。
  - secondary：raised 表面 + `--border-strong` 描边。
  - ghost：透明，悬停时用 hover 表面。
  - danger：`--danger` 文字与描边，只用于不可撤销的操作。

  审批卡片中，Allow 用 primary，Deny 用 secondary。
- **`DialogShell`**：16px 圆角、overlay 表面、1px `--border`、`--shadow-dialog`；宽度只有 sm 480、md 640、lg 760 三档。
- **`Pill`**：8px 圆角，不折行，静态与可交互两种样式。
- **图标**：只用 lucide，尺寸 14px 或 16px，`strokeWidth` 1.75；禁止用 Unicode 字符当图标。
- **文案**：
  - 统一 sentence case；WORK、CODE、PLAN、BUILD 作为产品专名保留大写。
  - 权限标签固定为 `Auto`、`Manual approval`、`Bypass permissions`。
  - `aria-label` 与 `title` 全部经过 i18n。

### 8.8 设置信息架构

- **结构**：三组，共 9 项：General、Appearance、Model & profiles、Permissions ｜ Modules、MCP & credentials、Memory ｜ Usage、Import & export。
- **Appearance**：包含主题、界面语言（并入现在的 LanguageDialog）、正文字号、对话字体（无衬线／衬线）和减弱动效。
- **About**：位于 General 底部，显示版本号（与 workspace 一致）、helper 状态和检查更新。不设 System 页。

### 8.9 设计稿与工具链

- **生成器**：[devin_theme_gen.py](devin_theme_gen.py) 改为读取 `tokens.css`，按 `--theme dark|light` 输出 CSS 变量。
- **Figma Variables**：导入 Figma 后，用 `use_figma` 脚本按值把节点绑定到 Variables 集合 “Soyo Theme”。该集合有 Dark、Light 两种 mode，变量名与 CSS 变量一致。
- **设计稿 frame**：Theme 页重新生成全部 14 个 frame；至少为 01 Home、02 Conversation、04 Settings 增加 Light 版本。
- **TUI**：`crates/soyo-tui/src/theme.rs` 的 `mode_color` 与 [tui.md](../tui.md) 改用 §8.3 的 Dark 一列。

### 8.10 实施阶段与验收

| 阶段 | 内容 | 验收 |
| --- | --- | --- |
| 1 | 权限配色；Composer 不折行与 `+` 菜单；宽度断点；文字 Token；最小字号 11px | 1280×840 与 900×620 截图无折行、无裁切；对比度脚本通过 |
| 2 | 引入 `tokens.css`（同时定义 Dark、Light）、`Button`、`Pill`、`DialogShell`；按组件分批替换任意值 | TSX 中 `[#…]` 任意色值为 0；字号不超过 6 级，圆角不超过 5 种 |
| 3 | 合并侧栏、设置信息架构、图标、字体、文案与 i18n；主题切换与 §8.5 全部内容适配，完成 Light 主题验收 | 两套主题 × 两种窗口尺寸截图通过；两套主题对比度脚本通过；没有硬编码 `aria-label`／`title` |
| 4 | Figma Variables（Dark／Light）与生成器；TUI 模式色；改写 §1–§7 与决策影响栏 | Figma 可绑定节点 ≥ 95% 绑定变量；规范、生成器、实现三处数值一致 |

### 8.11 参照主题详细描述（Claude Desktop）

- **来源**：本节依据 2026-11 可查到的公开资料整理：
  - Claude 帮助中心的外观设置说明（[Customizing your appearance settings](https://support.claude.com/en/articles/8887527-customizing-your-appearance-settings)）；
  - 第三方整理的 Claude Desktop（macOS）界面地图（[weloveclaude.com](https://www.weloveclaude.com/reference/desktop-sitemap)）；
  - Anthropic 关于 Claude Code 桌面版改版的文章（[Redesigning Claude Code on desktop](https://claude.com/resources/articles/claude-code-desktop-redesign)）；
  - Anthropic 品牌规范中的色值与字体。
- **适用范围**：参照对象会持续改版，本节只记录用于决策的特征，不作为参照对象的权威说明。按 D-075 的边界，Soyo 只借鉴设计语言，表中“Soyo 对应”一列才是实现依据。

| 维度 | 参照对象的做法 | Soyo 对应 |
| --- | --- | --- |
| 整体气质 | 暖调中性色（米白、炭灰）配单一暖橙强调色；界面安静，内容优先，装饰极少 | 暖调中性色配自有桃陶色强调（Dark `#d9a08e`、Light `#a9583f`），见 §8.2 |
| 品牌色值 | 深 `#141413`、浅 `#faf9f5`、中灰 `#b0aea5`、浅灰 `#e8e6dc`、强调橙 `#d97757`、辅助蓝 `#6a9bcc`、辅助绿 `#788c5d` | 不使用；§8.2–§8.3 中每个 Token 与上述每个色值的 RGB 分量差之和都 ≥ 12（已校验） |
| 品牌字体 | 品牌规范中，标题用 Poppins，正文用 Lora | 不沿用这组字体：控件与正文用系统字体，展示字体用 Source Serif 4（§8.12） |
| 外观模式 | Light、Match System、Dark 三档，位于 Settings › Appearance；左下角账户菜单也可切换 | System（默认）、Light、Dark 三档，位于 Settings › Appearance；底部用户菜单提供同一开关 |
| 对话字体 | Default、Match System、Dyslexic Friendly | 默认（系统无衬线）、衬线、系统三档；不提供易读障碍字体（§8.12） |
| 侧栏 | 左侧单栏：顶部为 New、Search 等导航，中部为置顶与最近会话，底部为头像与名称组成的账户区；可用按钮开合，但不能完全关闭 | 248px 带文字侧栏，可收起为 48px 导轨（同样不完全隐藏）；底部用户行打开用户菜单（§8.6、§8.12） |
| 快捷键 | 新建 ⌘N、搜索 ⌘K、开合侧栏 ⌘B、设置 ⌘, | 采用同一组快捷键（也符合 macOS 惯例） |
| 输入框 | 大圆角输入卡片；左侧为添加文件、连接器等入口，模型与思考强度合并为一个控件，右侧为发送 | §8.6 的 Composer：`+` 菜单、“模式 · 权限”胶囊、“模型 · 思考强度”胶囊、发送 |
| 消息排版 | 用户消息为右对齐的圆角气泡；回复下方有复制、重试、编辑与反馈的操作行 | 用户消息为右对齐气泡；助手回复不加气泡，按正文排版；操作行在悬停时显示（§8.12） |
| 设置 | 左侧为分组导航；General 含外观与文字大小；桌面版另有本机相关页（System、Extensions、Developer） | 三组共 9 项；Appearance 独立成页；不设 System 页，helper 状态放在 General › About（§8.8） |
| 多会话（Claude Code 桌面版） | 侧栏集中显示活动会话与最近会话，可按状态、项目、环境过滤，也可按项目分组；PR 合并或关闭后会话自动归档 | Recents 按项目分组；会话浏览器可按项目和状态过滤；不做自动归档（§8.12） |
| 图标与动效 | 细线条单色图标；动效克制、短暂 | lucide、`strokeWidth` 1.75；沿用 §5.1 的时长与曲线 |

### 8.12 细则定稿（D-076）

以下各项原为待定，现由 D-076 确定。

| 事项 | 定稿 |
| --- | --- |
| 展示字体 | Source Serif 4（SIL OFL 1.1），可变字重 woff2，只打包 Latin 子集；中文回退为 `"Songti SC", "Songti TC", "Noto Serif CJK SC", "Noto Serif CJK TC", serif` |
| 对话字体 | 默认为系统无衬线；可选“衬线”（Source Serif 4）或“系统”（跟随系统字体）；不提供易读障碍字体 |
| 主题偏好与缓存 | 偏好存为 `desktop_prefs.theme`（`system`／`light`／`dark`）；`localStorage` 键 `soyo.theme` 只缓存最终生效的 `dark`／`light`，用于启动时首帧 |
| Inspector 默认状态 | 首次启动且窗口 ≥ 1280px 时展开，此后记住上次状态（`desktop_prefs.inspector_open`）；窗口 < 1280px 时一律为抽屉，默认关闭 |
| 侧栏导航行为 | 见本表下方“侧栏导航行为”一段 |
| Recents 数量 | 每个项目分组最多 5 条，总计最多 30 条；其余经 View all 进入会话浏览器 |
| 会话浏览器 | 可按项目、状态（进行中、已完成、失败）过滤；不做自动归档 |
| 模型胶囊 | 显示“模型名 · 思考强度”；主区 < 640px 时只显示模型名前 16 个字符（超出加省略号），隐藏强度文字；完整内容在 `title` 与弹层中显示 |
| 强调色状态 | Dark：hover `#e4b3a3`、pressed `#cf9480`（`--on-accent` 上为 9.68／7.04:1）<br>Light：hover `#9a4e36`、pressed `#8c452f`（白字上为 5.97／6.98:1）<br>禁用态统一 40% 不透明度（WCAG 对禁用控件不作对比度要求） |
| 焦点环、选中与滚动条 | 焦点环为 2px `--accent`，偏移 2px，对相邻背景 ≥ 3:1；选中文本用 `--accent` 30% 透明度；滚动条宽 6px，滑块为 `--border-strong`，轨道透明 |
| 用户菜单 | 点击底部用户行打开，包含：主题（System／Light／Dark）、语言、设置、关于 |
| 消息排版 | 用户消息为右对齐气泡（`--surface-raised`、12px 圆角）；助手回复不加气泡，用 `body` 字号；操作行（复制、重试、编辑）悬停显示 |
| 设计稿范围 | 14 个 frame 全部生成 Dark、Light 两版，共 28 个；Light 版整体向右平移 4560px（三列 × 1520），与 Dark 版并排 |

**侧栏导航行为**：

- New session（⌘N）：新建会话。
- Search（⌘K）：打开覆盖式搜索，范围为会话标题与内容。
- Sessions：打开会话浏览器。
- Memory、MCP & credentials：分别打开设置中的对应页。
- Export：打开当前会话的导出对话框，没有会话时禁用。
- Customize：打开设置中的 Appearance 页。
- ⌘B 开合侧栏，⌘, 打开设置。
