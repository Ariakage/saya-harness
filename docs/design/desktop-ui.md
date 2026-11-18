# Soyo Desktop 设计规范（Soyo Dark）

本文档定义 `soyo-desktop` 的视觉语言、窗口布局、组件约定、动效规范与图标资产清单，对应实现位于 [`desktop/src/`](../../desktop/src/) 与 [`desktop/src-tauri/`](../../desktop/src-tauri/)。

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
- 当前 frame 序列：`01 Home · 02 Session+Context · 03 Approval · 04 Settings · 05 User Menu · 06 Sessions · 07 Attach Menu · 08 Browser Preview · 09 Subagent & Goal · 10 Usage & Tokens · 11 Slash Commands · 12 Attachments Preview · 13 Model & Effort · 14 Language`。
