# Soyo Desktop 设计规范（Soyo Dark）

本文档定义 `soyo-desktop` 的视觉语言、窗口布局、组件约定、动效规范与图标资产清单，对应实现位于 [`desktop/src/`](../../desktop/src/) 与 [`desktop/src-tauri/`](../../desktop/src-tauri/)。

> **状态**：§1–§7 描述现行实现，目前只有 Soyo Dark。D-075（2026-11-19）已确认视觉修正方案与 Soyo Light 亮色主题，目标规范见 [§8](#8-d-075-目标规范已确认待实施)，尚未实施；各阶段落地后改写 §1–§7 的对应条目。

---

## 1. 设计语言与命名约定

- **主题名称**：`Soyo Dark`（`soyo-dark`），在 [desktop/src/index.css](../../desktop/src/index.css) 的 `:root` 中通过 `color-scheme: dark` 与语义 CSS 变量声明。
- **类名前缀**：自定义控件、排版与动效类名统一使用 `.soyo-*`（例如 `.soyo-pill`、`.soyo-input`、`.soyo-diff`、`.soyo-switch`）；禁止重新引入历史命名。
- **渲染约束**：
  - 全部图标使用 `lucide-react` 或纯 CSS 几何图形，禁止在 `desktop/src/**/*.ts(x)` 中手写内联 `<svg>`（由 [scripts/release/test_desktop_packaging.py](../../scripts/release/test_desktop_packaging.py) 强制检查）。
  - 禁止使用 `localStorage`、`sessionStorage` 或 `indexedDB`；桌面状态由 Zustand 与 Tauri 命令通道维护。
  - 默认应用字体栈为 `-apple-system, BlinkMacSystemFont, "SF Pro Text", "Segoe UI", Inter, sans-serif`，代码与单行 Diff 使用 `ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", monospace`。

---

## 2. 色彩与语义 Token

以下 Token 均直接定义于 [desktop/src/index.css](../../desktop/src/index.css) 与 [desktop/src/lib/sanitize.ts](../../desktop/src/lib/sanitize.ts)。

### 2.1 表面与边框层次

| 用途 | Token / CSS 变量 | 色值 | 说明 |
| :--- | :--- | :--- | :--- |
| 窗口底色 / `--background` | — | `#101010` | 根容器背景 |
| Primary Rail 背景 | `--bg-rail` | `#0d0d0d` | 最左侧 `48px` 主导轨与顶栏左侧区域 |
| Sidebar 背景 | `--bg-sidebar` | `#0f0f0f` | `248px` 次级栏与右侧 Inspector 背景 |
| 主画布背景 | `--bg-main` | `#141414` | 对话主区域、Home 空态画布及顶栏主区域 |
| 卡片 / 输入槽位 | `--bg-card` | `#1a1a1a` | 消息卡片、下拉触发与次级面板 |
| 悬浮弹窗 / 对话框 | `--bg-dialog` | `#181818` | Dialog 容器背景 |
| 胶囊 / 浮动菜单 | `--bg-pill` | `#1c1c1c` | `.soyo-pill`、`.soyo-pill-btn` 与弹出菜单 |
| Composer 卡片 | `--bg-composer` | `#1d1d1d` | 底部与 Home 居中输入框表面 |
| 细分隔线 | `--border-subtle` | `#1f1f1f` | 顶栏底边与各列分界线 |
| 卡片边框 | `--border-card` | `#262626` | 常规边框与通配 `* { border-color }` |
| 胶囊边框 | `--border-pill` | `#2e2e2e` | `.soyo-pill` / `.soyo-pill-btn` 默认描边 |

### 2.2 文本、强调与语义色

| 类别 | Token / CSS 变量 | 色值 | 用途 |
| :--- | :--- | :--- | :--- |
| 主文字 | `--text-primary` | `#e8e8e8` | 标题、正文、激活按钮文字 |
| 次文字 | `--text-secondary` | `#a3a3a3` | 顶栏描述、次级按钮、标签 |
| 弱文字 | `--text-muted` | `#6e6e6e` | 占位符、分隔符、辅注时间 |
| 主强调色 | `--accent` | `#d9a08e` | 激活指示竖条、`+ New session`、开关开启态、焦点描边 |
| 强调链接 | `--accent-link` | `#e2a68f` | 行内强调链接 |
| 信息蓝 / Markdown 链接 | `--md-link` / `--info-blue` | `#8aa8d8` / `#7aa2f7` | 文档链接、恢复提示、Diff hunk 标题 |
| 成功 / Code | `--semantic-green` | `#4ade80` | Diff 新增标记、`CODE` 模式、选中勾号 |
| 警告 / Manual / Build | `--semantic-amber` | `#fac454` | Manual Approval、`BUILD` 模式、`next` 待切换标记 |
| 错误 / 删除 | `--semantic-red` | `#f87171` | 错误横幅、Diff 删除标记 |
| Ultracode / Plan | `--ultracode` / `--ultracode-bright` | `#8a7bd8` / `#a096eb` | Ultracode 开关与 `PLAN` 模式标识 |

### 2.3 工作模式与权限配色（`WORK_MODE_STYLES` / `PERMISSION_MODE_LABELS`）

- `work` → 标签 `WORK`，颜色 `#d9a08e`
- `code` → 标签 `CODE`，颜色 `#4ade80`
- `plan` → 标签 `PLAN`，颜色 `#a096eb`
- `build` → 标签 `BUILD`，颜色 `#fac454`
- 权限模式展示标签：`auto` → `auto`（`#8aa8d8`）、`bypass` → `bypass permissions`（`#8aa8d8`）、`manual` → `Manual Approval`（`#fac454`）。

---

## 3. 窗口外壳与全局布局

### 3.1 Tauri 窗口配置与 Overlay 标题栏

[desktop/src-tauri/tauri.conf.json](../../desktop/src-tauri/tauri.conf.json) 为桌面主窗口配置：

- `titleBarStyle: "Overlay"`、`hiddenTitle: true`、`trafficLightPosition: { "x": 15, "y": 21 }`
- 默认尺寸 `1280 × 840`，最小约束 `900 × 620`
- macOS 最低系统版本 `15.0`

其中 `trafficLightPosition: { x: 15, y: 21 }` 在 macOS AppKit / `tao` 的坐标映射下使三颗 `14pt` 红绿灯按钮位于 `top = 12px`、`bottom = 26px`（距离顶部与底部分界线各 `12px`），从而在 `38px` 统一顶栏内精确垂直居中，水平占据 `x = 15..75px`。前端在 [desktop/src/App.tsx](../../desktop/src/App.tsx) 中通过 `TITLEBAR_OVERLAY` 判断当前是否运行于 macOS Tauri 打包环境。

### 3.2 统一顶栏（`38px`）架构

窗口顶部全宽部署一条固定高度 `38px`（`h-[38px]`）的统一标题栏 `<header data-tauri-drag-region>`（底色 `#0f0f0f`、底边框 `#1f1f1f`），消除红绿灯未居中及报错横幅挤占标题栏拖拽区的冲突：

1. **左区（对齐红绿灯安全区与侧栏竖缝）**：
   - macOS Overlay 模式下应用 `pl-[88px]` 避让红绿灯（与 `x = 75px` 绿灯右缘保持 `13px` 间距）；非 macOS/浏览器预览使用水平对称内边距。
   - 当 Column 2（Sidebar）展开时固定宽度 `w-[296px] justify-between border-r border-[#1f1f1f] bg-[#0f0f0f]`（`48px` Rail + `248px` Sidebar = `296px`），使顶栏竖线与下方侧栏分界线连续对齐；当 Column 2 收起（含 Home 状态）时取消 `w-[296px]` 与右边框，改以 `gap-1.5` 紧凑排布，并在右侧通过 `1px × 14px` 细竖分隔线（`bg-[#222222]`）与会话标题区分隔。
   - 内置控件统一 `26px`（`h-[26px]`）高度中线：
     - **侧栏折叠按钮**（`PanelLeft`，`26 × 26px`，`rounded-[6px]`，`aria-label="Toggle sidebar"`）：在对话态切换 Column 2 显隐；在 Home 态按下则保留 Home 画布同时展开 `Sessions` 栏。
     - **两段视图切换器**（`role="group"`，`aria-label="View mode"`，`h-[26px]`）：左段 `MessageSquare`（`Rendered conversation view`），右段 `Code2`（`Markdown source view`），直接调用 `setMarkdownMode("render" | "source")`。
2. **右区（会话标题、拖拽留白与状态/动作控制簇）**：
   - 左侧会话/Home 切换按钮（`h-[26px]`，纯文字排版无重复图标）：显示当前标题（Home 时为 `"Home"`，会话态为 `sessionTitle`）、工作区短名（`· {currentProject}`）及 `ChevronDown`，点击打开 `SessionBrowserDialog`。
   - 中间弹性拖拽空白（`<div data-tauri-drag-region className="h-full min-w-[16px] flex-1" />`）：保证拖拽空白顶栏即可移动窗口，不误触按钮。
   - 右侧控制簇：浏览器预览场景切换条（仅预览模式）、Goal 运行状态胶囊（`data-testid="goal-topbar-pill"`，琥珀色 `#1c1a16` / `#3d3527` / `#e8d9b0`）、工作模式胶囊与权限模式胶囊（统一 `h-[26px] rounded-[6px] border-[#262626] bg-[#181818]`）、`26 × 26px` 快捷图标按钮组（非 Home 时显示 `PanelRight` 切换 Inspector、`Globe` 切换 Split Browser、`Folder` 打开设置）以及 `More actions` 溢出菜单。

### 3.3 多列工作区与 Home 侧栏自动收起规则

统一顶栏之下为 `<div className="flex min-h-0 flex-1 overflow-hidden">`，按需呈现以下列：

- **Column 1 — Primary Rail（`48px`，`#0d0d0d`）**：
  - 顶部导航图标依次为：`Home`（`"nav"`）、`Sessions`（`"sessions"`）、`Search`（`"search"`）、`Memory`（`"memory"`）、`MCP & Credentials`（`"mcp"`）、`Export`（`"export"`）、`Customize`（`"customize"`），选中项左侧渲染 `2px` 宽 `#d9a08e` 指示条。
  - 底部固定区放置 `Usage overview`（`HelpCircle`）与**唯一用户头像菜单触发器**（`aria-label="User menu"`，弹出 `data-testid="user-menu-popover"` 向右侧 `left-[calc(100%+10px)]` 展开）。将该菜单锚定在 Primary Rail 底部，保证 Home 状态或手动收起 Column 2 后仍可稳定访问账户与偏好设置。
- **Column 2 — Sidebar Panel（`248px`，`#0f0f0f`）**：
  - 仅在 `sidebarVisible = activePanel !== "nav" && !sidebarCollapsed` 为真时渲染（`data-testid="sidebar-panel"`）。
  - **Home 收起约定**：进入 Home 空态（或点击 Rail `Home`）时 `activePanel === "nav"`，Column 2 自动收起，主区域与 `48px` Primary Rail 直接衔接；从 Home 提交首个 Prompt、恢复历史会话或点击 Rail 任一功能面板（`Sessions` / `Search` / `Memory` / `MCP` / `Export` / `Customize`）或点击顶栏 `Toggle sidebar` 时，自动切换并展开对应面板。
- **Column 3 — Main Surface（`#141414`，`flex-1`）**：
  - `UpdaterBanner`、错误横幅、`Configuration incomplete` 配置提示条与 `recoveryBanner` 统一放在顶栏下方、工作区上方，避免占用系统顶栏热区。
  - 内部支持 Conversation 与右侧 `Split Browser Preview`（`w-[44%] min-w-[320px]`，`data-testid="split-browser-pane"`）并排分栏。
- **Column 4 — Right Inspector（`ContextSidebar`）**：
  - 仅在 `inspectorOpen && !isHomeEmpty && !browserPaneOpen` 时显示。

---

## 4. 核心组件规范

### 4.1 统一胶囊与表单控件

定义于 [desktop/src/index.css](../../desktop/src/index.css)：

- **`.soyo-pill`**：静态/下拉信息胶囊，背景 `#1c1c1c`、边框 `1px solid #2e2e2e`、圆角 `8px`、`padding: 3px 10px`、`font-size: 12px`（在 `38px` 顶栏内收紧为 `h-[26px]`、`rounded-[6px]`、`px-2.5 py-0`、`text-[11.5px]` 统一中线）。
- **`.soyo-pill-btn`**：交互动作胶囊按钮，`padding: 5px 14px`，悬停背景 `#242424` / 边框 `#3a3a3a`，按下微缩 `scale(0.98)`。
- **`.soyo-input`**：深色输入域（`#161616` 底、`#2b2b2b` 框、`8px` 圆角），聚焦时边框变为 `#d9a08e` 并附 `0 0 0 2px rgba(217, 160, 142, 0.14)` 光晕。
- **`.soyo-select` 与 `.soyo-select.is-inline`**：使用双线性渐变渲染纯 CSS 向下箭头（无 `<svg>` data URI）。
- **`.soyo-checkbox`**：`15 × 15px`、`4px` 圆角、勾选填充 `#d9a08e` 并以纯 CSS 边框旋转生成深色对勾。
- **`.soyo-switch`**：矩形滑块开关（`34 × 20px` 轨道 `border-radius: 5px`、`15 × 15px` 白色滑块 `border-radius: 4px`）；常规开启色 `#d9a08e`，Ultracode 开启态（`.is-ultra`）使用 `#8a7bd8`。
- **`.soyo-composer-card`**：输入框卡片聚焦时过渡边框 `#3d3d3d` 与强调晕影。

### 4.2 内联 Diff 预览（`.soyo-diff*`）

工具结果中检测到统一 Diff 块时渲染 `.soyo-diff` 容器（圆角 `10px`，底色 `#131313`，边框 `#262626`）：

- 顶栏 `.soyo-diff-header`（底 `#161616`）展示文件名、新增/删减行数徽标与 hunk 摘要。
- 行级样式 `.soyo-diff-line` 含 `36px` 行号槽（`.soyo-diff-line__ln`）、`16px` 符号槽（`.soyo-diff-line__sg`）与代码主体（`.soyo-diff-line__code`）：
  - 新增行 `.is-add`：浅绿背景 `rgba(74, 222, 128, 0.07)`、符号 `#4ade80`。
  - 删除行 `.is-del`：浅红背景 `rgba(248, 113, 113, 0.06)`、符号 `#f87171`。
  - Hunk 头部 `.is-hunk`：底色 `#181818`、文字 `#7aa2f7`。

### 4.3 Usage 统计卡片与堆叠柱状图（`SettingsDialog`）

在 [desktop/src/components/SettingsDialog.tsx](../../desktop/src/components/SettingsDialog.tsx) 的 Usage 面板中：

- **Token 数量格式化**：[desktop/src/lib/sanitize.ts](../../desktop/src/lib/sanitize.ts) 的 `formatTokenCount` 按量级缩放，防止百万/十亿级数值退化为过长的 `k` 单位：
  - `>= 1_000_000_000` → `.toFixed(1) + "B"`
  - `>= 1_000_000` → `.toFixed(1) + "M"`
  - `>= 1000` → `.toFixed(1) + "k"`
  - `< 1000` → 原样数字字符串
- **堆叠柱状图 Tooltip 层级与边缘对齐**：
  - 若直接把 `hover:brightness-110` 写在含 Tooltip 的 `.group` 外层容器上，CSS `filter` 会新建堆叠上下文，导致相邻后续柱子遮挡当前 Tooltip。
  - 修复约定：外层柱容器只保留定位与提升层级的 `hover:z-30 focus-within:z-30`；仅将 `group-hover:brightness-110` 与 `overflow-hidden rounded-[2px]` 应用在包裹三色堆叠段（input `#d9a08e`、output `#8aa8d8`、cache read `#6b6b6b`）的内层 `<div>` 上。
  - Tooltip 使用 `z-50 border-[#383838] bg-[#161616] shadow-[0_12px_32px_rgba(0,0,0,0.8)]`，并根据索引边缘自适应水平对齐（前 3 根 `left-0`、后 3 根 `right-0`、其余 `left-1/2 -translate-x-1/2`），避免贴边裁切。

### 4.4 语言切换与本地化（`i18n` / `LanguageDialog`）

- **首版支持语言**：定义于 [desktop/src/lib/i18n.ts](../../desktop/src/lib/i18n.ts)，支持 `en-US`（`English (United States)`，默认）、`zh-CN`（`简体中文 (中国)`）与 `zh-TW`（`繁體中文 (台灣)`），由 `useAppStore` 维护并在切换时同步写入 `document.documentElement.lang`（不使用 `localStorage` 等禁止存储 API）。
- **入口 1 — User Menu `Language` 项**：在左下角 Primary Rail 的 `User menu` 弹出菜单中，`Settings`（带 `⌘,` 快捷键标识）下方提供带 `Globe` 图标的 `Language` 入口（`data-testid="user-menu-language"`），点击打开 [desktop/src/components/LanguageDialog.tsx](../../desktop/src/components/LanguageDialog.tsx)。
- **入口 2 — Settings `General` 标签页**：在 [desktop/src/components/SettingsDialog.tsx](../../desktop/src/components/SettingsDialog.tsx) 的常规设置中提供 `<select aria-label="Interface language">` 同步切换。
- **选择弹窗规范（`LanguageDialog`）**：居中模态窗宽 `max-w-[580px]`（底色 `#1e1e1e`、边框 `#2b2b2b`、圆角 `14px`），使用 `grid-cols-1 sm:grid-cols-3` 三列单选网格展示双行语言卡片（母语名称 + 副标题）；当前选中卡片使用 `#242c3a` 背景、`#38465e` 边框与 `#6ea1f7` 勾选标记，支持点击空白遮罩或 `Escape` 键关闭。
- **全页面本地化覆盖**：除主框架（顶栏、侧栏、Home、Composer、ContextSidebar、Split Browser Preview）与设置弹窗（含 Usage 统计标签与堆叠图提示）外，同步覆盖所有次级弹窗与状态视图——[SessionBrowserDialog](../../desktop/src/components/SessionBrowserDialog.tsx)、[McpCredentialsDialog](../../desktop/src/components/McpCredentialsDialog.tsx)、[MemoryDialog](../../desktop/src/components/MemoryDialog.tsx)、[ExportDialog](../../desktop/src/components/ExportDialog.tsx)、[ModulesPanel](../../desktop/src/components/ModulesPanel.tsx)、[ApprovalDialog](../../desktop/src/components/ApprovalDialog.tsx)、[ClarificationDialog](../../desktop/src/components/ClarificationDialog.tsx)、[EffortPopover](../../desktop/src/components/EffortPopover.tsx)、[ThinkingCardView](../../desktop/src/components/ThinkingCardView.tsx)、[ToolCardView](../../desktop/src/components/ToolCardView.tsx)、[AttachmentPreview](../../desktop/src/components/AttachmentPreview.tsx) 与 [UpdaterBanner](../../desktop/src/components/UpdaterBanner.tsx)。
- **Home 问候语（`home.greetings` / `pickGreeting`）**：问候语按时段与随机池抽取，挂载时抽取一次、切换语言时重新抽取。时段分桶各存 3 条候选：05–12 `morning`、12–18 `afternoon`、18–23 `evening`、23–05 `night`，桶内均匀抽取；另有 15% 概率从 `anytime` 池（20+ 条，如 `You're here!`、`How can I help, {name}?`、`今天在想什么？`）抽取以制造变化。`{name}` 由 `formatGreeting` 按 `昵称 → OS 账户名 → 本地化兜底词`（`friend` / `朋友`）解析。Figma 各帧展示不同取值（`Good morning` / `Good evening` / `You're here!`）以覆盖该可变状态。

### 4.5 个人资料与全局指令（`desktop_prefs` / `~/.soyo/AGENTS.md`）

- **个人资料卡**（Settings → General）：头像更换使用原生图片选择器（PNG/JPEG/WebP/GIF，≤4 MB），图片以 data URL 存入 `~/.soyo/desktop_prefs.json`，`Reset` 恢复应用图标；昵称可覆盖问候语与 User menu 显示名，留空回退 OS 账户名。
- **全局指令卡**（Settings → General）：textarea 直接编辑用户级 `~/.soyo/AGENTS.md`；该文件由运行时作为 `user` 作用域指令注入所有会话，**不读写项目内 `AGENTS.md`**。读取按 `.soyo → .saya → .agents` 顺序兼容旧路径，写入固定 `.soyo`。

---

## 5. 动效规范与可访问性

### 5.1 窗口内动效原语（`index.css`）

| 类名 | 关键帧 | 时长与曲线 | 用途 |
| :--- | :--- | :--- | :--- |
| `.soyo-backdrop-enter` | `soyo-fade-in` | `160ms cubic-bezier(0.16, 1, 0.3, 1)` | 对话框半透明遮罩入场 |
| `.soyo-dialog-enter` | `soyo-dialog-in` | `200ms cubic-bezier(0.16, 1, 0.3, 1)` | 模态弹窗轻位移（`6px`）与缩放（`0.975 -> 1`） |
| `.soyo-popover-up` | `soyo-popover-up` | `150ms cubic-bezier(0.16, 1, 0.3, 1)` | 底部向上弹出菜单（User / Composer） |
| `.soyo-popover-down` | `soyo-popover-down` | `150ms cubic-bezier(0.16, 1, 0.3, 1)` | 顶栏向下弹出菜单（Mode / Perm / More） |
| `.soyo-panel-enter` | `soyo-panel-in` | `160ms cubic-bezier(0.16, 1, 0.3, 1)` | Column 2、Goal 胶囊与分栏切换入场 |
| `.soyo-msg-enter` | `soyo-msg-in` | `180ms cubic-bezier(0.16, 1, 0.3, 1)` | 对话消息气泡入场 |
| `.soyo-pulse-dot` | `soyo-pulse-soft` | `1.6s ease-in-out infinite` | 活跃/思考状态脉动圆点 |
| `.soyo-switch__knob` | — | `transform 180ms cubic-bezier(0.34, 1.3, 0.64, 1)` | 开关滑块轻微过冲位移动画 |

### 5.2 无障碍与键盘约定

- **减弱动效降级**：在 `@media (prefers-reduced-motion: reduce)` 下全面将 `.soyo-*` 及 `.ccs-*` 的 `transition` 与 `animation` 设为 `none`。
- **焦点环**：全局交互控件的 `:focus-visible` 统一采用 `1.5px solid rgba(217, 160, 142, 0.55)`，`outline-offset: 1.5px`。
- **键盘导航**：非阻塞对话框支持 `Escape` 快速关闭；顶栏菜单失焦或外部点击自动收起。

---

## 6. 图标资产目录与构建规范

全部设计原稿与主导出成品归档于 [docs/design/assets/](assets/)：

| 资产文件 | 用途与参数 |
| :--- | :--- |
| [assets/icon.json](assets/icon.json) | Xcode Icon Composer 描述清单：背景 `automatic-gradient`（`display-p3:0.00935,0.02207,0.04118,1.00000`），图层引用 `soyo-harness-icon-transparent.png`，`neutral` 阴影 `opacity: 0.5`，半透明开启 `value: 0.3`，`squares: "shared"`。 |
| [assets/soyo-harness-icon-transparent.png](assets/soyo-harness-icon-transparent.png) | `1024 × 1024` 8-bit/color RGBA 透明底矢量导出前景原稿。 |
| [assets/soyo-icon-macos-1024.png](assets/soyo-icon-macos-1024.png) | 使用 Xcode `ictool` 从 Icon Composer `.icon` 工程导出的 `1024 × 1024` macOS 主视觉图。 |

### 6.1 运行时与打包目标规格

由上述母版衍生并受 [scripts/release/test_desktop_packaging.py](../../scripts/release/test_desktop_packaging.py) 校验的运行时图标资产：

| 目标路径 | 尺寸 / 格式 | 用途 |
| :--- | :--- | :--- |
| [desktop/public/app-icon.png](../../desktop/public/app-icon.png) | `256 × 256` 8-bit RGBA PNG | 前端顶栏、Home 欢迎标题与左下角头像展示（统一使用 `rounded-[22%]` 圆角矩形裁切） |
| [desktop/src-tauri/icons/icon.png](../../desktop/src-tauri/icons/icon.png) | `512 × 512` 8-bit RGBA PNG | 通用桌面应用图标 |
| [desktop/src-tauri/icons/128x128@2x.png](../../desktop/src-tauri/icons/128x128@2x.png) | `256 × 256` 8-bit RGBA PNG | Tauri 高分屏 Bundle 图标 |
| [desktop/src-tauri/icons/128x128.png](../../desktop/src-tauri/icons/128x128.png) | `128 × 128` 8-bit RGBA PNG | Tauri 标准分辨率 Bundle 图标 |
| [desktop/src-tauri/icons/32x32.png](../../desktop/src-tauri/icons/32x32.png) | `32 × 32` 8-bit RGBA PNG | 小尺寸窗口 / 托盘图标 |
| [desktop/src-tauri/icons/icon.icns](../../desktop/src-tauri/icons/icon.icns) | macOS `.icns` 集合 | 遵循 macOS HIG `824 / 1024` 安全区边距 |
| [desktop/src-tauri/icons/icon.ico](../../desktop/src-tauri/icons/icon.ico) | Windows 多尺寸 `.ico` | Windows NSIS 安装包与应用可执行文件图标 |

---

## 7. Figma 设计稿同步工具

设计原稿维护在 Figma 文件 `ocT9XcOCUCifMHyta771gs` 的 `Theme` 页（节点 `20:4`），14 个 `1520 × 980` frame 按 `1520 / 980` 步长网格排列。

[devin_theme_gen.py](devin_theme_gen.py) 是全部 frame 的单源生成器：运行后把每个 frame 写成独立 HTML（内嵌 base64 图标与样式），再经 html_to_figma 导入替换同名 frame。

```bash
python3 docs/design/devin_theme_gen.py            # 输出到 /tmp/soyo_devin_theme/
python3 docs/design/devin_theme_gen.py <out_dir>  # 自定义输出目录
```

- 脚本以自身位置解析仓库根目录，读取 `desktop/public/app-icon.png` 注入各 frame；任意目录下均可运行。
- 修改设计时改脚本后重新生成，再对变更 frame 执行导入替换；不要直接在 Figma 里手改样式以免与单源失同步。

### .pen 迁移

同一套设计稿已迁移为仓库内的 pen.dev 文档 [`desktop-ui.pen`](desktop-ui.pen)：单个 `.pen` 文件包含全部 14 个 frame（4 列网格排布），内嵌图片资源落在 [`assets/`](assets/) 并以 `./assets/<hash>.png` 相对引用。节点为绝对定位的 `frame`/`rectangle`/`text`/`path`（SVG 保留 `viewBox` 与 path 几何、lucide 描边色与线宽按缩放还原），渐变与阴影按 .pen fill/effect 结构映射；虚线描边、分侧描边色等 CSS 特性按最接近形式打平。

[html_to_pen.py](html_to_pen.py) 是迁移转换器：重新生成方式与 Figma 同步一致——先跑 `devin_theme_gen.py` 得到各 frame 的 HTML，再经本脚本（playwright + 系统 Chrome 计算布局）输出 `.pen`：

```bash
python3 docs/design/devin_theme_gen.py /tmp/soyo_frames
python3 docs/design/html_to_pen.py /tmp/soyo_frames docs/design/desktop-ui.pen
```

转换器依赖 `playwright`（venv 中安装即可，`playwright install` 不需要——直接驱动系统 Chrome）。`.pen` 与 Figma 设计稿同源于生成脚本，修改设计仍以生成器为准，改完同时重跑两条同步路径。
- 当前 frame 序列：`01 Home · 02 Session+Context · 03 Approval · 04 Settings · 05 User Menu · 06 Sessions · 07 Attach Menu · 08 Browser Preview · 09 Subagent & Goal · 10 Usage & Tokens · 11 Slash Commands · 12 Attachments Preview · 13 Model & Effort · 14 Language`。

---

## 8. D-075 目标规范（已确认，待实施）

本节是 D-075 确认的目标规范，来源是[第二轮审查报告](../reviews/review-2026-11-19.md)第 6 节，并在此基础上追加 Soyo Light 亮色主题。代码尚未按本节修改；§1–§7 仍描述现行实现，每个阶段落地后再改写对应条目。

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
