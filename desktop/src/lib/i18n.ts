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

export type EffortLevelId = "low" | "medium" | "high" | "max" | "ultracode";

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
    savedStatus: string;
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
    mcpActiveCount: (n: number) => string;
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
    greetings: {
      morning: string[];
      afternoon: string[];
      evening: string[];
      night: string[];
      anytime: string[];
    };
    fallbackName: string;
    projectOrFolder: string;
    ideasForYou: string;
    ideaSummarize: string;
    ideaAuditMcp: string;
    ideaCustomize: string;
    browserNew: string;
    configIncomplete: string;
    configFallback: string;
    historyOmitted: string;
    loadingRuntime: string;
    streaming: string;
    copied: string;
    retryPrompt: string;
    editMessage: string;
    copyMessage: string;
    exportShare: string;
    branchUnsupported: string;
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
    queuedPrompt: string;
    compactionQueued: string;
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
    activeSkills: (n: number) => string;
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
    tokensUnit: string;
    inputLabel: string;
    outputLabel: string;
    cacheReadLabel: string;
    inputLower: string;
    outputLower: string;
    cacheReadLower: string;
    totalLower: string;
    tokensPerDay: string;
    today: string;
    windowLabel: (r: string) => string;
    liveSession: string;
    idle: string;
    purposeExecution: string;
    purposeReview: string;
    purposeCompaction: string;
    runsLabel: string;
    usageLoading: string;
    usageEmpty: string;
    usageEstimated: string;
    workspaceDir: string;
    browse: string;
    apply: string;
    languageLabel: string;
    profileLabel: string;
    avatarLabel: string;
    changeAvatar: string;
    resetAvatar: string;
    nicknameLabel: string;
    nicknameHint: string;
    globalPromptLabel: string;
    globalPromptHint: (path: string) => string;
    savePrompt: string;
    promptSaved: string;
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
  setup: {
    welcome: string;
    welcomeSubtitle: string;
    skipLabel: string;
    stepOf: (n: number, total: number) => string;
    stepLanguage: string;
    stepWorkspace: string;
    stepModel: string;
    stepPermissions: string;
    workspaceHint: string;
    browse: string;
    presets: {
      openai: string;
      anthropic: string;
      compatible: string;
      local: string;
    };
    profileName: string;
    contextWindow: string;
    baseUrl: string;
    modelName: string;
    apiKeyLabel: string;
    pasteKeyLabel: string;
    useEnvVarLabel: string;
    envVarNameLabel: string;
    apiKeyHint: string;
    noKeyNeeded: string;
    permAuto: string;
    permAutoHint: string;
    permManual: string;
    permManualHint: string;
    permBypass: string;
    permBypassHint: string;
    backLabel: string;
    continueLabel: string;
    finishLabel: string;
    applyingLabel: string;
  };
  sessionBrowser: {
    title: string;
    configChangedPrefix: string;
    configChangedSuffix: string;
    acceptAndResume: string;
    cancel: string;
    loading: string;
    emptyPage: string;
    active: string;
    current: string;
    resume: string;
    detailsHeader: string;
    project: string;
    profile: string;
    mode: string;
    status: string;
    selectToInspect: string;
    pageLabel: (p: number) => string;
    prev: string;
    next: string;
  };
  mcpDialog: {
    title: string;
    configuredServers: string;
    noServers: string;
    enabled: string;
    disabled: string;
    storedCredentials: string;
    noCredentials: string;
    revoked: string;
    revoke: string;
    importTitle: string;
    importDesc: string;
    serverPlaceholder: string;
    envPlaceholder: string;
    scopesPlaceholder: string;
    approveScopesLabel: string;
    importBtn: string;
  };
  memoryDialog: {
    title: string;
    queryBtn: string;
    prepareTitle: string;
    preparePlaceholder: string;
    prepareBtn: string;
    discardBtn: string;
    confirmBtn: string;
    appliedMsg: (opId: string) => string;
  };
  exportDialog: {
    title: string;
    published: string;
    outputPathLabel: (ws: string) => string;
    browse: string;
    format: string;
    scope: string;
    scopeTranscript: string;
    scopeFull: string;
    includeTools: string;
    close: string;
    exporting: string;
    publishBtn: string;
  };
  modulesPanel: {
    runtimeModules: string;
    noModules: string;
    enabled: string;
    disabled: string;
    extensionsHeader: (n: number) => string;
    noExtensions: string;
    jobsHeader: (n: number) => string;
    noJobs: string;
    stateLabel: string;
    cancel: string;
  };
  approval: {
    title: string;
    tool: string;
    action: string;
    call: string;
    workspace: string;
    pressPrefix: string;
    pressMid: string;
    pressSuffix: string;
    denyBtn: string;
    allowBtn: string;
  };
  clarification: {
    title: string;
    suggestedOptions: string;
    customAnswer: string;
    placeholder: string;
    submitBtn: string;
  };
  effort: {
    pillPrefix: string;
    header: string;
    faster: string;
    smarter: string;
    helpTip: string;
    ultracodeTitle: string;
    ultracodeDesc: string;
    levels: Record<EffortLevelId, { name: string; desc: string; warning?: string }>;
  };
  cards: {
    thinking: string;
    thinkingActive: string;
    thought: string;
    reasoning: string;
    autoCollapsed: string;
    truncated: string;
    empty: string;
    reasoningUnavailable: string;
    requested: string;
    running: string;
    error: string;
    done: string;
    tool: string;
    subagent: string;
    reviewPrefix: string;
    hunkCount: (n: number) => string;
    collapseDiff: string;
    expandDiff: (n: number) => string;
    requestHeader: string;
    outputHeader: string;
    attachmentsHeader: (n: number) => string;
  };
  updater: {
    available: (v: string) => string;
    current: (v: string) => string;
    downloading: string;
    downloadedInstalling: string;
    installedRestart: string;
    installingBtn: string;
    installRestartBtn: string;
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
      savedStatus: "saved",
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
      mcpActiveCount: (n) =>
        `${n} MCP server${n > 1 ? "s" : ""} registered with host redaction.`,
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
      greetings: {
        morning: [
          "Good morning, {name}.",
          "Morning, {name}.",
          "Rise and ship.",
        ],
        afternoon: [
          "Good afternoon, {name}.",
          "Afternoon, {name}.",
          "Hope the day's going well.",
        ],
        evening: [
          "Good evening, {name}.",
          "Evening, {name}.",
          "Winding down or ramping up?",
        ],
        night: [
          "Up late, {name}?",
          "Burning the midnight oil?",
          "Quiet hours. Good focus time.",
        ],
        anytime: [
          "You're here!",
          "Welcome back, {name}.",
          "Welcome, {name}.",
          "Good to see you, {name}.",
          "Hey, {name}.",
          "Let's build something.",
          "What are we making today?",
          "What would you like to work on in this project?",
          "What's on your mind today?",
          "What's on the agenda today?",
          "What's up next, {name}?",
          "Where should we begin?",
          "{name}, what can I do for you?",
          "Ready to dive in?",
          "Ready when you are.",
          "How can I help, {name}?",
          "How can I help you today?",
          "Yay, you're here!",
          "Hello hello!",
          "Oh! It's you.",
          "Psst… over here.",
          "You + me + a blinking cursor.",
          "Let's GO!",
        ],
      },
      fallbackName: "friend",
      projectOrFolder: "Project or folder",
      ideasForYou: "Ideas for you",
      ideaSummarize: "Summarize today's sessions into a handoff note",
      ideaAuditMcp: "Audit MCP servers and credential scopes",
      ideaCustomize: "Customize Soyo for this workspace",
      browserNew: "+ New",
      configIncomplete: "Configuration incomplete",
      configFallback: "Select or configure a profile first.",
      historyOmitted:
        "Earlier history omitted to stay within desktop restore window; full history remains on disk.",
      loadingRuntime: "Loading runtime state...",
      streaming: "streaming",
      copied: "Copied",
      retryPrompt: "Retry prompt",
      editMessage: "Edit message in composer",
      copyMessage: "Copy message",
      exportShare: "Export / share",
      branchUnsupported: "Branch session from this message (not yet supported by the runtime)",
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
      queuedPrompt: "queued",
      compactionQueued: "compact queued",
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
      activeSkills: (n) => `${n} active skill(s)`,
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
      usageSubtitle: "Token usage across sessions on this device",
      tokensUnit: "tokens",
      inputLabel: "Input",
      outputLabel: "Output",
      cacheReadLabel: "Cache read",
      inputLower: "input",
      outputLower: "output",
      cacheReadLower: "cache read",
      totalLower: "total",
      tokensPerDay: "Tokens per day",
      today: "Today",
      windowLabel: (r) => `Window: ${r}`,
      liveSession: "live session",
      idle: "idle",
      purposeExecution: "Execution",
      purposeReview: "Review",
      purposeCompaction: "Compaction",
      runsLabel: "runs",
      usageLoading: "Loading usage…",
      usageEmpty: "No recorded usage in this window",
      usageEstimated: "Includes estimated values",
      workspaceDir: "Workspace Directory",
      browse: "Browse…",
      apply: "Apply",
      languageLabel: "Language",
      profileLabel: "Profile",
      avatarLabel: "Avatar",
      changeAvatar: "Change…",
      resetAvatar: "Reset",
      nicknameLabel: "Nickname",
      nicknameHint: "Shown in greetings; leave empty to use your account name.",
      globalPromptLabel: "Global instructions",
      globalPromptHint: (path: string) =>
        `Applies to every session in all workspaces. Saved to ${path}, not your project files.`,
      savePrompt: "Save",
      promptSaved: "Saved",
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
    setup: {
      welcome: "Welcome to Soyo",
      welcomeSubtitle: "Set up the essentials to get started",
      skipLabel: "Set up later",
      stepOf: (n, total) => `Step ${n} of ${total}`,
      stepLanguage: "Choose your language",
      stepWorkspace: "Pick a workspace directory",
      stepModel: "Connect a model provider",
      stepPermissions: "Choose a permission mode",
      workspaceHint: "Sessions and artifacts live under this directory.",
      browse: "Browse…",
      presets: {
        openai: "OpenAI",
        anthropic: "Anthropic",
        compatible: "Compatible",
        local: "Local",
      },
      profileName: "Profile name",
      contextWindow: "Context window",
      baseUrl: "API base URL",
      modelName: "Model",
      apiKeyLabel: "API key",
      pasteKeyLabel: "Paste key",
      useEnvVarLabel: "Use existing env var",
      envVarNameLabel: "Variable name",
      apiKeyHint:
        "Stored in the private helper env file (~/.soyo/env); config.toml only keeps the variable name.",
      noKeyNeeded:
        "This endpoint needs no API key — requests are sent without authentication.",
      permAuto: "Auto",
      permAutoHint: "Recommended — built-in checks still apply",
      permManual: "Manual approval",
      permManualHint: "Approve every tool call before it runs",
      permBypass: "Bypass permissions",
      permBypassHint: "No prompts — only hard backend limits apply",
      backLabel: "Back",
      continueLabel: "Continue",
      finishLabel: "Finish setup",
      applyingLabel: "Applying…",
    },
    sessionBrowser: {
      title: "Sessions & Project History",
      configChangedPrefix: "Profile settings changed since session",
      configChangedSuffix: "was saved. Confirm to resume with current configuration?",
      acceptAndResume: "Accept Config Change & Resume",
      cancel: "Cancel",
      loading: "Loading sessions...",
      emptyPage: "No sessions found on this page.",
      active: "active",
      current: "Current",
      resume: "Resume",
      detailsHeader: "Session Details",
      project: "Project",
      profile: "Profile",
      mode: "Mode",
      status: "Status",
      selectToInspect: "Select a session to inspect.",
      pageLabel: (p) => `Page ${p}`,
      prev: "Prev",
      next: "Next",
    },
    mcpDialog: {
      title: "MCP Servers & Credentials (Redacted Metadata Only)",
      configuredServers: "Configured MCP Servers",
      noServers: "No MCP servers configured.",
      enabled: "Enabled",
      disabled: "Disabled",
      storedCredentials: "Stored Credential References",
      noCredentials: "No stored host credential references.",
      revoked: "revoked",
      revoke: "Revoke",
      importTitle: "Import Bearer Secret from Environment Variable",
      importDesc:
        "Plaintext tokens stay in host memory and private storage; enter only the environment variable name.",
      serverPlaceholder: "HTTP MCP server name",
      envPlaceholder: "Env var name (e.g. MCP_TOKEN)",
      scopesPlaceholder: "Optional comma-separated scopes",
      approveScopesLabel: "Explicitly approve endpoint & scopes",
      importBtn: "Import Reference",
    },
    memoryDialog: {
      title: "Global Memory Administration (Two-Phase Review)",
      queryBtn: "Query",
      prepareTitle: "Prepare Memory Mutation / Export",
      preparePlaceholder:
        "e.g. forget <fact_id> | correct <fact_id> <text> | export <path>",
      prepareBtn: "Prepare Review",
      discardBtn: "Discard",
      confirmBtn: "Confirm Apply",
      appliedMsg: (opId) => `Applied memory operation ${opId}`,
    },
    exportDialog: {
      title: "Export Session Transcript",
      published: "Export published",
      outputPathLabel: (ws) => `Output File Path (relative to ${ws} or absolute)`,
      browse: "Browse…",
      format: "Format",
      scope: "Scope",
      scopeTranscript: "Transcript",
      scopeFull: "Full (with events/metrics)",
      includeTools: "Include tool arguments and result bodies",
      close: "Close",
      exporting: "Exporting...",
      publishBtn: "Publish Export",
    },
    modulesPanel: {
      runtimeModules: "Runtime Modules",
      noModules: "No module status available.",
      enabled: "Enabled",
      disabled: "Disabled",
      extensionsHeader: (n) => `Extensions & Project Discovery (${n})`,
      noExtensions: "No discovered rules, skills, or plugins in current workspace.",
      jobsHeader: (n) => `Background Jobs (${n})`,
      noJobs: "No background jobs recorded in active session.",
      stateLabel: "state",
      cancel: "Cancel",
    },
    approval: {
      title: "Approval required",
      tool: "Tool",
      action: "Action",
      call: "Call",
      workspace: "Workspace",
      pressPrefix: "Press",
      pressMid: "to allow or",
      pressSuffix: "to deny",
      denyBtn: "Deny (N)",
      allowBtn: "Allow (Y)",
    },
    clarification: {
      title: "Clarification Needed",
      suggestedOptions: "Suggested Options",
      customAnswer: "Custom Answer",
      placeholder: "Type your response...",
      submitBtn: "Submit",
    },
    effort: {
      pillPrefix: "Effort",
      header: "Effort",
      faster: "Faster",
      smarter: "Smarter",
      helpTip:
        "Faster on the left, smarter on the right. Choose Ultracode to add dynamic workflows.",
      ultracodeTitle: "Ultracode",
      ultracodeDesc: "Standing dynamic-workflow orchestration for this session.",
      levels: {
        low: {
          name: "Low",
          desc: "Quick answers with minimal deliberation.",
        },
        medium: {
          name: "Medium",
          desc: "Balanced speed and reasoning for everyday tasks.",
        },
        high: {
          name: "High",
          desc: "Deeper reasoning for complex problems.",
        },
        max: {
          name: "Max",
          desc: "Maximum reasoning. Uses your limits faster.",
          warning:
            "Model thinks longer with Max effort and uses your limits faster than High.",
        },
        ultracode: {
          name: "Ultracode",
          desc: "Highest effort, plus dynamic workflows for this session.",
        },
      },
    },
    cards: {
      thinking: "Thinking",
      thinkingActive: "Thinking…",
      thought: "Thought",
      reasoning: "reasoning",
      autoCollapsed: "auto-collapsed",
      truncated: "truncated",
      empty: "(empty)",
      reasoningUnavailable: "Reasoning trace unavailable or redacted.",
      requested: "requested",
      running: "running",
      error: "error",
      done: "done",
      tool: "tool",
      subagent: "subagent",
      reviewPrefix: "Review:",
      hunkCount: (n) => `${n} ${n === 1 ? "hunk" : "hunks"}`,
      collapseDiff: "Collapse diff preview ‹",
      expandDiff: (n) => `${n} lines hidden · expand ›`,
      requestHeader: "Request",
      outputHeader: "Output",
      attachmentsHeader: (n) => `Attachments (${n})`,
    },
    updater: {
      available: (v) => `Update available: v${v}`,
      current: (v) => `(current v${v})`,
      downloading: "Downloading...",
      downloadedInstalling: "Downloaded; installing...",
      installedRestart: "Update installed — restart Soyo to finish applying.",
      installingBtn: "Installing...",
      installRestartBtn: "Install & Restart",
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
      savedStatus: "已保存",
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
      mcpActiveCount: (n) => `已注册 ${n} 个 MCP 服务器（宿主脱敏保护）。`,
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
      greetings: {
        morning: [
          "早上好，{name}。",
          "早安。",
          "新的一天，开始吧。",
        ],
        afternoon: [
          "下午好，{name}。",
          "午安。",
          "下午时光，慢慢来。",
        ],
        evening: [
          "晚上好，{name}。",
          "晚间好。",
          "收工还是继续？",
        ],
        night: [
          "夜深了，{name}。",
          "还在忙？",
          "深夜适合专注。",
        ],
        anytime: [
          "你来了！",
          "欢迎回来，{name}。",
          "欢迎，{name}。",
          "很高兴见到你，{name}。",
          "嗨，{name}。",
          "开始构建吧。",
          "今天做点什么？",
          "这个项目里想做点什么？",
          "今天在想什么？",
          "今天的议程是什么？",
          "接下来做什么，{name}？",
          "我们从哪里开始？",
          "准备好开始了？",
          "随时待命。",
          "随时可以开始。",
          "有什么可以帮你，{name}？",
          "今天有什么可以帮你？",
          "{name}，我能帮你做些什么？",
          "你来啦！",
          "嘿嘿，是你呀。",
          "等你好久啦。",
          "开工啦开工啦。",
          "今天也要加油喵。",
          "Let's GO!",
          "出发喽！",
          "加入游戏。",
        ],
      },
      fallbackName: "朋友",
      projectOrFolder: "项目或文件夹",
      ideasForYou: "为你推荐",
      ideaSummarize: "将今天的会话整理为交接笔记",
      ideaAuditMcp: "审计 MCP 服务器与凭据作用域",
      ideaCustomize: "为此工作区自定义 Soyo",
      browserNew: "+ 新建",
      configIncomplete: "配置不完整",
      configFallback: "请先选择或配置一个模型配置文件。",
      historyOmitted: "较早的历史记录已省略以控制恢复窗口大小；完整记录仍保存在磁盘中。",
      loadingRuntime: "正在加载运行时状态…",
      streaming: "流式输出中",
      copied: "已复制",
      retryPrompt: "重试提示词",
      editMessage: "在输入框中编辑",
      copyMessage: "复制消息",
      exportShare: "导出 / 分享",
      branchUnsupported: "从此消息创建会话分支（运行时暂不支持）",
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
      queuedPrompt: "已排队",
      compactionQueued: "压缩已排队",
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
      activeSkills: (n) => `${n} 个激活的技能`,
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
      usageSubtitle: "本设备所有会话的 Token 用量统计",
      tokensUnit: "tokens",
      inputLabel: "输入",
      outputLabel: "输出",
      cacheReadLabel: "缓存读取",
      inputLower: "输入",
      outputLower: "输出",
      cacheReadLower: "缓存读取",
      totalLower: "总计",
      tokensPerDay: "每日 Token 用量",
      today: "今天",
      windowLabel: (r) => `窗口范围：${r}`,
      liveSession: "活动会话",
      idle: "空闲",
      purposeExecution: "执行",
      purposeReview: "审查",
      purposeCompaction: "压缩",
      runsLabel: "次运行",
      usageLoading: "正在加载用量…",
      usageEmpty: "此时间范围内暂无用量记录",
      usageEstimated: "包含估算值",
      workspaceDir: "工作区目录",
      browse: "浏览…",
      apply: "应用",
      languageLabel: "界面语言",
      profileLabel: "个人资料",
      avatarLabel: "头像",
      changeAvatar: "更换…",
      resetAvatar: "重置",
      nicknameLabel: "昵称",
      nicknameHint: "显示在问候语中；留空则使用系统账户名。",
      globalPromptLabel: "全局提示词",
      globalPromptHint: (path: string) =>
        `对所有工作区的会话生效，保存到 ${path}，不修改项目内文件。`,
      savePrompt: "保存",
      promptSaved: "已保存",
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
    setup: {
      welcome: "欢迎使用 Soyo",
      welcomeSubtitle: "完成基础设置后即可开始使用",
      skipLabel: "稍后设置",
      stepOf: (n, total) => `第 ${n} / ${total} 步`,
      stepLanguage: "选择界面语言",
      stepWorkspace: "选择工作区目录",
      stepModel: "连接模型提供商",
      stepPermissions: "选择权限模式",
      workspaceHint: "会话与产物将保存在此目录下。",
      browse: "浏览…",
      presets: {
        openai: "OpenAI",
        anthropic: "Anthropic",
        compatible: "兼容接口",
        local: "本地服务",
      },
      profileName: "配置名称",
      contextWindow: "上下文窗口",
      baseUrl: "API 基础地址",
      modelName: "模型",
      apiKeyLabel: "API 密钥",
      pasteKeyLabel: "粘贴密钥",
      useEnvVarLabel: "引用已有环境变量",
      envVarNameLabel: "变量名",
      apiKeyHint:
        "密钥保存在 helper 私有环境文件（~/.soyo/env）中，config.toml 只记录变量名。",
      noKeyNeeded: "该端点无需 API 密钥——请求将不携带认证信息。",
      permAuto: "自动",
      permAutoHint: "推荐——内置检查仍然生效",
      permManual: "手动批准",
      permManualHint: "每次工具调用执行前都需要批准",
      permBypass: "跳过权限确认",
      permBypassHint: "不再提示——仅底层硬性限制生效",
      backLabel: "上一步",
      continueLabel: "继续",
      finishLabel: "完成设置",
      applyingLabel: "应用中…",
    },
    sessionBrowser: {
      title: "会话与项目历史",
      configChangedPrefix: "自会话",
      configChangedSuffix: "保存以来配置已变更，确认使用当前配置恢复吗？",
      acceptAndResume: "接受配置变更并恢复",
      cancel: "取消",
      loading: "正在加载会话…",
      emptyPage: "此页未找到会话。",
      active: "当前",
      current: "当前",
      resume: "恢复",
      detailsHeader: "会话详情",
      project: "项目",
      profile: "配置",
      mode: "模式",
      status: "状态",
      selectToInspect: "选择一个会话以查看详情。",
      pageLabel: (p) => `第 ${p} 页`,
      prev: "上一页",
      next: "下一页",
    },
    mcpDialog: {
      title: "MCP 服务器与凭据（仅脱敏元数据）",
      configuredServers: "已配置的 MCP 服务器",
      noServers: "暂未配置 MCP 服务器。",
      enabled: "已启用",
      disabled: "已禁用",
      storedCredentials: "已保存的凭据引用",
      noCredentials: "暂无已保存的主机凭据引用。",
      revoked: "已吊销",
      revoke: "吊销",
      importTitle: "从环境变量导入 Bearer 密钥",
      importDesc: "明文令牌仅保存在主机内存与私有存储中；此处只需填写环境变量名称。",
      serverPlaceholder: "HTTP MCP 服务器名称",
      envPlaceholder: "环境变量名（例如 MCP_TOKEN）",
      scopesPlaceholder: "可选逗号分隔的作用域列表",
      approveScopesLabel: "明确批准服务端点与权限作用域",
      importBtn: "导入凭据引用",
    },
    memoryDialog: {
      title: "全局记忆管理（两阶段复核）",
      queryBtn: "查询",
      prepareTitle: "准备记忆变更 / 导出",
      preparePlaceholder:
        "例如 forget <fact_id> | correct <fact_id> <text> | export <path>",
      prepareBtn: "生成复核",
      discardBtn: "放弃",
      confirmBtn: "确认应用",
      appliedMsg: (opId) => `已应用记忆操作 ${opId}`,
    },
    exportDialog: {
      title: "导出会话记录",
      published: "导出已生成",
      outputPathLabel: (ws) => `输出文件路径（相对于 ${ws} 或绝对路径）`,
      browse: "浏览…",
      format: "格式",
      scope: "范围",
      scopeTranscript: "对话记录",
      scopeFull: "完整（含事件与指标）",
      includeTools: "包含工具参数与结果内容",
      close: "关闭",
      exporting: "正在导出…",
      publishBtn: "确认导出",
    },
    modulesPanel: {
      runtimeModules: "运行时模块",
      noModules: "暂无模块状态信息。",
      enabled: "已启用",
      disabled: "已禁用",
      extensionsHeader: (n) => `扩展与项目发现（${n}）`,
      noExtensions: "当前工作区未发现规则、技能或插件。",
      jobsHeader: (n) => `后台任务（${n}）`,
      noJobs: "当前活动会话没有后台任务记录。",
      stateLabel: "状态",
      cancel: "取消",
    },
    approval: {
      title: "需要人工批准",
      tool: "工具",
      action: "操作",
      call: "调用",
      workspace: "工作区",
      pressPrefix: "按",
      pressMid: "允许，或按",
      pressSuffix: "拒绝",
      denyBtn: "拒绝 (N)",
      allowBtn: "允许 (Y)",
    },
    clarification: {
      title: "需要补充说明",
      suggestedOptions: "建议选项",
      customAnswer: "自定义回答",
      placeholder: "输入您的回复…",
      submitBtn: "提交",
    },
    effort: {
      pillPrefix: "思考强度",
      header: "思考强度",
      faster: "更快",
      smarter: "更深",
      helpTip: "向左更快速，向右更深入。选择 Ultracode 可启用会话级动态工作流。",
      ultracodeTitle: "Ultracode",
      ultracodeDesc: "为当前会话开启持续动态工作流编排。",
      levels: {
        low: {
          name: "低",
          desc: "几乎不加思考的快速回复。",
        },
        medium: {
          name: "中等",
          desc: "兼顾响应速度与日常推理能力。",
        },
        high: {
          name: "高",
          desc: "针对复杂问题进行更深入推理。",
        },
        max: {
          name: "极高",
          desc: "极限深度推理，将更快消耗额度。",
          warning: "选择极高强度时模型思考时间更长，用量额度消耗也比“高”更快。",
        },
        ultracode: {
          name: "Ultracode",
          desc: "最高思考强度，并为本次会话启用动态工作流。",
        },
      },
    },
    cards: {
      thinking: "思考过程",
      thinkingActive: "正在思考…",
      thought: "已思考",
      reasoning: "推理中",
      autoCollapsed: "已自动折叠",
      truncated: "已截断",
      empty: "（空）",
      reasoningUnavailable: "推理追踪不可用或已脱敏。",
      requested: "已请求",
      running: "运行中",
      error: "错误",
      done: "已完成",
      tool: "工具",
      subagent: "子代理",
      reviewPrefix: "复核：",
      hunkCount: (n) => `${n} 个变更块`,
      collapseDiff: "收起差异预览 ‹",
      expandDiff: (n) => `已隐藏 ${n} 行 · 展开 ›`,
      requestHeader: "请求参数",
      outputHeader: "输出结果",
      attachmentsHeader: (n) => `附件（${n}）`,
    },
    updater: {
      available: (v) => `发现可用更新：v${v}`,
      current: (v) => `（当前 v${v}）`,
      downloading: "正在下载…",
      downloadedInstalling: "下载完成，正在安装…",
      installedRestart: "更新已安装 — 请重启 Soyo 完成应用。",
      installingBtn: "正在安装…",
      installRestartBtn: "安装并重启",
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
      savedStatus: "已儲存",
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
      mcpActiveCount: (n) => `已註冊 ${n} 個 MCP 伺服器（主機去識別化保護）。`,
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
      greetings: {
        morning: [
          "早安，{name}。",
          "早上好。",
          "新的一天，開始吧。",
        ],
        afternoon: [
          "午安，{name}。",
          "下午好。",
          "午後時光，慢慢來。",
        ],
        evening: [
          "晚上好，{name}。",
          "晚間好。",
          "收工還是繼續？",
        ],
        night: [
          "夜深了，{name}。",
          "還在忙？",
          "深夜適合專注。",
        ],
        anytime: [
          "你來了！",
          "歡迎回來，{name}。",
          "歡迎，{name}。",
          "很高興見到你，{name}。",
          "嗨，{name}。",
          "開始建構吧。",
          "今天做點什麼？",
          "這個專案裡想做點什麼？",
          "今天在想什麼？",
          "今天的議程是什麼？",
          "接下來做什麼，{name}？",
          "我們從哪裡開始？",
          "準備好開始了？",
          "隨時待命。",
          "隨時可以開始。",
          "有什麼可以幫你，{name}？",
          "今天有什麼可以幫你？",
          "{name}，我能幫你做些什麼？",
          "你來啦！",
          "嘿嘿，是你呀。",
          "等你好久啦。",
          "開工啦開工啦。",
          "今天也要加油喵。",
          "Let's GO!",
          "出發嘍！",
          "加入遊戲。",
        ],
      },
      fallbackName: "朋友",
      projectOrFolder: "專案或資料夾",
      ideasForYou: "為你推薦",
      ideaSummarize: "將今天的工作階段整理為交接筆記",
      ideaAuditMcp: "稽核 MCP 伺服器與憑證範圍",
      ideaCustomize: "為此工作區自訂 Soyo",
      browserNew: "+ 新增",
      configIncomplete: "設定不完整",
      configFallback: "請先選擇或設定一個模型設定檔。",
      historyOmitted: "較早的歷史紀錄已略過以維持還原效能；完整紀錄仍保留在磁碟上。",
      loadingRuntime: "正在載入執行階段狀態…",
      streaming: "串流輸出中",
      copied: "已複製",
      retryPrompt: "重試提示詞",
      editMessage: "於輸入框中編輯",
      copyMessage: "複製訊息",
      exportShare: "匯出 / 分享",
      branchUnsupported: "從此訊息建立分支（執行階段尚未支援）",
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
      queuedPrompt: "已排程",
      compactionQueued: "壓縮已排程",
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
      activeSkills: (n) => `${n} 個啟用的技能`,
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
      usageSubtitle: "此裝置上所有工作階段的 Token 用量統計",
      tokensUnit: "tokens",
      inputLabel: "輸入",
      outputLabel: "輸出",
      cacheReadLabel: "快取讀取",
      inputLower: "輸入",
      outputLower: "輸出",
      cacheReadLower: "快取讀取",
      totalLower: "總計",
      tokensPerDay: "每日 Token 用量",
      today: "今天",
      windowLabel: (r) => `視窗範圍：${r}`,
      liveSession: "作用中工作階段",
      idle: "閒置",
      purposeExecution: "執行",
      purposeReview: "審查",
      purposeCompaction: "壓縮",
      runsLabel: "次執行",
      usageLoading: "正在載入用量…",
      usageEmpty: "此時間範圍內暫無用量記錄",
      usageEstimated: "包含估算值",
      workspaceDir: "工作區目錄",
      browse: "瀏覽…",
      apply: "套用",
      languageLabel: "介面語言",
      profileLabel: "個人資料",
      avatarLabel: "頭像",
      changeAvatar: "更換…",
      resetAvatar: "重置",
      nicknameLabel: "暱稱",
      nicknameHint: "顯示在問候語中；留空則使用系統帳戶名。",
      globalPromptLabel: "全局提示詞",
      globalPromptHint: (path: string) =>
        `對所有工作區的工作階段生效，儲存到 ${path}，不會修改專案內檔案。`,
      savePrompt: "儲存",
      promptSaved: "已儲存",
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
    setup: {
      welcome: "歡迎使用 Soyo",
      welcomeSubtitle: "完成基礎設定後即可開始使用",
      skipLabel: "稍後設定",
      stepOf: (n, total) => `第 ${n} / ${total} 步`,
      stepLanguage: "選擇介面語言",
      stepWorkspace: "選擇工作區目錄",
      stepModel: "連線模型供應商",
      stepPermissions: "選擇權限模式",
      workspaceHint: "工作階段與產出物將儲存在此目錄下。",
      browse: "瀏覽…",
      presets: {
        openai: "OpenAI",
        anthropic: "Anthropic",
        compatible: "相容介面",
        local: "本機服務",
      },
      profileName: "設定檔名稱",
      contextWindow: "上下文視窗",
      baseUrl: "API 基礎位址",
      modelName: "模型",
      apiKeyLabel: "API 金鑰",
      pasteKeyLabel: "貼上金鑰",
      useEnvVarLabel: "引用既有環境變數",
      envVarNameLabel: "變數名稱",
      apiKeyHint:
        "金鑰儲存在 helper 私有環境檔案（~/.soyo/env）中，config.toml 只記錄變數名稱。",
      noKeyNeeded: "此端點無需 API 金鑰——請求將不攜帶認證資訊。",
      permAuto: "自動",
      permAutoHint: "建議——內建檢查依然生效",
      permManual: "手動核准",
      permManualHint: "每次工具呼叫執行前都需要核准",
      permBypass: "略過權限確認",
      permBypassHint: "不再提示——僅底層硬性限制生效",
      backLabel: "上一步",
      continueLabel: "繼續",
      finishLabel: "完成設定",
      applyingLabel: "套用中…",
    },
    sessionBrowser: {
      title: "工作階段與專案歷史",
      configChangedPrefix: "自工作階段",
      configChangedSuffix: "儲存以來設定檔已變更，確定要使用目前設定繼續嗎？",
      acceptAndResume: "接受設定變更並復原",
      cancel: "取消",
      loading: "正在載入工作階段…",
      emptyPage: "此頁找不到工作階段。",
      active: "目前",
      current: "目前",
      resume: "復原",
      detailsHeader: "工作階段詳情",
      project: "專案",
      profile: "設定檔",
      mode: "模式",
      status: "狀態",
      selectToInspect: "選擇一個工作階段以檢視詳情。",
      pageLabel: (p) => `第 ${p} 頁`,
      prev: "上一頁",
      next: "下一頁",
    },
    mcpDialog: {
      title: "MCP 伺服器與憑證（僅去識別化中繼資料）",
      configuredServers: "已設定的 MCP 伺服器",
      noServers: "尚未設定 MCP 伺服器。",
      enabled: "已啟用",
      disabled: "已停用",
      storedCredentials: "已儲存的憑證參照",
      noCredentials: "尚無已儲存的主機憑證參照。",
      revoked: "已撤銷",
      revoke: "撤銷",
      importTitle: "從環境變數匯入 Bearer 密鑰",
      importDesc: "明文權杖僅保留於主機記憶體與私密儲存區；此處只需輸入環境變數名稱。",
      serverPlaceholder: "HTTP MCP 伺服器名稱",
      envPlaceholder: "環境變數名稱（例如 MCP_TOKEN）",
      scopesPlaceholder: "選填以逗號分隔的權限範圍",
      approveScopesLabel: "明確核准端點與權限範圍",
      importBtn: "匯入憑證參照",
    },
    memoryDialog: {
      title: "全域記憶管理（兩階段複核）",
      queryBtn: "查詢",
      prepareTitle: "準備記憶變更 / 匯出",
      preparePlaceholder:
        "例如 forget <fact_id> | correct <fact_id> <text> | export <path>",
      prepareBtn: "準備複核",
      discardBtn: "捨棄",
      confirmBtn: "確認套用",
      appliedMsg: (opId) => `已套用記憶操作 ${opId}`,
    },
    exportDialog: {
      title: "匯出工作階段紀錄",
      published: "匯出已發佈",
      outputPathLabel: (ws) => `輸出檔案路徑（相對於 ${ws} 或絕對路徑）`,
      browse: "瀏覽…",
      format: "格式",
      scope: "範圍",
      scopeTranscript: "對話紀錄",
      scopeFull: "完整（含事件與指標）",
      includeTools: "包含工具參數與結果內容",
      close: "關閉",
      exporting: "正在匯出…",
      publishBtn: "發佈匯出",
    },
    modulesPanel: {
      runtimeModules: "執行階段模組",
      noModules: "尚無模組狀態資訊。",
      enabled: "已啟用",
      disabled: "已停用",
      extensionsHeader: (n) => `擴充功能與專案探索（${n}）`,
      noExtensions: "目前工作區未發現規則、技能或外掛程式。",
      jobsHeader: (n) => `背景任務（${n}）`,
      noJobs: "目前作用中工作階段沒有背景任務紀錄。",
      stateLabel: "狀態",
      cancel: "取消",
    },
    approval: {
      title: "需要人工核准",
      tool: "工具",
      action: "動作",
      call: "呼叫",
      workspace: "工作區",
      pressPrefix: "按",
      pressMid: "允許，或按",
      pressSuffix: "拒絕",
      denyBtn: "拒絕 (N)",
      allowBtn: "允許 (Y)",
    },
    clarification: {
      title: "需要補充說明",
      suggestedOptions: "建議選項",
      customAnswer: "自訂回答",
      placeholder: "輸入您的回覆…",
      submitBtn: "送出",
    },
    effort: {
      pillPrefix: "思考強度",
      header: "思考強度",
      faster: "更快",
      smarter: "更深",
      helpTip: "向左較快速，向右更深入。選擇 Ultracode 可啟用工作階段動態工作流。",
      ultracodeTitle: "Ultracode",
      ultracodeDesc: "為此工作階段持續啟用動態工作流編排。",
      levels: {
        low: {
          name: "低",
          desc: "極簡思考的快速回覆。",
        },
        medium: {
          name: "中等",
          desc: "兼顧回應速度與日常推理能力。",
        },
        high: {
          name: "高",
          desc: "針對複雜問題進行更深入推理。",
        },
        max: {
          name: "極高",
          desc: "最高推理深度，將更快消耗額度。",
          warning: "使用極高強度時模型思考更久，額度消耗也比「高」更快。",
        },
        ultracode: {
          name: "Ultracode",
          desc: "最高思考強度，並為此工作階段啟用動態工作流。",
        },
      },
    },
    cards: {
      thinking: "思考過程",
      thinkingActive: "正在思考…",
      thought: "已思考",
      reasoning: "推理中",
      autoCollapsed: "已自動摺疊",
      truncated: "已截斷",
      empty: "（空白）",
      reasoningUnavailable: "推理追蹤不可用或已去識別化。",
      requested: "已請求",
      running: "執行中",
      error: "錯誤",
      done: "已完成",
      tool: "工具",
      subagent: "子代理",
      reviewPrefix: "複核：",
      hunkCount: (n) => `${n} 個變更區塊`,
      collapseDiff: "收合差異預覽 ‹",
      expandDiff: (n) => `已隱藏 ${n} 行 · 展開 ›`,
      requestHeader: "請求參數",
      outputHeader: "輸出結果",
      attachmentsHeader: (n) => `附件（${n}）`,
    },
    updater: {
      available: (v) => `有可用更新：v${v}`,
      current: (v) => `（目前 v${v}）`,
      downloading: "正在下載…",
      downloadedInstalling: "下載完成，正在安裝…",
      installedRestart: "更新已安裝 — 請重新啟動 Soyo 完成套用。",
      installingBtn: "正在安裝…",
      installRestartBtn: "安裝並重啟",
    },
  },
};

export function getMessages(locale: LocaleId): UiMessages {
  return MESSAGES[locale] ?? MESSAGES[DEFAULT_LOCALE];
}

export function formatGreeting(
  template: string,
  name: string | null,
  fallbackName: string,
): string {
  return template.replaceAll("{name}", name?.trim() || fallbackName);
}

export function greetingCandidates(
  home: UiMessages["home"],
  name: string | null,
): string[] {
  const g = home.greetings;
  return [...g.morning, ...g.afternoon, ...g.evening, ...g.night, ...g.anytime].map(
    (tpl) => formatGreeting(tpl, name, home.fallbackName),
  );
}

export function pickGreeting(
  home: UiMessages["home"],
  name: string | null,
  now: Date = new Date(),
  rand: () => number = Math.random,
): string {
  const g = home.greetings;
  const pick = (arr: string[]): string =>
    arr[Math.floor(rand() * arr.length)] ?? arr[0] ?? "";
  const tpl =
    g.anytime.length > 0 && rand() < 0.15
      ? pick(g.anytime)
      : (() => {
          const h = now.getHours();
          if (h >= 5 && h < 12) return pick(g.morning);
          if (h >= 12 && h < 18) return pick(g.afternoon);
          if (h >= 18 && h < 23) return pick(g.evening);
          return pick(g.night);
        })();
  return formatGreeting(tpl, name, home.fallbackName);
}
