export type LocaleId = "en-US" | "zh-CN" | "zh-TW";

export const DEFAULT_LOCALE: LocaleId = "en-US";

export interface LanguageOption {
  id: LocaleId;
  nativeLabel: string;
  subtitle: string;
}

export const LANGUAGE_OPTIONS: readonly LanguageOption[] = [
  {
    id: "en-US",
    nativeLabel: "English (United States)",
    subtitle: "English (United States)",
  },
  {
    id: "zh-CN",
    nativeLabel: "简体中文 (中国)",
    subtitle: "Chinese (Simplified)",
  },
  {
    id: "zh-TW",
    nativeLabel: "繁體中文 (台灣)",
    subtitle: "Chinese (Traditional)",
  },
];

export interface UiMessages {
  languageDialog: {
    title: string;
    closeAria: string;
  };
  userMenu: {
    settings: string;
    language: string;
    usage: string;
    profiles: string;
    close: string;
  };
  topbar: {
    home: string;
    newSession: string;
    showMarkdownSource: string;
    renderMarkdown: string;
    browserPreview: string;
    inspector: string;
    exportSession: string;
    browseSessions: string;
  };
  sidebar: {
    sessionsTitle: string;
    newSession: string;
    pinned: string;
    answerQuestions: string;
    noRecentSessions: string;
    untitledSession: string;
    viewAll: string;
    searchTitle: string;
    searchPlaceholder: string;
    noMatchingSessions: string;
    browseAllSessions: string;
    memoryTitle: string;
    globalStore: string;
    twoPhase: string;
    memoryDesc: string;
    openMemory: string;
    mcpTitle: string;
    workspaceBridges: string;
    noMcpActive: string;
    manageMcp: string;
    exportTitle: string;
    exportDesc: string;
    exportSession: string;
    browseToExport: string;
    customizeTitle: string;
    customizeGeneral: string;
    customizeProfiles: string;
    customizePermissions: string;
    customizeModules: string;
    customizeUsage: string;
  };
  home: {
    greeting: string;
    projectOrFolder: string;
    ideasForYou: string;
    ideaSummarize: string;
    ideaAuditMcp: string;
    ideaCustomize: string;
    configIncomplete: string;
    configFallback: string;
    historyOmitted: string;
    loadingRuntime: string;
  };
  composer: {
    placeholderApproval: string;
    placeholderActive: string;
    placeholderHome: string;
    placeholderDefault: string;
    commandsHeader: string;
    cmdModel: string;
    cmdMode: string;
    cmdPermission: string;
    cmdCompact: string;
    cmdExport: string;
    cmdSessions: string;
    cmdMemory: string;
    cmdMcp: string;
    addFilesOrPhotos: string;
    addFolder: string;
    enterPathManually: string;
    skills: string;
    connectors: string;
    addPlugins: string;
    modelProfileHeader: string;
    manageProfiles: string;
    stagePathPlaceholder: string;
    stageBtn: string;
    messagePreview: string;
    attach: string;
    webSearch: string;
    compact: string;
    pausedApproval: string;
    steer: string;
    queue: string;
    hintEnter: string;
    hintShiftEnter: string;
    hintTab: string;
    hintEsc: string;
  };
  inspector: {
    runtime: string;
    profile: string;
    model: string;
    sandbox: string;
    workspace: string;
    unset: string;
    contextWindow: string;
    contextEmpty: string;
    freeSpace: string;
    tasks: string;
    noActiveTasks: string;
    outputs: string;
    outputsAndRecent: string;
    noSavedSessions: string;
  };
  settings: {
    navGeneral: string;
    navUsage: string;
    navProfiles: string;
    navPermissions: string;
    navModules: string;
    navMcp: string;
    navMemory: string;
    navExport: string;
    titleUsage: string;
    titleProfiles: string;
    titlePermissions: string;
    titleModules: string;
    titleGeneral: string;
    usageSubtitle: string;
    tokensPerDay: string;
    today: string;
    liveSession: string;
    idle: string;
    workspaceDir: string;
    browse: string;
    apply: string;
    languageLabel: string;
    modelProfile: string;
    thinkingEffort: string;
    sandboxBackend: string;
    autoCompact: string;
    on: string;
    off: string;
    triggerPlaceholder: string;
    setTrigger: string;
    workMode: string;
    allowBypassTitle: string;
    allowBypassDesc: string;
  };
}

