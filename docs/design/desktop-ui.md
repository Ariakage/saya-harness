# Soyo Harness Desktop — UI/UX 设计规范

> **状态**：设计稿已确认（Figma）且已接入 `desktop/src/`（Devin Theme 全套布局与 `cc_eff` 五档 Effort WebGL2 滑块）。设计稿载体：Figma 文件 `ocT9XcOCUCifMHyta771gs` 的 **Page 3（`03 · Devin Theme`，节点 `20:4`）**，共 13 帧，每帧 1280×720，均为可编辑图层。
>
> Page 2（`6:4`）保留旧版 11 屏参考稿，不再演进。

## 1. 范围

- 适用于 Tauri 2 + React/Vite 的独立桌面应用（`desktop/`），与 CLI/TUI 共享同一运行时、XDG 配置、SQLite 会话库与凭据存储；可独立安装，也可与 CLI/TUI 并存。
- 设计基线为近黑深色主题（"Devin Theme"），应用图标（`desktop/public/app-icon.png`）作为产品标识，替代一切第三方 logo。

## 2. 信息架构

单窗口三栏结构：

```
┌────────┬───────────┬──────────────────────┬────────────┐
│ rail   │ sidebar   │ main                 │ inspector  │
│ 图标轨  │ Sessions  │ 消息流 / 空态 / 对话框 │ Runtime/   │
│ 48px   │ ~250px    │                      │ Context 等 │
│        │ + New     │                      │ ~300px     │
│        │ + 项目分组 │                      │            │
└────────┴───────────┴──────────────────────┴────────────┘
```

- **rail（最左图标轨）**：Home / Chat / Search / Sessions / Memory / MCP & Creds / Export / Customize；底部用户芯片（头像入口）。
- **sidebar（会话列）**：`New session` 链接、`Pinned` 分组、按 **project 分组**的 recent 列表（每组标题行 = 项目名 + 会话数，组下挂该项目的最近会话）。示例分组：`saya-harness`、`AkaMemo`、`ai-gateway`。
- **main**：会话标题栏（标题 · 项目名，右侧模式/权限丸 + `…`）、消息流、底部 Composer。
- **inspector（右栏）**：Runtime（Profile/Model/Sandbox/Workspace）、Context window 分解条、Tasks、Outputs。

空态（01）为居中问候 + Composer + `Project or folder` + Ideas 建议列表。会话浏览器（06）为列表 + 详情两栏。

## 3. 视觉令牌

### 3.1 颜色

| 令牌 | 值 | 用途 |
| --- | --- | --- |
| bg-rail | `#0d0d0d` | 图标轨、最深底 |
| bg-sidebar | `#0d0d0d`/`#111` | 侧栏 |
| bg-main | `#141414` | 主区 |
| bg-card | `#1a1a1a`–`#1d1d1d` | 卡片/对话框 |
| bg-pill | `#1c1c1c` | 徽标/按钮底 |
| border | `#1f1f1f`–`#2e2e2e` | 分隔线与细边 |
| text | `#e8e8e8` | 正文 |
| text-2 | `#a3a3a3` | 次级 |
| text-3 | `#6e6e6e`–`#7a7a7a` | 弱化/元信息 |
| **accent** | `#d9a08e`（图标发色）| 品牌高亮 |
| accent-link | `#e2a68f` | 链接态 accent |
| md-link | `#8aa8d8` | **Markdown 正文内超链接（保持蓝色）** |
| green | `#4ade80` | done/Allow/CODE/On |
| amber | `#fac454` | running/Manual Approval/警告 |
| red | `#f87171` | Deny/错误/取消语义 |
| blue | `#7aa2f7`/`#8aa8d8` | 信息型标签/命令名/PR 链接 |
| purple | `#8a7bd8`/`#a096eb` | Ultracode 专属 |
| goal | `#1c1a16`/`#3d3527`/`#e8d9b0` | Goal 常驻 pill（琥珀系） |

### 3.2 accent 与蓝色的分工（硬性规则）

- **accent 棕**用于实体高亮件：激活发送按钮、开关、CTA、选中态、UI 层链接（`New session`、`Answer questions`、`Manage profiles…`）、Context 分解条 Messages 主段、图表 input 段、subagent 左边条。
- **蓝色**仅用于：Markdown 正文内超链接、信息型标签（命令名、PR 号、`shell.exec` 这类动作标识）、权限芯片（`auto`、`Bypass permissions`）。
- 语义色（绿/琥珀/红）独立于 accent，不因主题替换。

## 4. 组件规范

### 4.1 统一 pill（唯一 pill 规格）

所有徽标、状态丸、模式/权限丸、Deny/Allow 按钮、工具丸共用：

- 底 `#1c1c1c`，边 `1px #2e2e2e`，圆角 `8px`，字号 `12px`，内边距 `3px 10px`（按钮 `5px 14px`）
- **语义只靠文字色**：done/Allow/CODE/On=绿，running/Manual Approval=琥珀，Deny/错误=红，profile/info=蓝，普通=灰
- 彩色底只允许两处：激活发送钮（accent 底 + 深色图标）、`Apply`/主 CTA（accent 文字或描边）

### 4.2 消息流

