import {
  createContext,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";

export type Lang = "en" | "zh-CN";

const en = {
  "nav.loop": "The loop",
  "nav.modes": "Modes",
  "nav.permissions": "Permissions",
  "nav.isolation": "Isolation",
  "nav.surfaces": "Status",
  "nav.download": "Download",
  "nav.github": "GitHub",

  "hero.title":
    "Hand Soyo a bug fix, a failing test, or a week-long migration.",
  "hero.tagline":
    "You set the mode and the permissions. Soyo drives the model ↔ tool loop itself and keeps working until the run ends.",
  "hero.cta.start": "Get started",
  "hero.cta.docs": "Documentation",
  "hero.command": "cargo install --path crates/soyo-cli --locked",
  "hero.copy": "Copy install command",
  "hero.copied": "Copied",
  "hero.availability": "Available for macOS 15+ and Linux (x86_64 / aarch64).",
  "hero.surfaces": "CLI and TUI ship in 0.2.0 · desktop GUI in development",

  "win.title": "Fix the failing test",
  "win.project": "soyo-harness",
  "win.sessions": "Sessions",
  "win.new": "New session",
  "win.pinned": "Pinned 1",
  "win.pinnedTitle": "Hi, I'm Soyo…",
  "win.pinnedSub": "Answer questions",
  "win.viewAll": "View all",
  "win.recent1": "Fix the failing test",
  "win.recent1.sub": "code · running",
  "win.recent2": "Plan the network broker",
  "win.recent2.sub": "plan · saved",
  "win.recent3": "Export session 0x3f2a",
  "win.recent3.sub": "work · completed",
  "win.recent4": "Migrate config to .soyo",
  "win.recent4.sub": "build · saved",
  "win.composer": "Steer the run, or press Esc / Stop…",
  "win.attach": "Attach",
  "win.websearch": "Web search",
  "win.compact": "Compact",
  "win.model": "claude-sonnet-4-5",
  "win.effort": "Effort: Medium",
  "win.steer": "Steer",
  "win.hint.enter": "Enter send",
  "win.hint.newline": "Shift+Enter newline",
  "win.hint.tab": "Empty Tab cycle mode",
  "win.hint.esc": "Esc cancel",
  "win.done": "done",
  "win.error": "error",
  "win.streaming": "streaming",
  "win.exec.summary": "cargo test -p soyo-core · exit 1",
  "win.read.summary": "src/loop.rs · 90 lines",
  "win.hunk": "1 hunk ›",
  "win.summary": "claude-sonnet-4-5 · 1.8s",
  "win.usage": "~↑12.4k ↓340",

  "loop.title": "The loop is the product",
  "loop.body":
    "Soyo calls the model, executes the tools it asks for, hands the results back, and goes again — until the run ends, you cancel, or a budget runs out. Every boundary returns an explicit error; nothing fails silently.",
  "loop.caption":
    "Cancellation, timeouts and budgets apply to model calls, tool runs and the loop itself.",
  "loop.demo.user": "Fix the failing test in crates/soyo-core",
  "loop.demo.thinking": "I'll reproduce the failure first, then patch the assertion.",
  "loop.demo.tool.exec": "exec",
  "loop.demo.tool.cmd": "cargo test -p soyo-core",
  "loop.demo.tool.exit": "exit 1",
  "loop.demo.tool.edit": "edit_file",
  "loop.demo.tool.file": "src/loop.rs",
  "loop.demo.diff.file": "crates/soyo-core/src/loop.rs",
  "loop.demo.done": "Fixed — the rerun passes.",
  "loop.step.model": "model call",
  "loop.step.tools": "tool runs",
  "loop.step.results": "results back",
  "loop.step.repeat": "next turn",

  "modes.title": "Four modes, one session",
  "modes.body":
    "You switch modes by hand; the agent may suggest one. The change lands between invocations and the conversation keeps its context.",
  "modes.work.desc": "General tasks. The default for every new session.",
  "modes.code.desc": "Reading, writing and debugging code.",
  "modes.plan.desc":
    "Read-only investigation that ends in a plan — the workspace is never modified.",
  "modes.build.desc": "End-to-end implementation and delivery.",

  "perm.title": "Permissions in three gears",
  "perm.body":
    "Permission and work modes are orthogonal: pick both independently.",
  "perm.auto.name": "Auto",
  "perm.auto.desc":
    "The default. Rule checks plus CST structural analysis of every command, an optional review model, and human confirmation where the rules ask for it.",
  "perm.manual.name": "Manual approval",
  "perm.manual.desc": "Every tool call waits for your approval.",
  "perm.bypass.name": "Bypass permissions",
  "perm.bypass.desc":
    "Skips human confirmation — PLAN read-only, explicit deny rules and the sandbox boundary still hold. Must be enabled in settings first.",

  "isolation.title": "Tools run inside a native sandbox",
  "isolation.body":
    "Default-on isolation, not an opt-in flag. When required constraints can't be met, the run fails closed instead of silently degrading.",
  "isolation.item1.title": "Platform backends",
  "isolation.item1.desc":
    "macOS Seatbelt and Linux Bubblewrap + Seccomp behind one interface, with container and VM implementations reserved.",
  "isolation.item2.title": "Deny-by-default network",
  "isolation.item2.desc":
    "A domain-allowlist broker mediates HTTP and CONNECT traffic; everything else never leaves.",
  "isolation.item3.title": "CST command analysis",
  "isolation.item3.desc":
    "Commands are parsed as concrete syntax trees, so risk checks see shell structure — not just strings.",
  "isolation.item4.title": "Credentials stay referenced",
  "isolation.item4.desc":
    "Config names environment variables; secrets are never written to files, logs or examples.",

  "surfaces.title": "One runtime, three surfaces",
  "surfaces.body":
    "CLI, TUI and the upcoming GUI reuse the same agent runtime — none of the loop lives in a terminal renderer or a GUI framework.",
  "surfaces.cli.name": "CLI",
  "surfaces.cli.desc": "Interactive sessions and one-shot `run` for scripts.",
  "surfaces.tui.name": "TUI",
  "surfaces.tui.desc": "The default `soyo` entry point.",
  "surfaces.gui.name": "GUI",
  "surfaces.gui.desc": "Shared application services, desktop next.",
  "surfaces.shipped": "Shipped in 0.2.0",
  "surfaces.dev": "In development",
  "surfaces.strip.title": "Under the hood",

  "start.title": "Get started",
  "start.body":
    "Build from source, point the config at your model endpoint, and run. `config check` validates TOML and environment references without network or paid calls.",
  "start.releases": "Releases on GitHub",
  "start.docs": "Read the usage guide",
  "start.step1": "# build the workspace",
  "start.step2": "# create a config from the credential-free example",
  "start.step3": "# enter the default TUI",

  "dl.hero.eyebrow": "Download",
  "dl.hero.title": "Download Soyo",
  "dl.hero.sub":
    "One runtime, three surfaces — CLI and TUI ship today, the desktop GUI is next.",
  "dl.hero.cta.mac": "Download for macOS",
  "dl.hero.cta.linux": "Download for Linux",
  "dl.hero.cta": "Download",
  "dl.hero.releases": "All releases",
  "dl.hero.meta":
    "v0.2.0 · macOS 15+ and Linux · MIT · no account required",
  "dl.plat.title": "Choose your build",
  "dl.plat.sub":
    "Prebuilt archives from the v0.2.0 release — each contains the binary, MIT license, README and a BUILD-INFO.json provenance record.",
  "dl.plat.macArm": "macOS · Apple Silicon",
  "dl.plat.macIntel": "macOS · Intel",
  "dl.plat.linuxX64": "Linux · x86_64",
  "dl.plat.linuxArm": "Linux · ARM64",
  "dl.plat.windows": "Windows",
  "dl.plat.reqMac": "macOS 15+",
  "dl.plat.reqLinux": "glibc 2.35+",
  "dl.plat.download": "Download",
  "dl.plat.winSoon": "Not available yet",
  "dl.plat.rename":
    "0.2.0 archives ship under the former saya name — the binary inside is saya. Releases after the rename ship as soyo-*.",
  "dl.verify.title": "Verify your download",
  "dl.verify.body":
    "Every release ships SHA256SUMS and structured build evidence. Check the archive hash, then the source and binary records inside BUILD-INFO.json.",
  "dl.verify.sums": "SHA256SUMS",
  "dl.verify.evidence": "release-evidence.json",
  "dl.verify.how": "How releases are validated",
  "dl.src.title": "Prefer the source?",
  "dl.src.body":
    "Rust 1.99.1, edition 2024. Install the CLI to ~/.cargo/bin, or build the whole workspace.",
  "dl.src.clone": "# clone the repository",
  "dl.src.install": "# install the CLI to ~/.cargo/bin",
  "dl.src.build": "# or build the whole workspace",
  "dl.surf.title": "One binary, three surfaces",
  "dl.surf.sub":
    "The same archive ships every interface. The TUI is the default `soyo` entry point.",
  "dl.faq.title": "FAQ",
  "dl.faq.q1": "Which platforms does Soyo support?",
  "dl.faq.a1":
    "macOS 15+ on Apple Silicon and Intel, and Linux on x86_64 and ARM64 with glibc 2.35+. Windows is not supported yet.",
  "dl.faq.q2": "Why does the archive say saya?",
  "dl.faq.a2":
    "Soyo Harness was formerly Saya Harness. The 0.2.0 release predates the rename, so its archives, the saya binary and the .saya data directory keep the old name. Everything after the rename ships as soyo.",
  "dl.faq.q3": "macOS says the binary is damaged or unidentified — is it safe?",
  "dl.faq.a3":
    "The ARM build is ad-hoc signed and the Intel build unsigned; neither is notarized yet. Verify the SHA-256 against SHA256SUMS, then clear the quarantine flag — or build from source.",
  "dl.faq.a3.cmd": "xattr -d com.apple.quarantine ./saya",
  "dl.faq.q4": "How do I verify a download?",
  "dl.faq.a4": "Fetch the archive and SHA256SUMS from the release, then run:",
  "dl.faq.a4.cmd":
    "gh release download v0.2.0 --repo Ariakage/soyo-harness --pattern SHA256SUMS\nshasum -a 256 -c SHA256SUMS",
  "dl.faq.q5": "Do I need an account or subscription?",
  "dl.faq.a5":
    "No. Soyo talks to your own model endpoint — configure a profile in config.toml and reference credentials via environment variables. `soyo config check` validates everything offline.",
  "dl.faq.q6": "Where is the desktop app?",
  "dl.faq.a6":
    "The Tauri 2 desktop shell is in development in this repository. Today's release ships the CLI and the default TUI; watch Releases for the GUI.",
  "dl.back": "Back to the product tour",

  "footer.tagline": "Rust Agent Runtime",
  "footer.license": "MIT License",
  "footer.formerly": "Formerly Saya Harness.",
  "footer.github": "GitHub",
  "footer.releases": "Releases",
  "footer.issues": "Issues",
  "footer.docs": "Docs",
  "footer.support": "Support",

  "lang.label": "Language",
} as const;

export type MessageKey = keyof typeof en;

const zhCN: Record<MessageKey, string> = {
  "nav.loop": "执行循环",
  "nav.modes": "工作模式",
  "nav.permissions": "权限",
  "nav.isolation": "隔离",
  "nav.surfaces": "状态",
  "nav.download": "下载",
  "nav.github": "GitHub",

  "hero.title": "把 bug 修复、跑挂的测试、耗时一周的迁移交给 Soyo。",
  "hero.tagline":
    "你设定模式与权限——Soyo 自己驱动模型 ↔ 工具的执行循环，一直跑到任务结束。",
  "hero.cta.start": "开始上手",
  "hero.cta.docs": "使用文档",
  "hero.command": "cargo install --path crates/soyo-cli --locked",
  "hero.copy": "复制安装命令",
  "hero.copied": "已复制",
  "hero.availability": "支持 macOS 15+ 与 Linux（x86_64 / aarch64）。",
  "hero.surfaces": "CLI 与 TUI 已随 0.2.0 发布 · 桌面 GUI 开发中",

  "win.title": "修复失败的测试",
  "win.project": "soyo-harness",
  "win.sessions": "会话",
  "win.new": "新建会话",
  "win.pinned": "置顶 1",
  "win.pinnedTitle": "Hi, I'm Soyo…",
  "win.pinnedSub": "回答问题",
  "win.viewAll": "查看全部",
  "win.recent1": "修复失败的测试",
  "win.recent1.sub": "code · running",
  "win.recent2": "规划网络 broker",
  "win.recent2.sub": "plan · saved",
  "win.recent3": "导出会话 0x3f2a",
  "win.recent3.sub": "work · completed",
  "win.recent4": "迁移配置到 .soyo",
  "win.recent4.sub": "build · saved",
  "win.composer": "继续输入以引导运行，或按 Esc / 停止…",
  "win.attach": "附件",
  "win.websearch": "联网搜索",
  "win.compact": "压缩",
  "win.model": "claude-sonnet-4-5",
  "win.effort": "思考强度: 中",
  "win.steer": "引导",
  "win.hint.enter": "Enter 发送",
  "win.hint.newline": "Shift+Enter 换行",
  "win.hint.tab": "空框 Tab 切模式",
  "win.hint.esc": "Esc 取消",
  "win.done": "已完成",
  "win.error": "错误",
  "win.streaming": "流式输出中",
  "win.exec.summary": "cargo test -p soyo-core · exit 1",
  "win.read.summary": "src/loop.rs · 90 lines",
  "win.hunk": "1 hunk ›",
  "win.summary": "claude-sonnet-4-5 · 1.8s",
  "win.usage": "~↑12.4k ↓340",

  "loop.title": "产品本体就是这个循环",
  "loop.body":
    "Soyo 调用模型、执行模型请求的工具、把结果交回模型，然后继续下一轮——直到任务结束、被你取消或达到预算。每一处边界都返回明确错误，不静默失败。",
  "loop.caption": "取消、超时与预算同时约束模型调用、工具运行与循环本身。",
  "loop.demo.user": "修复 crates/soyo-core 里失败的测试",
  "loop.demo.thinking": "我先复现这个失败，再修改断言。",
  "loop.demo.tool.exec": "exec",
  "loop.demo.tool.cmd": "cargo test -p soyo-core",
  "loop.demo.tool.exit": "exit 1",
  "loop.demo.tool.edit": "edit_file",
  "loop.demo.tool.file": "src/loop.rs",
  "loop.demo.diff.file": "crates/soyo-core/src/loop.rs",
  "loop.demo.done": "修好了——重跑通过。",
  "loop.step.model": "调用模型",
  "loop.step.tools": "执行工具",
  "loop.step.results": "结果回传",
  "loop.step.repeat": "进入下一轮",

  "modes.title": "四种模式，同一会话",
  "modes.body":
    "模式由你手动切换，Agent 可以建议；在下一次调用前生效，会话上下文保留。",
  "modes.work.desc": "通用任务。新会话的默认模式。",
  "modes.code.desc": "代码的编写、修改与调试。",
  "modes.plan.desc": "只读调查并输出计划——不修改工作区文件。",
  "modes.build.desc": "端到端实现与交付。",

  "perm.title": "三档权限",
  "perm.body": "权限模式与工作模式分别处理，可以任意组合。",
  "perm.auto.name": "Auto",
  "perm.auto.desc":
    "默认档。规则检查 + 命令 CST 结构分析 + 可选的独立模型核查，规则要求处人工确认。",
  "perm.manual.name": "Manual approval",
  "perm.manual.desc": "每次工具调用都需要人工批准。",
  "perm.bypass.name": "Bypass permissions",
  "perm.bypass.desc":
    "跳过人工确认——PLAN 只读、显式 deny 与沙箱硬边界仍然生效。须先在设置中开启。",

  "isolation.title": "工具跑在原生沙箱里",
  "isolation.body":
    "隔离默认开启，不是可选开关。必需约束无法满足时，运行明确失败而不是静默降级。",
  "isolation.item1.title": "平台后端",
  "isolation.item1.desc":
    "macOS Seatbelt 与 Linux Bubblewrap + Seccomp 封装在同一接口下，预留容器与 VM 实现。",
  "isolation.item2.title": "默认拒绝的网络",
  "isolation.item2.desc":
    "域名白名单 broker 代理 HTTP 与 CONNECT 流量，其余连接不离开本机。",
  "isolation.item3.title": "CST 命令分析",
  "isolation.item3.desc":
    "命令按具体语法树解析，风险检查看到的是 shell 结构而非字符串。",
  "isolation.item4.title": "凭据只引用不落盘",
  "isolation.item4.desc":
    "配置只写环境变量名；密钥不写入配置文件、日志或示例。",

  "surfaces.title": "一个运行时，三个界面",
  "surfaces.body":
    "CLI、TUI 与后续的 GUI 复用同一个 Agent 运行时——循环逻辑不依赖终端渲染或某个 GUI 框架。",
  "surfaces.cli.name": "CLI",
  "surfaces.cli.desc": "交互会话与脚本化的单次 `run`。",
  "surfaces.tui.name": "TUI",
  "surfaces.tui.desc": "默认的 `soyo` 入口。",
  "surfaces.gui.name": "GUI",
  "surfaces.gui.desc": "复用共享应用服务，桌面端随后。",
  "surfaces.shipped": "已随 0.2.0 发布",
  "surfaces.dev": "开发中",
  "surfaces.strip.title": "能力一览",

  "start.title": "上手",
  "start.body":
    "从源码构建，把配置指向你的模型端点即可运行。`config check` 只校验 TOML 与环境变量引用，不联网、不调用付费模型。",
  "start.releases": "GitHub Releases",
  "start.docs": "阅读使用指南",
  "start.step1": "# 构建 workspace",
  "start.step2": "# 从无凭据示例创建配置",
  "start.step3": "# 进入默认 TUI",

  "dl.hero.eyebrow": "下载",
  "dl.hero.title": "下载 Soyo",
  "dl.hero.sub":
    "一个运行时，三个界面——CLI 与 TUI 已发布，桌面端随后。",
  "dl.hero.cta.mac": "下载 macOS 版",
  "dl.hero.cta.linux": "下载 Linux 版",
  "dl.hero.cta": "下载",
  "dl.hero.releases": "全部发行版",
  "dl.hero.meta":
    "v0.2.0 · macOS 15+ 与 Linux · MIT · 无需账号",
  "dl.plat.title": "选择你的构建",
  "dl.plat.sub":
    "v0.2.0 发行的预编译归档——每个包内含二进制、MIT 许可证、README 与记录源码及哈希的 BUILD-INFO.json。",
  "dl.plat.macArm": "macOS · Apple Silicon",
  "dl.plat.macIntel": "macOS · Intel",
  "dl.plat.linuxX64": "Linux · x86_64",
  "dl.plat.linuxArm": "Linux · ARM64",
  "dl.plat.windows": "Windows",
  "dl.plat.reqMac": "macOS 15+",
  "dl.plat.reqLinux": "glibc 2.35+",
  "dl.plat.download": "下载",
  "dl.plat.winSoon": "暂未提供",
  "dl.plat.rename":
    "0.2.0 归档沿用更名前的 saya 名称——包内二进制为 saya；更名后的发行将以 soyo-* 命名。",
  "dl.verify.title": "校验下载",
  "dl.verify.body":
    "每次发行附带 SHA256SUMS 与结构化构建证据。先核对归档哈希，再核对包内 BUILD-INFO.json 的源码与二进制记录。",
  "dl.verify.sums": "SHA256SUMS",
  "dl.verify.evidence": "release-evidence.json",
  "dl.verify.how": "发行如何验收",
  "dl.src.title": "从源码构建？",
  "dl.src.body":
    "Rust 1.99.1 / edition 2024。可把 CLI 安装到 ~/.cargo/bin，或构建整个 workspace。",
  "dl.src.clone": "# 克隆仓库",
  "dl.src.install": "# 安装 CLI 到 ~/.cargo/bin",
  "dl.src.build": "# 或构建整个 workspace",
  "dl.surf.title": "一个二进制，三个界面",
  "dl.surf.sub": "同一个归档提供全部界面。TUI 是 `soyo` 的默认入口。",
  "dl.faq.title": "常见问题",
  "dl.faq.q1": "Soyo 支持哪些平台？",
  "dl.faq.a1":
    "macOS 15+（Apple Silicon 与 Intel），以及 glibc 2.35+ 的 Linux x86_64 与 ARM64。Windows 暂不支持。",
  "dl.faq.q2": "为什么归档名是 saya？",
  "dl.faq.a2":
    "Soyo Harness 原名 Saya Harness。0.2.0 先于更名发布，归档、saya 二进制与 .saya 数据目录沿用旧名；更名后的版本将全部使用 soyo。",
  "dl.faq.q3": "macOS 提示二进制已损坏或无法识别，安全吗？",
  "dl.faq.a3":
    "ARM 构建为 ad-hoc 签名、Intel 构建未签名，均未公证。先对照 SHA256SUMS 校验哈希，再移除隔离属性——或改用源码构建。",
  "dl.faq.a3.cmd": "xattr -d com.apple.quarantine ./saya",
  "dl.faq.q4": "如何校验下载？",
  "dl.faq.a4": "下载归档与 SHA256SUMS 后执行：",
  "dl.faq.a4.cmd":
    "gh release download v0.2.0 --repo Ariakage/soyo-harness --pattern SHA256SUMS\nshasum -a 256 -c SHA256SUMS",
  "dl.faq.q5": "需要账号或订阅吗？",
  "dl.faq.a5":
    "不需要。Soyo 连接你自己的模型端点——在 config.toml 中配置 profile 并用环境变量引用凭据。`soyo config check` 可离线校验。",
  "dl.faq.q6": "桌面应用在哪里？",
  "dl.faq.a6":
    "Tauri 2 桌面端正在本仓库开发中。当前发行包含 CLI 与默认 TUI，GUI 请关注 Releases。",
  "dl.back": "返回产品导览",

  "footer.tagline": "Rust Agent Runtime",
  "footer.license": "MIT 许可证",
  "footer.formerly": "原名 Saya Harness。",
  "footer.github": "GitHub",
  "footer.releases": "发行版",
  "footer.issues": "Issues",
  "footer.docs": "文档",
  "footer.support": "支持",

  "lang.label": "语言",
};

export const dictionaries: Record<Lang, Record<MessageKey, string>> = {
  en,
  "zh-CN": zhCN,
};

/* Time-bucketed greeting pool — same idea as the desktop home greeting. */
const greetings: Record<Lang, { morning: string[]; afternoon: string[]; evening: string[]; night: string[]; anytime: string[] }> = {
  en: {
    morning: ["Good morning.", "Morning — coffee first?", "Early start."],
    afternoon: ["Good afternoon.", "Back at it.", "Afternoon."],
    evening: ["Good evening.", "Winding down?", "Evening."],
    night: ["Up late?", "Quiet hours.", "Night owl."],
    anytime: [
      "You're here!",
      "What are we building today?",
      "Ready when you are.",
      "Let's get to work.",
    ],
  },
  "zh-CN": {
    morning: ["早上好。", "起得真早。", "早安。"],
    afternoon: ["下午好。", "继续干活。", "午后。"],
    evening: ["晚上好。", "今天辛苦了。", "傍晚好。"],
    night: ["还没睡？", "夜深人静。", "夜猫子时间。"],
    anytime: ["你来了！", "今天做点什么？", "准备好了就开始。", "开工吧。"],
  },
};

export function pickGreeting(lang: Lang): string {
  const h = new Date().getHours();
  const g = greetings[lang];
  const bucket =
    h >= 5 && h < 12
      ? g.morning
      : h >= 12 && h < 18
        ? g.afternoon
        : h >= 18 && h < 23
          ? g.evening
          : g.night;
  const pool = Math.random() < 0.15 ? g.anytime : bucket;
  return pool[Math.floor(Math.random() * pool.length)];
}

interface I18nContextValue {
  lang: Lang;
  setLang: (l: Lang) => void;
  t: (key: MessageKey) => string;
}

const I18nContext = createContext<I18nContextValue | null>(null);

function detectInitialLang(): Lang {
  try {
    const saved = localStorage.getItem("soyo.lang");
    if (saved === "en" || saved === "zh-CN") return saved;
  } catch {
    /* ignore */
  }
  return navigator.language.toLowerCase().startsWith("zh") ? "zh-CN" : "en";
}

export function I18nProvider({ children }: { children: ReactNode }) {
  const [lang, setLangState] = useState<Lang>(detectInitialLang);

  const setLang = (l: Lang) => {
    setLangState(l);
    try {
      localStorage.setItem("soyo.lang", l);
    } catch {
      /* ignore */
    }
  };

  useEffect(() => {
    document.documentElement.lang = lang;
  }, [lang]);

  const value = useMemo<I18nContextValue>(
    () => ({
      lang,
      setLang,
      t: (key) => dictionaries[lang][key] ?? en[key],
    }),
    [lang],
  );

  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>;
}

export function useI18n(): I18nContextValue {
  const ctx = useContext(I18nContext);
  if (!ctx) throw new Error("useI18n outside I18nProvider");
  return ctx;
}