const MESSAGES: Record<LocaleId, UiMessages> = {
  "en-US": {
    languageDialog: {
      title: "Choose your language",
      closeAria: "Close language selector",
    },
    userMenu: {
      settings: "Settings",
      language: "Language",
      usage: "Usage analytics",
      profiles: "Model providers",
      close: "Close menu",
    },
    topbar: {
      home: "Home",
      newSession: "New session",
      showMarkdownSource: "Show markdown source",
      renderMarkdown: "Render markdown",
      browserPreview: "Browser preview",
      inspector: "Inspector",
      exportSession: "Export session…",
      browseSessions: "Browse sessions…",
    },
    sidebar: {
      sessionsTitle: "Sessions",
      newSession: "New session",
      pinned: "Pinned 1",
      answerQuestions: "Answer questions",
      noRecentSessions: "No recent sessions yet.",
      untitledSession: "Untitled session",
      viewAll: "View all",
      searchTitle: "Search",
      searchPlaceholder: "Search sessions…",
      noMatchingSessions: "No matching sessions.",
      browseAllSessions: "Browse all sessions…",
      memoryTitle: "Memory",
      globalStore: "Global store",
      twoPhase: "two-phase",
      memoryDesc:
        "Global memory entries persist across sessions and are injected into the system context.",
      openMemory: "Open memory",
      mcpTitle: "MCP & Creds",
      workspaceBridges: "Workspace bridges",
      noMcpActive: "No MCP servers active in current run.",
      manageMcp: "Manage servers & credentials",
      exportTitle: "Export",
      exportDesc:
        "Export the current session as a Markdown handoff or JSON v1 archive.",
      exportSession: "Export session…",
      browseToExport: "Browse sessions to export",
      customizeTitle: "Customize",
      customizeGeneral: "General & preferences",
      customizeProfiles: "Model providers & profiles",
      customizePermissions: "Permissions & sandbox",
      customizeModules: "Modules & skills",
      customizeUsage: "Usage & tokens",
    },
    home: {
      greeting: "Good morning, ariakage.",
      projectOrFolder: "Project or folder",
      ideasForYou: "Ideas for you",
      ideaSummarize: "Summarize today's sessions into a handoff note",
      ideaAuditMcp: "Audit MCP servers and credential scopes",
      ideaCustomize: "Customize Soyo for this workspace",
      configIncomplete: "Configuration incomplete",
      configFallback: "Select or configure a profile first.",
      historyOmitted:
        "Earlier history omitted to stay within desktop restore window; full history remains on disk.",
      loadingRuntime: "Loading runtime state...",
    },
    composer: {
      placeholderApproval: "Steer the run, or reply to approval above…",
      placeholderActive: "Steer the run, or press Esc / Stop…",
      placeholderHome: "How can I help you today?",
      placeholderDefault: "Type a message or / for commands…",
      commandsHeader: "Commands",
      cmdModel: "switch model profile",
      cmdMode: "change work mode — plan · build · code · work",
      cmdPermission: "auto · bypass permissions · manual approval",
      cmdCompact: "summarize context to free window space",
      cmdExport: "export session to markdown handoff",
      cmdSessions: "browse and restore sessions",
      cmdMemory: "view and edit workspace memory",
      cmdMcp: "manage MCP servers and credentials",
      addFilesOrPhotos: "Add files or photos",
      addFolder: "Add folder",
      enterPathManually: "Enter path manually",
      skills: "Skills",
      connectors: "Connectors",
      addPlugins: "Add plugins",
      modelProfileHeader: "Model profile",
      manageProfiles: "Manage profiles…",
      stagePathPlaceholder: "Local file path inside workspace or readable file...",
      stageBtn: "Stage",
      messagePreview: "Message preview",
      attach: "Attach",
      webSearch: "Web search",
      compact: "Compact",
      pausedApproval: "paused · approval required",
      steer: "Steer",
      queue: "Queue",
      hintEnter: "Enter send",
      hintShiftEnter: "Shift+Enter newline",
      hintTab: "Empty Tab cycle mode",
      hintEsc: "Esc cancel",
    },
    inspector: {
      runtime: "Runtime",
      profile: "Profile",
      model: "Model",
      sandbox: "Sandbox",
      workspace: "Workspace",
      unset: "unset",
      contextWindow: "Context window",
      contextEmpty: "Context breakdown updates on model turns.",
      freeSpace: "Free space",
      tasks: "Tasks",
      noActiveTasks: "No active tasks.",
      outputs: "Outputs",
      outputsAndRecent: "Outputs & Recent",
      noSavedSessions: "No saved sessions.",
    },
    settings: {
      navGeneral: "General",
      navUsage: "Usage",
      navProfiles: "Model & Profiles",
      navPermissions: "Permissions",
      navModules: "Modules",
      navMcp: "MCP & Credentials",
      navMemory: "Memory",
      navExport: "Import & export",
      titleUsage: "Usage & Tokens",
      titleProfiles: "Model & Profiles",
      titlePermissions: "Permissions & Modes",
      titleModules: "Modules & Extensions",
      titleGeneral: "Runtime & Modules",
      usageSubtitle: "Token usage across CLI, TUI and Desktop on this device",
      tokensPerDay: "Tokens per day",
      today: "Today",
      liveSession: "live session",
      idle: "idle",
      workspaceDir: "Workspace Directory",
      browse: "Browse…",
      apply: "Apply",
      languageLabel: "Language",
      modelProfile: "Model Profile",
      thinkingEffort: "Thinking Effort",
      sandboxBackend: "Sandbox Backend",
      autoCompact: "Auto-Compact",
      on: "On",
      off: "Off",
      triggerPlaceholder: "Optional exact token trigger (blank = % threshold)",
      setTrigger: "Set Trigger",
      workMode: "Work mode",
      allowBypassTitle: "Allow bypass permissions mode",
      allowBypassDesc:
        "Bypass skips path checks, model review, and human confirmation. PLAN, explicit deny and hard backend limits still apply.",
    },
  },
  "zh-CN": {
    languageDialog: {
      title: "选择语言",
      closeAria: "关闭语言选择",
    },
    userMenu: {
      settings: "设置",
      language: "语言",
      usage: "用量分析",
      profiles: "模型提供商",
      close: "关闭菜单",
    },
    topbar: {
      home: "主页",
      newSession: "新建会话",
      showMarkdownSource: "显示 Markdown 源码",
      renderMarkdown: "渲染 Markdown",
      browserPreview: "浏览器预览",
      inspector: "检查器",
      exportSession: "导出会话…",
      browseSessions: "浏览会话…",
    },
    sidebar: {
      sessionsTitle: "会话",
      newSession: "新建会话",
      pinned: "置顶 1",
      answerQuestions: "回答问题",
      noRecentSessions: "暂无最近会话。",
      untitledSession: "未命名会话",
      viewAll: "查看全部",
      searchTitle: "搜索",
      searchPlaceholder: "搜索会话…",
      noMatchingSessions: "没有匹配的会话。",
      browseAllSessions: "浏览全部会话…",
      memoryTitle: "记忆",
      globalStore: "全局存储",
      twoPhase: "两阶段",
      memoryDesc: "全局记忆条目会在会话间持久保存，并注入系统上下文。",
      openMemory: "打开记忆管理",
      mcpTitle: "MCP 与凭据",
      workspaceBridges: "工作区桥接",
      noMcpActive: "当前运行没有激活的 MCP 服务器。",
      manageMcp: "管理服务器与凭据",
      exportTitle: "导出",
      exportDesc: "将当前会话导出为 Markdown 交接文档或 JSON v1 归档。",
      exportSession: "导出会话…",
      browseToExport: "选择要导出的会话",
      customizeTitle: "自定义",
      customizeGeneral: "常规与偏好设置",
      customizeProfiles: "模型提供商与配置",
      customizePermissions: "权限与沙箱",
      customizeModules: "模块与技能",
      customizeUsage: "用量与 Token",
    },
    home: {
      greeting: "早上好，ariakage。",
      projectOrFolder: "项目或文件夹",
      ideasForYou: "为你推荐",
      ideaSummarize: "将今天的会话整理为交接笔记",
      ideaAuditMcp: "审计 MCP 服务器与凭据作用域",
      ideaCustomize: "为此工作区自定义 Soyo",
      configIncomplete: "配置不完整",
      configFallback: "请先选择或配置一个模型配置文件。",
      historyOmitted: "较早的历史记录已省略以控制恢复窗口大小；完整记录仍保存在磁盘中。",
      loadingRuntime: "正在加载运行时状态…",
    },
    composer: {
      placeholderApproval: "引导当前运行，或在上方回复审批请求…",
      placeholderActive: "继续输入以引导运行，或按 Esc / 停止…",
      placeholderHome: "今天我可以帮你做些什么？",
      placeholderDefault: "输入消息，或输入 / 查看命令…",
      commandsHeader: "命令",
      cmdModel: "切换模型配置",
      cmdMode: "切换工作模式 — plan · build · code · work",
      cmdPermission: "auto · bypass permissions · 手动审批",
      cmdCompact: "压缩上下文以释放窗口空间",
      cmdExport: "导出会话为 Markdown 交接文档",
      cmdSessions: "浏览并恢复历史会话",
      cmdMemory: "查看并编辑工作区记忆",
      cmdMcp: "管理 MCP 服务器与凭据",
      addFilesOrPhotos: "添加文件或图片",
      addFolder: "添加文件夹",
      enterPathManually: "手动输入路径",
      skills: "技能",
      connectors: "连接器",
      addPlugins: "添加插件",
      modelProfileHeader: "模型配置",
      manageProfiles: "管理配置文件…",
      stagePathPlaceholder: "输入工作区内的本地路径或可读文件路径…",
      stageBtn: "暂存",
      messagePreview: "消息预览",
      attach: "附件",
      webSearch: "联网搜索",
      compact: "压缩",
      pausedApproval: "已暂停 · 等待审批",
      steer: "引导",
      queue: "排队",
      hintEnter: "Enter 发送",
      hintShiftEnter: "Shift+Enter 换行",
      hintTab: "空框 Tab 切模式",
      hintEsc: "Esc 取消",
    },
    inspector: {
      runtime: "运行时",
      profile: "配置",
      model: "模型",
      sandbox: "沙箱",
      workspace: "工作区",
      unset: "未设置",
      contextWindow: "上下文窗口",
      contextEmpty: "模型轮次执行后将更新上下文明细。",
      freeSpace: "剩余空间",
      tasks: "任务",
      noActiveTasks: "暂无进行中的任务。",
      outputs: "输出文件",
      outputsAndRecent: "输出与最近会话",
      noSavedSessions: "暂无保存的会话。",
    },
    settings: {
      navGeneral: "常规",
      navUsage: "用量",
      navProfiles: "模型与配置",
      navPermissions: "权限",
      navModules: "模块",
      navMcp: "MCP 与凭据",
      navMemory: "记忆",
      navExport: "导入与导出",
      titleUsage: "用量与 Token",
      titleProfiles: "模型与配置",
      titlePermissions: "权限与模式",
      titleModules: "模块与扩展",
      titleGeneral: "运行时与模块",
      usageSubtitle: "本设备 CLI、TUI 与 Desktop 的 Token 用量统计",
      tokensPerDay: "每日 Token 用量",
      today: "今天",
      liveSession: "活动会话",
      idle: "空闲",
      workspaceDir: "工作区目录",
      browse: "浏览…",
      apply: "应用",
      languageLabel: "界面语言",
      modelProfile: "模型配置",
      thinkingEffort: "思考强度",
      sandboxBackend: "沙箱后端",
      autoCompact: "自动压缩",
      on: "开启",
      off: "关闭",
      triggerPlaceholder: "可选精确 Token 触发阈值（留空则按百分比）",
      setTrigger: "设置阈值",
      workMode: "工作模式",
      allowBypassTitle: "允许跳过权限确认（bypass）",
      allowBypassDesc:
        "开启后将跳过路径检查、模型复核与人工确认；PLAN 只读限制、显式拒绝规则与底层硬隔离约束仍生效。",
    },
  },
  "zh-TW": {
    languageDialog: {
      title: "選擇語言",
      closeAria: "關閉語言選擇",
    },
    userMenu: {
      settings: "設定",
      language: "語言",
      usage: "用量分析",
      profiles: "模型供應商",
      close: "關閉選單",
    },
    topbar: {
      home: "首頁",
      newSession: "新增工作階段",
      showMarkdownSource: "顯示 Markdown 原始碼",
      renderMarkdown: "算繪 Markdown",
      browserPreview: "瀏覽器預覽",
      inspector: "檢查器",
      exportSession: "匯出工作階段…",
      browseSessions: "瀏覽工作階段…",
    },
    sidebar: {
      sessionsTitle: "工作階段",
      newSession: "新增工作階段",
      pinned: "釘選 1",
      answerQuestions: "回答問題",
      noRecentSessions: "尚無最近的工作階段。",
      untitledSession: "未命名工作階段",
      viewAll: "檢視全部",
      searchTitle: "搜尋",
      searchPlaceholder: "搜尋工作階段…",
      noMatchingSessions: "找不到相符的工作階段。",
      browseAllSessions: "瀏覽全部工作階段…",
      memoryTitle: "記憶",
      globalStore: "全域儲存區",
      twoPhase: "兩階段",
      memoryDesc: "全域記憶項目會在工作階段之間持續保存，並注入系統內容。",
      openMemory: "開啟記憶管理",
      mcpTitle: "MCP 與憑證",
      workspaceBridges: "工作區橋接器",
      noMcpActive: "目前執行中沒有啟用的 MCP 伺服器。",
      manageMcp: "管理伺服器與憑證",
      exportTitle: "匯出",
      exportDesc: "將目前工作階段匯出為 Markdown 交接筆記或 JSON v1 封存檔。",
      exportSession: "匯出工作階段…",
      browseToExport: "選擇要匯出的工作階段",
      customizeTitle: "自訂",
      customizeGeneral: "一般與偏好設定",
      customizeProfiles: "模型供應商與設定檔",
      customizePermissions: "權限與沙箱",
      customizeModules: "模組與技能",
      customizeUsage: "用量與 Token",
    },
    home: {
      greeting: "早安，ariakage。",
      projectOrFolder: "專案或資料夾",
      ideasForYou: "為你推薦",
      ideaSummarize: "將今天的工作階段整理為交接筆記",
      ideaAuditMcp: "稽核 MCP 伺服器與憑證範圍",
      ideaCustomize: "為此工作區自訂 Soyo",
      configIncomplete: "設定不完整",
      configFallback: "請先選擇或設定一個模型設定檔。",
      historyOmitted: "較早的歷史紀錄已略過以維持還原效能；完整紀錄仍保留在磁碟上。",
      loadingRuntime: "正在載入執行階段狀態…",
    },
    composer: {
      placeholderApproval: "引導目前執行，或在上方回覆核准請求…",
      placeholderActive: "繼續輸入以引導執行，或按 Esc / 停止…",
      placeholderHome: "今天需要什麼協助？",
      placeholderDefault: "輸入訊息，或輸入 / 檢視指令…",
      commandsHeader: "指令",
      cmdModel: "切換模型設定檔",
      cmdMode: "切換工作模式 — plan · build · code · work",
      cmdPermission: "auto · bypass permissions · 人工核准",
      cmdCompact: "摘要壓縮內容以釋放視窗空間",
      cmdExport: "將工作階段匯出為 Markdown 交接筆記",
      cmdSessions: "瀏覽並還原歷史工作階段",
      cmdMemory: "檢視與編輯工作區記憶",
      cmdMcp: "管理 MCP 伺服器與憑證",
      addFilesOrPhotos: "新增檔案或圖片",
      addFolder: "新增資料夾",
      enterPathManually: "手動輸入路徑",
      skills: "技能",
      connectors: "連接器",
      addPlugins: "新增外掛程式",
      modelProfileHeader: "模型設定檔",
      manageProfiles: "管理設定檔…",
      stagePathPlaceholder: "輸入工作區內的本機路徑或可讀檔案路徑…",
      stageBtn: "暫存",
      messagePreview: "訊息預覽",
      attach: "附件",
      webSearch: "網路搜尋",
      compact: "壓縮",
      pausedApproval: "已暫停 · 等待核准",
      steer: "引導",
      queue: "排程",
      hintEnter: "Enter 傳送",
      hintShiftEnter: "Shift+Enter 換行",
      hintTab: "空欄 Tab 切換模式",
      hintEsc: "Esc 取消",
    },
    inspector: {
      runtime: "執行階段",
      profile: "設定檔",
      model: "模型",
      sandbox: "沙箱",
      workspace: "工作區",
      unset: "未設定",
      contextWindow: "內容視窗",
      contextEmpty: "模型回合執行後將更新內容細項。",
      freeSpace: "剩餘空間",
      tasks: "任務",
      noActiveTasks: "目前無進行中任務。",
      outputs: "輸出檔案",
      outputsAndRecent: "輸出與最近工作階段",
      noSavedSessions: "尚無儲存的工作階段。",
    },
    settings: {
      navGeneral: "一般",
      navUsage: "用量",
      navProfiles: "模型與設定檔",
      navPermissions: "權限",
      navModules: "模組",
      navMcp: "MCP 與憑證",
      navMemory: "記憶",
      navExport: "匯入與匯出",
      titleUsage: "用量與 Token",
      titleProfiles: "模型與設定檔",
      titlePermissions: "權限與模式",
      titleModules: "模組與擴充功能",
      titleGeneral: "執行階段與模組",
      usageSubtitle: "此裝置上 CLI、TUI 與 Desktop 的 Token 用量統計",
      tokensPerDay: "每日 Token 用量",
      today: "今天",
      liveSession: "作用中工作階段",
      idle: "閒置",
      workspaceDir: "工作區目錄",
      browse: "瀏覽…",
      apply: "套用",
      languageLabel: "介面語言",
      modelProfile: "模型設定檔",
      thinkingEffort: "思考強度",
      sandboxBackend: "沙箱後端",
      autoCompact: "自動壓縮",
      on: "開啟",
      off: "關閉",
      triggerPlaceholder: "選填精確 Token 觸發門檻（留白則採百分比）",
      setTrigger: "設定門檻",
      workMode: "工作模式",
      allowBypassTitle: "允許略過權限確認（bypass）",
      allowBypassDesc:
        "啟用後將略過路徑檢查、模型複核與人工確認；PLAN 唯讀限制、明確拒絕規則與底層硬隔離約束依然有效。",
    },
  },
};

export function getMessages(locale: LocaleId): UiMessages {
  return MESSAGES[locale] ?? MESSAGES[DEFAULT_LOCALE];
}