- **普通 assistant 消息无底框**，正文直接铺页面背景；用户消息保留右侧气泡（`#242424` 圆角）。
- 工具调用为**扁平行**：`>_` 图标 + 等宽工具名 + 状态 pill + 右侧元信息（路径/行数/耗时），无容器边框。
- thinking 两态：收起 `› Thought for 8 s` + `auto-collapsed` pill；展开 `⌄ Thinking` + 左细引导线 + 弱化正文。
- **Diff 预览**：文件编辑类工具行右侧附行数统计（`+n` 绿 / `-n` 红）；展开后为统一 diff 卡——头部 `文件路径 +n -n · N hunks ›`，正文等宽行：`@@` hunk 头蓝字、上下文行灰、`+` 行绿字绿底（`rgba(74,222,128,.07)`）、`-` 行红字红底（`rgba(248,113,113,.06)`）、行号列弱化、底部 `N unchanged lines hidden · expand ›` 折叠提示。
- 用户消息 hover 才显示操作行：时间戳 + Branch / Retry / Edit / Copy 图标。

### 4.3 Composer

- 大卡圆角容器；左上 `+`（附件菜单：Add files or photos / Skills / Connectors / Add plugins）、回形针、`Web search` 丸、工具 iconchip（中性）。
- 右侧状态链：`● CODE`（模式 + 色点）、`auto`（权限）、`claude-sonnet-4-5 Max`（模型）、`Steer ⌄`、动作钮。
- **动作钮单键变形**（运行中）：空输入 = accent 圆钮内 `■`（点按停止）；有输入 = `↑`（steer 发送）。**不再出现并列的红叉取消钮**。默认动作是引导（Steer），`⌄` 可切 Queue。
- 输入消息预览：虚线 `Message preview` 区渲染 markdown + 附件摘要。
- 附件：chip 行（图片缩略图 / 文件卡 + `✕` 移除）。
- `/` 唤起命令面板：等宽命令名 + 描述（`/model /mode /permission /compact /export /sessions /memory /mcp`）。

### 4.4 审批

- 审批是**消息流内的内联卡片**（琥珀警示容器），非弹窗：标题 `⚠ Approval required` + 模式 pill；字段表 Tool/Action/Call/Workspace；命令块；`Deny (N)`/`Allow (Y)` 统一 pill 按钮（红/绿字）；底部说明行。
- 暂停时 Composer 显示 `paused · approval required`，发送钮 disabled。

### 4.5 Effort 滑块（13 帧）

按 `cc_eff` 参考实现的静态稿：

- 24px 满高轨道（`rgba(255,255,255,.10)`），填充 = 灰→紫渐变 + 全幅紫色点阵能量格（左淡出 mask）
- 白色 `20×24` 平头 thumb（r4、深色描边阴影、Ultracode 态紫色辉光 `0 0 22px`）
- 4 个 3px 刻度点 + Faster/Smarter 端标 + `?` 帮助钮
- Ultracode 档位与开关用紫色；Model profile 菜单为等宽 profile 名 + ✓ 选中 + `Manage profiles…`（accent）

### 4.6 开关与滑块

- 开关为**圆角长方形**：`34×20` 轨道（r5）+ `15×15` 方角旋钮（r4）；开=accent 底，关=`#3a3a3a`。

### 4.7 用量面板（10 帧）

- 段选 `7d/30d/90d`；CLI/TUI/Desktop 三卡（Input/Output/Cache read）
- `Tokens per day` **堆叠柱**：input=accent / output=蓝 / cache read=灰 + 图例色块；像素高度（≤88px）防溢出
- 底部 live 行：`● live session`、`142 tok/s`、本会话 token、ctx 占比

### 4.8 Subagent 与 Goal

- subagent 卡：accent 左边条、`subagent_explore` profile pill、状态 pill、嵌套工具行（左引导线）、`3 tool calls · 4.2k tok` 汇总
- Goal 常驻顶栏 pill：`⚑ 目标名 · 计时 · ⏸暂停 · ⏹停止`（琥珀系）
- 流式状态行：`● streaming · 142 tok/s · 12.4k tokens this run · elapsed 02:41`

### 4.9 浏览器预览（08 帧）

会话区与浏览器分栏；浏览器面板含前进/后退/刷新、URL 栏（`localhost:5173`）、外开/关闭钮，视口内嵌迷你版应用预览。

## 5. 交互规则

- 发送=默认引导（Steer）运行中的 run，`⌄` 可切 Queue；取消=同一 accent 钮的 `■` 态。
- 问候语可变：`Good morning/evening, [Username].`、`You're here!` 等。
- UI 链接用 accent；Markdown 超链接保持蓝色。
- 会话按 project 分组展示 recent。

## 6. 帧清单（Figma Page 3）

| # | 帧 | 节点 |
| --- | --- | --- |
| 01 | Home (Empty) | `39:2` |
| 02 | Session — Conversation + Context | `79:2` |
| 03 | Session — Approval Card | `77:2` |
| 04 | Settings Dialog | `84:2` |
| 05 | User Menu | `42:2` |
| 06 | Session Browser | `78:2` |
| 07 | Composer — Attach Menu | `43:2` |
| 08 | Session — Browser Preview | `80:2` |
| 09 | Session — Subagent & Goal | `82:2` |
| 10 | Usage & Tokens | `85:2` |
| 11 | Composer — Slash Commands | `54:2` |
| 12 | Composer — Attachments & Preview | `47:2` |
| 13 | Composer — Model & Effort | `81:2` |

## 7. 待定项

- 设计稿为 plain layers，未绑定 Figma 变量/组件；后续可抽取第 3 节令牌建 variables 并回绑。
- 命令面板命令名、PR 号、`shell.exec` 等"信息蓝"的归类（UI 控件 vs 元信息）如再调整需同步第 3.2 节规则。
- 生成器脚本当前在 `/tmp/devin_theme_gen.py`（产出 `/tmp/soyo_devin_theme/*.html`），属设计工具链，不入库；如需沉淀可另定位置。
