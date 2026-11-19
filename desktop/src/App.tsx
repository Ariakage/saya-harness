import {
  ArrowDown,
  ArrowLeft,
  ArrowRight,
  ArrowUp,
  Bookmark,
  Box,
  Check,
  ChevronDown,
  ChevronsUpDown,
  Clock,
  Code2,
  Copy,
  Database,
  Download,
  ExternalLink,
  Eye,
  Flag,
  Folder,
  GitBranch,
  Globe,
  History,
  KeyRound,
  Lock,
  MessageSquare,
  MoreHorizontal,
  Palette,
  PanelLeft,
  PanelRight,
  Pause,
  Pencil,
  Plus,
  RotateCw,
  Search,
  Settings,
  Share2,
  ShieldAlert,
  SlidersHorizontal,
  Sparkles,
  Square,
  SunMoon,
  X,
} from "lucide-react";
import { isTauri } from "@tauri-apps/api/core";
import { useEffect, useMemo, useRef, useState } from "react";
import type { SessionItemDto_Serialize } from "./bindings";
import { ApprovalDialog } from "./components/ApprovalDialog";
import { ClarificationDialog } from "./components/ClarificationDialog";
import { Composer } from "./components/Composer";
import { ContextSidebar } from "./components/ContextSidebar";
import { ExportDialog } from "./components/ExportDialog";
import { LanguageDialog } from "./components/LanguageDialog";
import { MarkdownRenderer } from "./components/MarkdownRenderer";
import { SessionBrowserDialog } from "./components/SessionBrowserDialog";
import { SettingsDialog, type SettingsTab } from "./components/SettingsDialog";
import { SetupWizard } from "./components/SetupWizard";
import { SquaresLoader } from "./components/SquaresLoader";
import { ThinkingCardView } from "./components/ThinkingCardView";
import { ToolCardView } from "./components/ToolCardView";
import { UpdaterBanner } from "./components/UpdaterBanner";
import { Button } from "./components/ui/Button";
import { DialogShell } from "./components/ui/DialogShell";
import { getMessages, pickGreeting } from "./lib/i18n";
import {
  PERMISSION_MODE_LABELS,
  WORK_MODE_STYLES,
  formatTokenCount,
  sanitizeDisplayText,
} from "./lib/sanitize";
import {
  PREVIEW_APPROVAL_REQUEST,
  PREVIEW_INPUT_REQUEST,
  type PreviewScene,
  getPreviewScene,
  getPreviewTab,
  isBrowserPreview,
  setPreviewScene,
} from "./preview/browserPreview";
import { type ThemePreference, useAppStore } from "./store/useAppStore";

const TITLEBAR_OVERLAY =
  isTauri() && /mac/i.test(typeof navigator !== "undefined" ? navigator.userAgent : "");

const MAX_RECENTS_PER_PROJECT = 5;
const MAX_RECENTS_TOTAL = 30;

const VALID_SETTINGS_TABS: readonly SettingsTab[] = [
  "general",
  "appearance",
  "profiles",
  "permissions",
  "modules",
  "mcp",
  "memory",
  "usage",
  "export",
];

function isSettingsTab(value: string | null): value is SettingsTab {
  return value !== null && (VALID_SETTINGS_TABS as readonly string[]).includes(value);
}

function workspaceBase(workspace: string | null | undefined, fallback: string): string {
  if (!workspace) return fallback;
  const trimmed = workspace.replace(/\/+$/, "");
  return trimmed.split("/").pop() || workspace;
}

function groupRecentsByProject(
  sessions: SessionItemDto_Serialize[],
  workspaceFallback: string
): {
  groups: Array<[string, SessionItemDto_Serialize[]]>;
  omittedCount: number;
} {
  const byProject = new Map<string, SessionItemDto_Serialize[]>();
  for (const item of sessions) {
    const project = workspaceBase(item.workspace, workspaceFallback);
    const list = byProject.get(project) ?? [];
    list.push(item);
    byProject.set(project, list);
  }

  const groups: Array<[string, SessionItemDto_Serialize[]]> = [];
  let shown = 0;
  for (const [project, list] of byProject.entries()) {
    if (shown >= MAX_RECENTS_TOTAL) break;
    const budget = Math.min(MAX_RECENTS_PER_PROJECT, MAX_RECENTS_TOTAL - shown);
    const slice = list.slice(0, budget);
    if (slice.length > 0) {
      groups.push([project, slice]);
      shown += slice.length;
    }
  }
  return {
    groups,
    omittedCount: Math.max(0, sessions.length - shown),
  };
}

const PREVIEW_SCENES: PreviewScene[] = [
  "home",
  "conversation",
  "approval",
  "effort",
  "usage",
  "browser",
  "subagent",
  "markdown",
  "clarification",
  "peripheral",
];

export function App() {
  const bootstrap = useAppStore((s) => s.bootstrap);
  const loading = useAppStore((s) => s.loading);
  const sessionId = useAppStore((s) => s.sessionId);
  const sessionTitle = useAppStore((s) => s.sessionTitle);
  const historyOmitted = useAppStore((s) => s.historyOmitted);
  const workMode = useAppStore((s) => s.workMode);
  const pendingMode = useAppStore((s) => s.pendingMode);
  const permissionMode = useAppStore((s) => s.permissionMode);
  const messages = useAppStore((s) => s.messages);
  const thinkingEntries = useAppStore((s) => s.thinkingEntries);
  const contextEntries = useAppStore((s) => s.contextEntries);
  const contextSkillsCount = useAppStore((s) => s.contextSkillsCount);
  const contextEstimated = useAppStore((s) => s.contextEstimated);
  const mcpServers = useAppStore((s) => s.mcpServers);
  const todos = useAppStore((s) => s.todos);
  const lastUsage = useAppStore((s) => s.lastUsage);
  const recentSessions = useAppStore((s) => s.recentSessions);
  const runActive = useAppStore((s) => s.runActive);
  const cancelling = useAppStore((s) => s.cancelling);
  const summaryLine = useAppStore((s) => s.summaryLine);
  const markdownMode = useAppStore((s) => s.markdownMode);
  const pendingApproval = useAppStore((s) => s.pendingApproval);
  const pendingInput = useAppStore((s) => s.pendingInput);
  const stagedAttachments = useAppStore((s) => s.stagedAttachments);
  const ultracodeEnabled = useAppStore((s) => s.ultracodeEnabled);
  const submitMode = useAppStore((s) => s.submitMode);
  const queuedPrompt = useAppStore((s) => s.queuedPrompt);
  const compactionPending = useAppStore((s) => s.compactionPending);
  const configChangeSessionId = useAppStore((s) => s.configChangeSessionId);
  const recoveryBanner = useAppStore((s) => s.recoveryBanner);
  const error = useAppStore((s) => s.error);
  const theme = useAppStore((s) => s.theme);
  const inspectorOpenPref = useAppStore((s) => s.inspectorOpenPref);
  const helperReconnecting = useAppStore((s) => s.helperReconnecting);

  const initApp = useAppStore((s) => s.initApp);
  const applyBootstrap = useAppStore((s) => s.applyBootstrap);
  const setThinkingEffort = useAppStore((s) => s.setThinkingEffort);
  const selectProfile = useAppStore((s) => s.selectProfile);
  const setSubmitMode = useAppStore((s) => s.setSubmitMode);
  const startNewSession = useAppStore((s) => s.startNewSession);
  const stageAttachments = useAppStore((s) => s.stageAttachments);
  const stagePastedAttachments = useAppStore((s) => s.stagePastedAttachments);
  const removeStagedAttachment = useAppStore((s) => s.removeStagedAttachment);
  const submitPrompt = useAppStore((s) => s.submitPrompt);
  const cancelActiveRun = useAppStore((s) => s.cancelActiveRun);
  const compactCurrentSession = useAppStore((s) => s.compactCurrentSession);
  const replyApproval = useAppStore((s) => s.replyApproval);
  const replyInput = useAppStore((s) => s.replyInput);
  const setWorkMode = useAppStore((s) => s.setWorkMode);
  const cycleWorkMode = useAppStore((s) => s.cycleWorkMode);
  const setPermissionMode = useAppStore((s) => s.setPermissionMode);
  const clearQueuedPrompt = useAppStore((s) => s.clearQueuedPrompt);
  const resumeSession = useAppStore((s) => s.resumeSession);
  const clearConfigChangePrompt = useAppStore((s) => s.clearConfigChangePrompt);
  const setMarkdownMode = useAppStore((s) => s.setMarkdownMode);
  const toggleMarkdownMode = useAppStore((s) => s.toggleMarkdownMode);
  const locale = useAppStore((s) => s.locale);
  const setLocale = useAppStore((s) => s.setLocale);
  const setTheme = useAppStore((s) => s.setTheme);
  const applySystemThemeChange = useAppStore((s) => s.applySystemThemeChange);
  const setInspectorOpenPref = useAppStore((s) => s.setInspectorOpenPref);
  const reconnectHelper = useAppStore((s) => s.reconnectHelper);
  const clearError = useAppStore((s) => s.clearError);
  const t = getMessages(locale);
  const userName = useAppStore((s) => s.userName);
  const nickname = useAppStore((s) => s.nickname);
  const avatarData = useAppStore((s) => s.avatarData);
  const displayName = nickname ?? userName;
  const greeting = useMemo(
    () => pickGreeting(t.home, displayName),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [locale, displayName]
  );

  const previewMode = isBrowserPreview();
  const [scene, setScene] = useState<PreviewScene>(() => getPreviewScene());
  const [viewportWidth, setViewportWidth] = useState<number>(() =>
    typeof window !== "undefined" ? window.innerWidth : 1280
  );
  const [userSidebarCollapsed, setUserSidebarCollapsed] = useState<boolean | null>(
    null
  );
  const [drawerInspectorOpen, setDrawerInspectorOpen] = useState(false);

  const [sessionsOpen, setSessionsOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [setupDismissed, setSetupDismissed] = useState(false);
  const [settingsTab, setSettingsTab] = useState<SettingsTab>("general");
  const [languageOpen, setLanguageOpen] = useState(false);
  const [exportOpen, setExportOpen] = useState(false);
  const [searchOverlayOpen, setSearchOverlayOpen] = useState(false);
  const [userMenuOpen, setUserMenuOpen] = useState(false);
  const [browserPaneOpen, setBrowserPaneOpen] = useState(false);
  const [browserMiniTab, setBrowserMiniTab] = useState<
    "new" | "search" | "sessions" | "memory" | "customize"
  >("new");
  const [browserRefreshTick, setBrowserRefreshTick] = useState(0);
  const [copiedId, setCopiedId] = useState<string | null>(null);
  const [editDraft, setEditDraft] = useState<{
    text: string;
    nonce: number;
  } | null>(null);
  const [forceHome, setForceHome] = useState(false);
  const [topMenu, setTopMenu] = useState<"mode" | "perm" | "more" | null>(null);
  const [sessionQuery, setSessionQuery] = useState("");
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const userMenuRef = useRef<HTMLDivElement | null>(null);
  const topbarClusterRef = useRef<HTMLDivElement | null>(null);

  const isWideViewport = viewportWidth >= 1280;
  const isNarrowViewport = viewportWidth < 1024;
  const sidebarCollapsed = userSidebarCollapsed ?? isNarrowViewport;
  const inspectorOpen = isWideViewport
    ? (inspectorOpenPref ?? true)
    : drawerInspectorOpen;

  const applyPreviewOverlay = (targetScene: PreviewScene) => {
    if (!previewMode) return;
    useAppStore.setState({
      pendingApproval:
        targetScene === "approval" ? PREVIEW_APPROVAL_REQUEST : null,
      pendingInput:
        targetScene === "clarification" ? PREVIEW_INPUT_REQUEST : null,
    });
    setForceHome(targetScene === "home");
    setBrowserPaneOpen(targetScene === "browser");
    const urlTab = getPreviewTab();
    if (isSettingsTab(urlTab)) {
      setSettingsTab(urlTab);
      setSettingsOpen(true);
    } else if (targetScene === "peripheral") {
      setSettingsTab("general");
      setSettingsOpen(true);
    } else if (targetScene === "usage") {
      setSettingsTab("usage");
      setSettingsOpen(true);
    } else {
      setSettingsOpen(false);
    }
    setExportOpen(false);
  };

  useEffect(() => {
    void initApp().then(() => {
      applyPreviewOverlay(getPreviewScene());
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [initApp]);

  // Follow system light/dark changes when theme preference is "system".
  useEffect(() => {
    if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
      return;
    }
    const mq = window.matchMedia("(prefers-color-scheme: light)");
    const onChange = () => applySystemThemeChange();
    mq.addEventListener?.("change", onChange);
    return () => mq.removeEventListener?.("change", onChange);
  }, [applySystemThemeChange]);

  // Track responsive breakpoints: >=1280 / 1024-1279 / <1024.
  useEffect(() => {
    if (typeof window === "undefined") return;
    let prevNarrow = window.innerWidth < 1024;
    const onResize = () => {
      const nextWidth = window.innerWidth;
      setViewportWidth(nextWidth);
      const nextNarrow = nextWidth < 1024;
      if (nextNarrow !== prevNarrow) {
        prevNarrow = nextNarrow;
        setUserSidebarCollapsed(null);
      }
      if (nextWidth < 1280) {
        setDrawerInspectorOpen(false);
      }
    };
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  useEffect(() => {
    const el = scrollRef.current;
    if (el) {
      el.scrollTop = el.scrollHeight;
    }
  }, [messages.length, thinkingEntries.length, pendingApproval]);

  const handleSceneChange = (nextScene: PreviewScene) => {
    setPreviewScene(nextScene);
    setScene(nextScene);
    void initApp().then(() => {
      applyPreviewOverlay(nextScene);
    });
  };

  const handleOpenSettings = (tab: SettingsTab = "general") => {
    setSettingsTab(tab);
    setSettingsOpen(true);
    setUserMenuOpen(false);
  };

  const handleToggleSidebar = () => {
    setUserSidebarCollapsed(!(userSidebarCollapsed ?? isNarrowViewport));
  };

  const handleToggleInspector = () => {
    if (isWideViewport) {
      void setInspectorOpenPref(!(inspectorOpenPref ?? true));
    } else {
      setDrawerInspectorOpen((v) => !v);
    }
  };

  // Global desktop shortcuts: ⌘N new, ⌘K search overlay, ⌘B toggle sidebar, ⌘, settings.
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (!(e.metaKey || e.ctrlKey) || e.altKey) return;
      const key = e.key.toLowerCase();
      if (key === "b") {
        e.preventDefault();
        setUserSidebarCollapsed((prev) => !(prev ?? viewportWidth < 1024));
      } else if (key === "k") {
        e.preventDefault();
        setSearchOverlayOpen((v) => !v);
      } else if (key === ",") {
        e.preventDefault();
        handleOpenSettings("general");
      } else if (key === "n" && !runActive) {
        e.preventDefault();
        setForceHome(false);
        void startNewSession();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [viewportWidth, runActive, startNewSession]);

  // Close lightweight popovers (topbar menus, user menu) on outside press.
  useEffect(() => {
    if (!topMenu && !userMenuOpen) return;
    const onDown = (e: MouseEvent) => {
      const target = e.target as Node;
      if (
        topMenu &&
        topbarClusterRef.current &&
        !topbarClusterRef.current.contains(target)
      ) {
        setTopMenu(null);
      }
      if (
        userMenuOpen &&
        userMenuRef.current &&
        !userMenuRef.current.contains(target)
      ) {
        setUserMenuOpen(false);
      }
    };
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [topMenu, userMenuOpen]);

  const handleCopyMessage = async (id: string, text: string) => {
    try {
      await navigator.clipboard?.writeText(text);
      setCopiedId(id);
      setTimeout(() => setCopiedId((prev) => (prev === id ? null : prev)), 1200);
    } catch {
      // Clipboard unavailable in headless environments.
    }
  };

  const needsSetup =
    bootstrap !== null &&
    (!bootstrap.config_ready || bootstrap.profiles.length === 0);
  const isHomeEmpty =
    forceHome ||
    (!loading &&
      messages.length === 0 &&
      thinkingEntries.length === 0 &&
      !pendingApproval);

  const currentProject = workspaceBase(
    bootstrap?.workspace,
    t.sessionBrowser.workspaceFallback
  );
  const { groups: recentGroups, omittedCount: recentsOmittedCount } =
    groupRecentsByProject(recentSessions, t.sessionBrowser.workspaceFallback);

  const query = sessionQuery.trim().toLowerCase();
  const filteredSessions = query
    ? recentSessions.filter((item) => {
        const titleHit = (item.title ?? "").toLowerCase().includes(query);
        const projHit = workspaceBase(
          item.workspace,
          t.sessionBrowser.workspaceFallback
        )
          .toLowerCase()
          .includes(query);
        const contentHit =
          item.id === sessionId &&
          messages.some((m) => m.text.toLowerCase().includes(query));
        return titleHit || projHit || contentHit;
      })
    : recentSessions;

  const forceComposerPopover =
    previewMode && scene === "effort" ? "effort" : null;

  const contextEntriesTotal = contextEntries.reduce(
    (acc, entry) => acc + entry.tokens,
    0
  );
  const contextUsedTokens =
    contextEntriesTotal > 0
      ? contextEntriesTotal
      : (lastUsage?.input_tokens ?? null);
  const contextWindowTokens = bootstrap?.context_window_tokens ?? null;
  const composerContextEstimated =
    contextEntriesTotal === 0 || contextEstimated;

  const sidebarNavItems: Array<{
    id: string;
    label: string;
    shortcut?: string;
    icon: typeof Plus;
    disabled?: boolean;
    onClick: () => void;
  }> = [
    {
      id: "new",
      label: t.sidebar.newSession,
      shortcut: "⌘N",
      icon: Plus,
      disabled: runActive,
      onClick: () => {
        setForceHome(false);
        void startNewSession();
      },
    },
    {
      id: "search",
      label: t.sidebar.searchTitle,
      shortcut: "⌘K",
      icon: Search,
      onClick: () => setSearchOverlayOpen(true),
    },
    {
      id: "sessions",
      label: t.sidebar.sessionsTitle,
      icon: History,
      onClick: () => setSessionsOpen(true),
    },
    {
      id: "memory",
      label: t.sidebar.memoryTitle,
      icon: Database,
      onClick: () => handleOpenSettings("memory"),
    },
    {
      id: "mcp",
      label: t.sidebar.mcpTitle,
      icon: KeyRound,
      onClick: () => handleOpenSettings("mcp"),
    },
    {
      id: "export",
      label: t.sidebar.exportTitle,
      icon: Download,
      disabled: sessionId === null,
      onClick: () => setExportOpen(true),
    },
    {
      id: "customize",
      label: t.sidebar.customizeTitle,
      icon: Palette,
      onClick: () => handleOpenSettings("appearance"),
    },
  ];

  return (
    <div className="flex h-screen w-screen flex-col overflow-hidden bg-[var(--surface-app)] text-[var(--text-primary)]">
      {/* Unified Overlay Titlebar (38px) */}
      <header
        data-tauri-drag-region
        className="relative z-30 flex h-[38px] w-full shrink-0 select-none items-center border-b border-[var(--border-subtle)] bg-[var(--surface-sidebar)]"
      >
        {/* Left Titlebar Zone: clears macOS traffic lights & aligns with 248px expanded sidebar */}
        <div
          data-tauri-drag-region
          className={`flex h-full shrink-0 items-center ${
            !sidebarCollapsed
              ? `w-[248px] justify-between border-r border-[var(--border-subtle)] bg-[var(--surface-sidebar)] ${
                  TITLEBAR_OVERLAY ? "pl-[88px] pr-2.5" : "px-2.5"
                }`
              : `gap-1.5 ${TITLEBAR_OVERLAY ? "pl-[88px] pr-1" : "pl-2.5 pr-1"}`
          }`}
        >
          <button
            type="button"
            onClick={handleToggleSidebar}
            aria-label={t.a11y.toggleSidebar}
            aria-pressed={!sidebarCollapsed}
            title={
              sidebarCollapsed ? t.a11y.expandSidebar : t.a11y.collapseSidebar
            }
            className="flex h-[26px] w-[26px] items-center justify-center rounded-[var(--radius-sm)] text-[var(--text-tertiary)] transition-colors hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]"
          >
            <PanelLeft className="h-4 w-4" strokeWidth={1.75} />
          </button>

          <div
            role="group"
            aria-label={t.a11y.viewMode}
            className="inline-flex h-[26px] items-center rounded-[var(--radius-md)] border border-[var(--border)] bg-[var(--surface-raised)] p-0.5"
          >
            <button
              type="button"
              onClick={() => setMarkdownMode("render")}
              aria-label={t.a11y.renderedView}
              aria-pressed={markdownMode === "render"}
              title={t.a11y.renderedView}
              className={`flex h-[20px] w-[26px] items-center justify-center rounded-[var(--radius-sm)] transition-colors ${
                markdownMode === "render"
                  ? "bg-[var(--surface-hover)] text-[var(--text-primary)]"
                  : "text-[var(--text-tertiary)] hover:text-[var(--text-secondary)]"
              }`}
            >
              <MessageSquare className="h-3.5 w-3.5" strokeWidth={1.75} />
            </button>
            <button
              type="button"
              onClick={() => setMarkdownMode("source")}
              aria-label={t.a11y.sourceView}
              aria-pressed={markdownMode === "source"}
              title={t.a11y.sourceView}
              className={`flex h-[20px] w-[26px] items-center justify-center rounded-[var(--radius-sm)] transition-colors ${
                markdownMode === "source"
                  ? "bg-[var(--surface-hover)] text-[var(--text-primary)]"
                  : "text-[var(--text-tertiary)] hover:text-[var(--text-secondary)]"
              }`}
            >
              <Code2 className="h-3.5 w-3.5" strokeWidth={1.75} />
            </button>
          </div>
        </div>

        {sidebarCollapsed && (
          <div className="mx-1 h-3.5 w-px bg-[var(--border-subtle)]" aria-hidden="true" />
        )}

        {/* Right Titlebar Zone */}
        <div
          data-tauri-drag-region
          className="flex h-full min-w-0 flex-1 items-center gap-2 px-3 text-[12.5px] text-[var(--text-secondary)]"
        >
          <button
            type="button"
            onClick={() => setSessionsOpen(true)}
            title={t.a11y.switchOrBrowseSessions}
            className="flex h-[26px] min-w-0 max-w-[380px] items-center gap-1.5 rounded-[var(--radius-sm)] px-2 text-left text-[12.5px] transition-colors hover:bg-[var(--surface-hover)]"
          >
            <span className="truncate font-medium text-[var(--text-primary)]">
              {isHomeEmpty ? t.topbar.home : sessionTitle}
            </span>
            <span className="hidden shrink-0 text-[var(--text-tertiary)] sm:inline">
              · {currentProject}
            </span>
            <ChevronDown className="h-3.5 w-3.5 shrink-0 text-[var(--text-tertiary)]" strokeWidth={1.75} />
          </button>

          <div data-tauri-drag-region className="h-full min-w-[16px] flex-1" />

          <div
            ref={topbarClusterRef}
            className="relative flex shrink-0 items-center gap-1"
            onBlur={(e) => {
              if (!e.currentTarget.contains(e.relatedTarget as Node)) {
                setTopMenu(null);
              }
            }}
          >
            {previewMode && (
              <div className="flex flex-wrap items-center gap-1 rounded-[var(--radius-md)] border border-[var(--border)] bg-[var(--surface-raised)] px-2 py-0.5 text-[11px]">
                <Sparkles className="h-3 w-3 text-[var(--accent-text)]" strokeWidth={1.75} />
                <span className="mr-0.5 text-[var(--accent-text)]">Preview</span>
                {PREVIEW_SCENES.map((item) => (
                  <button
                    key={item}
                    type="button"
                    onClick={() => handleSceneChange(item)}
                    className={`rounded px-1.5 py-0.5 ${
                      scene === item
                        ? "bg-[var(--accent)] font-semibold text-[var(--on-accent)]"
                        : "text-[var(--text-secondary)] hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]"
                    }`}
                  >
                    {item}
                  </button>
                ))}
              </div>
            )}

            {/* Goal Topbar Pill */}
            {(runActive || (previewMode && scene === "subagent")) && (
              <div
                data-testid="goal-topbar-pill"
                className="soyo-panel-enter inline-flex items-center gap-2 rounded-[var(--radius-md)] border border-[var(--warning)] bg-[var(--warning-surface)] px-2.5 py-1 text-[11.5px] text-[var(--warning)]"
              >
                <Flag className="h-3 w-3 shrink-0" strokeWidth={1.75} />
                <span className="max-w-[140px] truncate">{sessionTitle}</span>
                <button
                  type="button"
                  onClick={() => void cancelActiveRun()}
                  title={t.a11y.cancelRun}
                  aria-label={t.a11y.cancelRun}
                  className="flex h-4 w-4 items-center justify-center rounded-[var(--radius-xs)] border border-[var(--warning)] bg-[var(--surface-raised)] text-[var(--warning)] transition-colors hover:bg-[var(--surface-hover)]"
                >
                  <Pause className="h-2.5 w-2.5" strokeWidth={1.75} />
                </button>
                <button
                  type="button"
                  onClick={() => void cancelActiveRun()}
                  title={t.a11y.cancelRun}
                  aria-label={t.a11y.cancelRun}
                  className="flex h-4 w-4 items-center justify-center rounded-[var(--radius-xs)] border border-[var(--warning)] bg-[var(--surface-raised)] text-[var(--warning)] transition-colors hover:bg-[var(--surface-hover)]"
                >
                  <Square className="h-2 w-2 fill-current" />
                </button>
              </div>
            )}

            {/* Permanent Bypass Permissions Warning Badge (R-20, always visible) */}
            {permissionMode === "bypass" && (
              <button
                type="button"
                data-testid="bypass-mode-badge"
                onClick={() => setTopMenu(topMenu === "perm" ? null : "perm")}
                title={t.a11y.permissionMode}
                className="soyo-pill h-[26px] border-[var(--danger)] bg-[var(--danger-surface)] px-2.5 py-0 text-[11.5px] font-medium text-[var(--danger)]"
              >
                <ShieldAlert className="h-3.5 w-3.5 shrink-0" strokeWidth={1.75} />
                <span>{PERMISSION_MODE_LABELS.bypass}</span>
              </button>
            )}

            {/* Mode pill */}
            <div className="relative">
              <button
                type="button"
                onClick={() => setTopMenu(topMenu === "mode" ? null : "mode")}
                className="soyo-pill h-[26px] rounded-[var(--radius-sm)] border-[var(--border)] bg-[var(--surface-raised)] px-2.5 py-0 text-[11.5px] hover:border-[var(--border-strong)] hover:bg-[var(--surface-hover)]"
                title={t.a11y.workMode}
                aria-label={t.a11y.workMode}
                aria-haspopup="menu"
              >
                <span
                  className="h-1.5 w-1.5 rounded-full"
                  style={{ backgroundColor: WORK_MODE_STYLES[workMode].color }}
                />
                <span style={{ color: WORK_MODE_STYLES[workMode].color }}>
                  {WORK_MODE_STYLES[workMode].label}
                </span>
                {pendingMode && pendingMode !== workMode && (
                  <span className="inline-flex items-center gap-0.5 text-[var(--text-secondary)]">
                    <ArrowRight className="h-3 w-3" strokeWidth={1.75} />
                    {WORK_MODE_STYLES[pendingMode].label}
                  </span>
                )}
                <ChevronDown className="h-3 w-3 text-[var(--text-tertiary)]" strokeWidth={1.75} />
              </button>
              {topMenu === "mode" && (
                <div
                  role="menu"
                  className="soyo-popover-down absolute right-0 top-full z-50 mt-1 w-[180px] rounded-[var(--radius-lg)] border border-[var(--border)] bg-[var(--surface-overlay)] p-1.5 text-[12.5px] shadow-[var(--shadow-popover)]"
                >
                  {(
                    Object.keys(WORK_MODE_STYLES) as Array<
                      keyof typeof WORK_MODE_STYLES
                    >
                  ).map((mode) => (
                    <button
                      key={mode}
                      type="button"
                      onClick={() => {
                        void setWorkMode(mode);
                        setTopMenu(null);
                      }}
                      className="flex w-full items-center gap-2 rounded-[var(--radius-sm)] px-2.5 py-1.5 text-left text-[var(--text-secondary)] transition-colors hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]"
                    >
                      <span
                        className="h-1.5 w-1.5 rounded-full"
                        style={{ backgroundColor: WORK_MODE_STYLES[mode].color }}
                      />
                      <span>{WORK_MODE_STYLES[mode].label}</span>
                      {mode === workMode && (
                        <Check className="ml-auto h-3.5 w-3.5 text-[var(--success)]" strokeWidth={1.75} />
                      )}
                      {pendingMode === mode && pendingMode !== workMode && (
                        <span className="ml-auto text-[11px] text-[var(--warning)]">
                          {t.composer.queue}
                        </span>
                      )}
                    </button>
                  ))}
                </div>
              )}
            </div>

            {/* Permission pill (when not already shown via prominent bypass badge, still accessible via menu) */}
            {permissionMode !== "bypass" && (
              <div className="relative">
                <button
                  type="button"
                  onClick={() => setTopMenu(topMenu === "perm" ? null : "perm")}
                  className="soyo-pill h-[26px] rounded-[var(--radius-sm)] border-[var(--border)] bg-[var(--surface-raised)] px-2.5 py-0 text-[11.5px] text-[var(--text-secondary)] hover:border-[var(--border-strong)] hover:bg-[var(--surface-hover)]"
                  title={t.a11y.permissionMode}
                  aria-label={t.a11y.permissionMode}
                  aria-haspopup="menu"
                >
                  <span
                    className={
                      permissionMode === "manual"
                        ? "text-[var(--warning)]"
                        : "text-[var(--info)]"
                    }
                  >
                    {PERMISSION_MODE_LABELS[permissionMode]}
                  </span>
                  <ChevronDown className="h-3 w-3 text-[var(--text-tertiary)]" strokeWidth={1.75} />
                </button>
                {topMenu === "perm" && (
                  <div
                    role="menu"
                    className="soyo-popover-down absolute right-0 top-full z-50 mt-1 w-[196px] rounded-[var(--radius-lg)] border border-[var(--border)] bg-[var(--surface-overlay)] p-1.5 text-[12.5px] shadow-[var(--shadow-popover)]"
                  >
                    {(
                      ["auto", "bypass", "manual"] as Array<
                        keyof typeof PERMISSION_MODE_LABELS
                      >
                    ).map((perm) => (
                      <button
                        key={perm}
                        type="button"
                        disabled={runActive}
                        onClick={() => {
                          void setPermissionMode(perm);
                          setTopMenu(null);
                        }}
                        className={`flex w-full items-center gap-2 rounded-[var(--radius-sm)] px-2.5 py-1.5 text-left transition-colors hover:bg-[var(--surface-hover)] disabled:opacity-40 ${
                          perm === "bypass"
                            ? "text-[var(--danger)]"
                            : perm === "manual"
                              ? "text-[var(--warning)]"
                              : "text-[var(--text-secondary)]"
                        }`}
                      >
                        {perm === "bypass" && (
                          <ShieldAlert className="h-3.5 w-3.5 shrink-0" strokeWidth={1.75} />
                        )}
                        <span>{PERMISSION_MODE_LABELS[perm]}</span>
                        {perm === permissionMode && (
                          <Check className="ml-auto h-3.5 w-3.5 text-[var(--success)]" strokeWidth={1.75} />
                        )}
                      </button>
                    ))}
                  </div>
                )}
              </div>
            )}

            {permissionMode === "bypass" && topMenu === "perm" && (
              <div className="relative">
                <div
                  role="menu"
                  className="soyo-popover-down absolute right-0 top-full z-50 mt-1 w-[196px] rounded-[var(--radius-lg)] border border-[var(--border)] bg-[var(--surface-overlay)] p-1.5 text-[12.5px] shadow-[var(--shadow-popover)]"
                >
                  {(
                    ["auto", "bypass", "manual"] as Array<
                      keyof typeof PERMISSION_MODE_LABELS
                    >
                  ).map((perm) => (
                    <button
                      key={perm}
                      type="button"
                      disabled={runActive}
                      onClick={() => {
                        void setPermissionMode(perm);
                        setTopMenu(null);
                      }}
                      className={`flex w-full items-center gap-2 rounded-[var(--radius-sm)] px-2.5 py-1.5 text-left transition-colors hover:bg-[var(--surface-hover)] disabled:opacity-40 ${
                        perm === "bypass"
                          ? "text-[var(--danger)]"
                          : perm === "manual"
                            ? "text-[var(--warning)]"
                            : "text-[var(--text-secondary)]"
                      }`}
                    >
                      {perm === "bypass" && (
                        <ShieldAlert className="h-3.5 w-3.5 shrink-0" strokeWidth={1.75} />
                      )}
                      <span>{PERMISSION_MODE_LABELS[perm]}</span>
                      {perm === permissionMode && (
                        <Check className="ml-auto h-3.5 w-3.5 text-[var(--success)]" strokeWidth={1.75} />
                      )}
                    </button>
                  ))}
                </div>
              </div>
            )}

            {/* Quick Titlebar Action Icons */}
            {!isHomeEmpty && (
              <button
                type="button"
                onClick={handleToggleInspector}
                aria-label={
                  inspectorOpen ? t.a11y.closeInspector : t.a11y.openInspector
                }
                aria-pressed={inspectorOpen && !browserPaneOpen}
                title={
                  inspectorOpen ? t.a11y.closeInspector : t.a11y.openInspector
                }
                className={`flex h-[26px] w-[26px] items-center justify-center rounded-[var(--radius-sm)] transition-colors ${
                  inspectorOpen && !browserPaneOpen
                    ? "bg-[var(--surface-hover)] text-[var(--text-primary)]"
                    : "text-[var(--text-tertiary)] hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]"
                }`}
              >
                <PanelRight className="h-3.5 w-3.5" strokeWidth={1.75} />
              </button>
            )}

            <button
              type="button"
              onClick={() => setBrowserPaneOpen((v) => !v)}
              aria-label={t.topbar.browserPreview}
              aria-pressed={browserPaneOpen}
              title={t.topbar.browserPreview}
              className={`flex h-[26px] w-[26px] items-center justify-center rounded-[var(--radius-sm)] transition-colors ${
                browserPaneOpen
                  ? "bg-[var(--surface-hover)] text-[var(--accent-text)]"
                  : "text-[var(--text-tertiary)] hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]"
              }`}
            >
              <Globe className="h-3.5 w-3.5" strokeWidth={1.75} />
            </button>

            <button
              type="button"
              onClick={() => handleOpenSettings("general")}
              aria-label={t.settings.titleGeneral}
              title={t.settings.titleGeneral}
              className="flex h-[26px] w-[26px] items-center justify-center rounded-[var(--radius-sm)] text-[var(--text-tertiary)] transition-colors hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]"
            >
              <Settings className="h-3.5 w-3.5" strokeWidth={1.75} />
            </button>

            {/* Overflow menu */}
            <div className="relative">
              <button
                type="button"
                onClick={() => setTopMenu(topMenu === "more" ? null : "more")}
                aria-label={t.a11y.moreSessionActions}
                aria-haspopup="menu"
                className="flex h-[26px] w-[26px] items-center justify-center rounded-[var(--radius-sm)] text-[var(--text-secondary)] transition-colors hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]"
              >
                <MoreHorizontal className="h-4 w-4" strokeWidth={1.75} />
              </button>
              {topMenu === "more" && (
                <div
                  role="menu"
                  className="soyo-popover-down absolute right-0 top-full z-50 mt-1 w-[210px] rounded-[var(--radius-lg)] border border-[var(--border)] bg-[var(--surface-overlay)] p-1.5 text-[12.5px] shadow-[var(--shadow-popover)]"
                >
                  <button
                    type="button"
                    disabled={runActive}
                    onClick={() => {
                      setTopMenu(null);
                      setForceHome(false);
                      void startNewSession();
                    }}
                    className="flex w-full items-center gap-2 rounded-[var(--radius-sm)] px-2.5 py-1.5 text-left text-[var(--accent-text)] hover:bg-[var(--surface-hover)] disabled:opacity-40"
                  >
                    <Plus className="h-3.5 w-3.5" strokeWidth={1.75} />
                    <span>{t.topbar.newSession}</span>
                  </button>
                  <button
                    type="button"
                    onClick={() => {
                      toggleMarkdownMode();
                      setTopMenu(null);
                    }}
                    className="flex w-full items-center gap-2 rounded-[var(--radius-sm)] px-2.5 py-1.5 text-left text-[var(--text-secondary)] hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]"
                  >
                    {markdownMode === "render" ? (
                      <Code2 className="h-3.5 w-3.5 text-[var(--text-tertiary)]" strokeWidth={1.75} />
                    ) : (
                      <Eye className="h-3.5 w-3.5 text-[var(--text-tertiary)]" strokeWidth={1.75} />
                    )}
                    <span>
                      {markdownMode === "render"
                        ? t.topbar.showMarkdownSource
                        : t.topbar.renderMarkdown}
                    </span>
                  </button>
                  <button
                    type="button"
                    onClick={() => {
                      setBrowserPaneOpen((v) => !v);
                      setTopMenu(null);
                    }}
                    className="flex w-full items-center gap-2 rounded-[var(--radius-sm)] px-2.5 py-1.5 text-left text-[var(--text-secondary)] hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]"
                  >
                    <Globe className="h-3.5 w-3.5 text-[var(--text-tertiary)]" strokeWidth={1.75} />
                    <span>{t.topbar.browserPreview}</span>
                    {browserPaneOpen && (
                      <Check className="ml-auto h-3.5 w-3.5 text-[var(--success)]" strokeWidth={1.75} />
                    )}
                  </button>
                  <button
                    type="button"
                    onClick={() => {
                      handleToggleInspector();
                      setTopMenu(null);
                    }}
                    className="flex w-full items-center gap-2 rounded-[var(--radius-sm)] px-2.5 py-1.5 text-left text-[var(--text-secondary)] hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]"
                  >
                    <PanelRight className="h-3.5 w-3.5 text-[var(--text-tertiary)]" strokeWidth={1.75} />
                    <span>{t.topbar.inspector}</span>
                    {inspectorOpen && (
                      <Check className="ml-auto h-3.5 w-3.5 text-[var(--success)]" strokeWidth={1.75} />
                    )}
                  </button>
                  <div className="my-1 h-px bg-[var(--border-subtle)]" />
                  <button
                    type="button"
                    disabled={sessionId === null}
                    onClick={() => {
                      setTopMenu(null);
                      setExportOpen(true);
                    }}
                    className="flex w-full items-center gap-2 rounded-[var(--radius-sm)] px-2.5 py-1.5 text-left text-[var(--text-secondary)] hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)] disabled:opacity-40"
                  >
                    <Download className="h-3.5 w-3.5 text-[var(--text-tertiary)]" strokeWidth={1.75} />
                    <span>{t.topbar.exportSession}</span>
                  </button>
                  <button
                    type="button"
                    onClick={() => {
                      setTopMenu(null);
                      setSessionsOpen(true);
                    }}
                    className="flex w-full items-center gap-2 rounded-[var(--radius-sm)] px-2.5 py-1.5 text-left text-[var(--text-secondary)] hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]"
                  >
                    <History className="h-3.5 w-3.5 text-[var(--text-tertiary)]" strokeWidth={1.75} />
                    <span>{t.topbar.browseSessions}</span>
                  </button>
                </div>
              )}
            </div>
          </div>
        </div>
      </header>

      {/* Workspace Body */}
      <div className="relative flex min-h-0 flex-1 overflow-hidden">
        {/* Single Collapsible Sidebar (248px expanded / 48px rail collapsed) */}
        <aside
          data-testid="sidebar-panel"
          className={`relative z-20 flex shrink-0 flex-col border-r border-[var(--border-subtle)] bg-[var(--surface-sidebar)] transition-[width] duration-150 ${
            sidebarCollapsed ? "w-[48px] items-center py-2.5" : "w-[248px] p-2.5"
          }`}
        >
          {/* Top Navigation */}
          <nav
            aria-label={t.a11y.sidebarNav}
            className={
              sidebarCollapsed
                ? "flex flex-col items-center gap-1"
                : "flex flex-col gap-0.5"
            }
          >
            {sidebarNavItems.map((item) => {
              const Icon = item.icon;
              if (sidebarCollapsed) {
                return (
                  <button
                    key={item.id}
                    type="button"
                    disabled={item.disabled}
                    onClick={item.onClick}
                    title={item.label}
                    aria-label={item.label}
                    className="flex h-9 w-9 items-center justify-center rounded-[var(--radius-md)] text-[var(--text-secondary)] transition-colors hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)] disabled:opacity-40"
                  >
                    <Icon className="h-4 w-4" strokeWidth={1.75} />
                  </button>
                );
              }
              return (
                <button
                  key={item.id}
                  type="button"
                  disabled={item.disabled}
                  onClick={item.onClick}
                  className={`flex items-center gap-2.5 rounded-[var(--radius-md)] px-2.5 py-1.5 text-left text-[13px] transition-colors hover:bg-[var(--surface-hover)] disabled:opacity-40 ${
                    item.id === "new"
                      ? "font-medium text-[var(--accent-text)]"
                      : "text-[var(--text-secondary)] hover:text-[var(--text-primary)]"
                  }`}
                >
                  <Icon className="h-3.5 w-3.5 shrink-0" strokeWidth={1.75} />
                  <span className="truncate">{item.label}</span>
                  {item.shortcut && (
                    <span className="ml-auto text-[11px] text-[var(--text-tertiary)]">
                      {item.shortcut}
                    </span>
                  )}
                </button>
              );
            })}
          </nav>

          {/* Recents List (expanded sidebar only, max 5/project max 30 total) */}
          {!sidebarCollapsed && (
            <div className="mt-3 flex min-h-0 flex-1 flex-col">
              <div className="flex items-center justify-between px-2 pb-1 text-[11px] font-medium uppercase tracking-wider text-[var(--text-tertiary)]">
                <span>{t.extra.recentsTitle}</span>
                <button
                  type="button"
                  onClick={() => setSessionsOpen(true)}
                  aria-label={t.topbar.browseSessions}
                  className="rounded-[var(--radius-sm)] p-0.5 text-[var(--text-tertiary)] hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]"
                >
                  <MoreHorizontal className="h-3.5 w-3.5" strokeWidth={1.75} />
                </button>
              </div>

              <div className="flex-1 space-y-2.5 overflow-y-auto pr-0.5 text-xs">
                {recentGroups.length === 0 ? (
                  <div className="px-2 py-2 text-[11px] text-[var(--text-tertiary)]">
                    {t.sidebar.noRecentSessions}
                  </div>
                ) : (
                  recentGroups.map(([proj, items]) => (
                    <div key={proj} className="space-y-0.5">
                      <div className="flex items-center justify-between px-2 py-1 text-[11px] text-[var(--text-tertiary)]">
                        <span className="inline-flex items-center gap-1.5 truncate">
                          <Folder className="h-3 w-3 shrink-0" strokeWidth={1.75} />
                          <span className="truncate">{proj}</span>
                        </span>
                        <span>{items.length}</span>
                      </div>
                      {items.map((item) => {
                        const isCurrent = item.id === sessionId;
                        return (
                          <button
                            key={item.id}
                            type="button"
                            disabled={runActive}
                            onClick={() => {
                              setForceHome(false);
                              void resumeSession(item.id, false);
                            }}
                            className={`flex w-full flex-col gap-0.5 rounded-[var(--radius-md)] px-2.5 py-1.5 text-left transition-colors disabled:opacity-50 ${
                              isCurrent
                                ? "bg-[var(--surface-raised)] text-[var(--text-primary)]"
                                : "text-[var(--text-secondary)] hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]"
                            }`}
                          >
                            <span className="truncate text-[12.5px] font-medium">
                              {item.title ?? t.sidebar.untitledSession}
                            </span>
                            <span className="truncate text-[11px] text-[var(--text-tertiary)]">
                              {item.work_mode} · {item.last_status ?? t.sidebar.savedStatus}
                            </span>
                          </button>
                        );
                      })}
                    </div>
                  ))
                )}
              </div>

              <div className="mt-1 flex items-center justify-between px-2 pt-1">
                <button
                  type="button"
                  onClick={() => setSessionsOpen(true)}
                  className="text-left text-[12px] text-[var(--text-tertiary)] transition-colors hover:text-[var(--text-primary)]"
                >
                  {t.sidebar.viewAll}
                </button>
                {recentsOmittedCount > 0 && (
                  <button
                    type="button"
                    onClick={() => setSessionsOpen(true)}
                    className="text-[11px] text-[var(--accent-text)] hover:underline"
                  >
                    {t.extra.moreCount(recentsOmittedCount)}
                  </button>
                )}
              </div>
            </div>
          )}

          {/* Bottom User Row & User Menu */}
          <div
            ref={userMenuRef}
            className={`relative mt-auto ${sidebarCollapsed ? "" : "pt-2 border-t border-[var(--border-subtle)]"}`}
          >
            <button
              type="button"
              onClick={() => setUserMenuOpen((prev) => !prev)}
              title={t.a11y.userMenuTitle(bootstrap?.selected_profile ?? "default")}
              aria-label={t.a11y.userMenu}
              className={
                sidebarCollapsed
                  ? "flex h-9 w-9 items-center justify-center rounded-[var(--radius-md)] transition-colors hover:bg-[var(--surface-hover)]"
                  : "flex w-full items-center gap-2.5 rounded-[var(--radius-md)] px-2 py-1.5 text-left transition-colors hover:bg-[var(--surface-hover)]"
              }
            >
              <img
                src={avatarData ?? "/app-icon.png"}
                alt={displayName ?? t.about.productName}
                className="h-6 w-6 shrink-0 rounded-[22%] object-cover ring-1 ring-[var(--border)]"
              />
              {!sidebarCollapsed && (
                <>
                  <div className="min-w-0 flex-1">
                    <div className="truncate text-[12.5px] font-medium text-[var(--text-primary)]">
                      {displayName ?? "soyo-desktop"}
                    </div>
                    <div className="truncate font-mono text-[11px] text-[var(--text-tertiary)]">
                      {bootstrap?.selected_profile ?? "default"}
                    </div>
                  </div>
                  <ChevronsUpDown
                    className="h-3.5 w-3.5 shrink-0 text-[var(--text-tertiary)]"
                    strokeWidth={1.75}
                  />
                </>
              )}
            </button>

            {userMenuOpen && (
              <div
                data-testid="user-menu-popover"
                className={`soyo-popover-up absolute z-50 w-[232px] rounded-[var(--radius-lg)] border border-[var(--border)] bg-[var(--surface-overlay)] p-1.5 text-[12.5px] shadow-[var(--shadow-popover)] ${
                  sidebarCollapsed
                    ? "bottom-0 left-[calc(100%+10px)] origin-bottom-left"
                    : "bottom-[calc(100%+6px)] left-0 origin-bottom"
                }`}
              >
                <div className="px-2.5 pb-1.5 pt-1 text-[11px] text-[var(--text-tertiary)]">
                  <div className="font-medium text-[var(--text-primary)]">
                    {displayName ?? "soyo-desktop"}
                  </div>
                  <div className="truncate font-mono text-[11px]">
                    {bootstrap?.selected_profile ?? "default"}
                  </div>
                </div>

                {/* Theme selector inline (§8.11/§8.12) */}
                <div className="px-2 py-1.5">
                  <div className="mb-1 flex items-center gap-1.5 text-[11px] text-[var(--text-tertiary)]">
                    <SunMoon className="h-3 w-3" strokeWidth={1.75} />
                    <span>{t.extra.themeAction}</span>
                  </div>
                  <div className="grid grid-cols-3 gap-1">
                    {(
                      [
                        ["system", t.appearance.themeSystem],
                        ["light", t.appearance.themeLight],
                        ["dark", t.appearance.themeDark],
                      ] as [ThemePreference, string][]
                    ).map(([opt, label]) => (
                      <button
                        key={opt}
                        type="button"
                        onClick={() => void setTheme(opt)}
                        className={`rounded-[var(--radius-sm)] px-1.5 py-1 text-[11px] transition-colors ${
                          theme === opt
                            ? "bg-[var(--accent)] font-semibold text-[var(--on-accent)]"
                            : "bg-[var(--surface-raised)] text-[var(--text-secondary)] hover:bg-[var(--surface-hover)]"
                        }`}
                      >
                        {label}
                      </button>
                    ))}
                  </div>
                </div>

                <div className="my-1 h-px bg-[var(--border-subtle)]" />

                <button
                  type="button"
                  data-testid="user-menu-language"
                  onClick={() => {
                    setUserMenuOpen(false);
                    setLanguageOpen(true);
                  }}
                  className="flex w-full items-center gap-2 rounded-[var(--radius-sm)] px-2.5 py-1.5 text-left text-[var(--text-secondary)] transition-colors hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]"
                >
                  <Globe className="h-3.5 w-3.5 text-[var(--text-tertiary)]" strokeWidth={1.75} />
                  <span>{t.userMenu.language}</span>
                </button>
                <button
                  type="button"
                  onClick={() => handleOpenSettings("general")}
                  className="flex w-full items-center gap-2 rounded-[var(--radius-sm)] px-2.5 py-1.5 text-left text-[var(--text-secondary)] transition-colors hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]"
                >
                  <Settings className="h-3.5 w-3.5 text-[var(--text-tertiary)]" strokeWidth={1.75} />
                  <span>{t.userMenu.settings}</span>
                  <span className="ml-auto text-[11px] text-[var(--text-tertiary)]">
                    ⌘,
                  </span>
                </button>
                <button
                  type="button"
                  onClick={() => handleOpenSettings("usage")}
                  className="flex w-full items-center gap-2 rounded-[var(--radius-sm)] px-2.5 py-1.5 text-left text-[var(--text-secondary)] transition-colors hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]"
                >
                  <SlidersHorizontal className="h-3.5 w-3.5 text-[var(--text-tertiary)]" strokeWidth={1.75} />
                  <span>{t.userMenu.usage}</span>
                </button>
                <button
                  type="button"
                  onClick={() => handleOpenSettings("general")}
                  className="flex w-full items-center gap-2 rounded-[var(--radius-sm)] px-2.5 py-1.5 text-left text-[var(--text-secondary)] transition-colors hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]"
                >
                  <Bookmark className="h-3.5 w-3.5 text-[var(--text-tertiary)]" strokeWidth={1.75} />
                  <span>{t.extra.aboutAction}</span>
                </button>
                <div className="my-1 h-px bg-[var(--border-subtle)]" />
                <button
                  type="button"
                  onClick={() => setUserMenuOpen(false)}
                  className="flex w-full items-center gap-2 rounded-[var(--radius-sm)] px-2.5 py-1.5 text-left text-[var(--text-tertiary)] transition-colors hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]"
                >
                  <X className="h-3.5 w-3.5" strokeWidth={1.75} />
                  <span>{t.userMenu.close}</span>
                </button>
              </div>
            )}
          </div>
        </aside>

        {/* Main Content Surface */}
        <div className="flex min-w-0 flex-1 flex-col bg-[var(--surface-main)]">
          <UpdaterBanner />

          {helperReconnecting && (
            <div
              data-testid="helper-reconnect-banner"
              className="flex items-center justify-between gap-2 border-b border-[var(--warning)] bg-[var(--warning-surface)] px-4 py-1.5 text-xs text-[var(--warning)]"
            >
              <span className="inline-flex items-center gap-2">
                <SquaresLoader size={12} />
                <span>{t.extra.reconnectBanner}</span>
              </span>
              <Button
                variant="secondary"
                size="sm"
                onClick={() => void reconnectHelper()}
              >
                {t.extra.reconnectBtn}
              </Button>
            </div>
          )}

          {error && (
            <div className="flex items-center justify-between gap-2 border-b border-[var(--danger)] bg-[var(--danger-surface)] px-4 py-2 text-xs text-[var(--danger)]">
              <span>
                <strong>{error.code}:</strong> {error.message}
              </span>
              <button
                type="button"
                onClick={clearError}
                aria-label={t.a11y.dismissError}
                className="rounded p-0.5 hover:bg-[var(--surface-hover)]"
              >
                <X className="h-3.5 w-3.5" strokeWidth={1.75} />
              </button>
            </div>
          )}

          {bootstrap && !bootstrap.config_ready && (
            <div className="border-b border-[var(--warning)] bg-[var(--warning-surface)] px-4 py-2 text-xs text-[var(--warning)]">
              {t.home.configIncomplete}
              {bootstrap.config_path ? ` (${bootstrap.config_path})` : ""}:{" "}
              {bootstrap.config_error ?? t.home.configFallback}
            </div>
          )}

          {recoveryBanner && (
            <div className="border-b border-[var(--border-subtle)] bg-[var(--surface-raised)] px-4 py-1.5 text-xs text-[var(--info)]">
              {recoveryBanner}
            </div>
          )}

          {/* Split Area */}
          <div className="flex min-h-0 flex-1 overflow-hidden">
            <div className="flex min-w-0 flex-1 flex-col">
              {isHomeEmpty ? (
                /* Home Empty State (§8) */
                <div
                  data-testid="home-empty-state"
                  data-tauri-drag-region
                  className="soyo-panel-enter flex flex-1 flex-col items-center justify-center overflow-y-auto px-6 pb-10"
                >
                  <div className="mb-8 flex items-center gap-3 text-[var(--text-primary)]">
                    <img
                      src="/app-icon.png"
                      alt={t.about.productName}
                      className="h-[30px] w-[30px] rounded-[22%] object-cover"
                    />
                    <h1 className="soyo-font-display text-[28px] leading-[36px] tracking-tight">
                      {greeting}
                    </h1>
                  </div>

                  <Composer
                    variant="home"
                    disabled={!bootstrap?.config_ready && !previewMode}
                    runActive={runActive}
                    cancelling={cancelling}
                    hasSession={sessionId !== null}
                    approvalPending={Boolean(pendingApproval)}
                    workMode={workMode}
                    permissionMode={permissionMode}
                    selectedProfile={bootstrap?.selected_profile}
                    selectedModel={bootstrap?.selected_model}
                    profiles={bootstrap?.profiles ?? []}
                    thinkingEffort={bootstrap?.thinking_effort}
                    ultracodeEnabled={ultracodeEnabled}
                    submitMode={submitMode}
                    stagedAttachments={stagedAttachments}
                    forcePopover={forceComposerPopover}
                    onSubmit={async (prompt) => {
                      setForceHome(false);
                      return submitPrompt(prompt);
                    }}
                    onCancel={cancelActiveRun}
                    onCompact={compactCurrentSession}
                    onCycleMode={cycleWorkMode}
                    onSelectProfile={selectProfile}
                    onSelectEffort={(eff, ultra) => setThinkingEffort(eff, ultra)}
                    onToggleSubmitMode={() =>
                      setSubmitMode(submitMode === "steer" ? "queue" : "steer")
                    }
                    onStageAttachments={stageAttachments}
                    onPasteAttachments={stagePastedAttachments}
                    onRemoveAttachment={removeStagedAttachment}
                    onOpenSettings={() => handleOpenSettings("general")}
                    onOpenPermissions={() => handleOpenSettings("permissions")}
                    onOpenSessions={() => setSessionsOpen(true)}
                    onOpenMemory={() => handleOpenSettings("memory")}
                    onOpenMcp={() => handleOpenSettings("mcp")}
                    onOpenExport={() => setExportOpen(true)}
                  />

                  <button
                    type="button"
                    onClick={() => handleOpenSettings("general")}
                    className="-mt-px flex w-full max-w-[592px] items-center justify-between gap-2 rounded-b-[var(--radius-lg)] border border-t-0 border-[var(--border)] bg-[var(--surface-raised)] px-3.5 py-2 text-[12px] text-[var(--text-secondary)] transition-colors hover:border-[var(--border-strong)] hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]"
                  >
                    <span className="inline-flex min-w-0 items-center gap-1.5">
                      <Folder className="h-3.5 w-3.5 shrink-0 text-[var(--text-tertiary)]" strokeWidth={1.75} />
                      <span className="truncate font-medium">
                        {currentProject || t.home.projectOrFolder}
                      </span>
                    </span>
                    {bootstrap?.workspace && (
                      <span className="max-w-[300px] truncate font-mono text-[11px] text-[var(--text-tertiary)]">
                        {bootstrap.workspace}
                      </span>
                    )}
                  </button>

                  <div className="mt-8 w-full max-w-[620px] space-y-0.5">
                    <div className="px-3 py-1 text-[11.5px] text-[var(--text-tertiary)]">
                      {t.home.ideasForYou}
                    </div>
                    {[
                      {
                        icon: Clock,
                        text: t.home.ideaSummarize,
                        action: () =>
                          sessionId ? setExportOpen(true) : setSessionsOpen(true),
                      },
                      {
                        icon: Box,
                        text: t.home.ideaAuditMcp,
                        action: () => handleOpenSettings("mcp"),
                      },
                      {
                        icon: SlidersHorizontal,
                        text: t.home.ideaCustomize,
                        action: () => handleOpenSettings("appearance"),
                      },
                    ].map((idea) => {
                      const Icon = idea.icon;
                      return (
                        <button
                          key={idea.text}
                          type="button"
                          onClick={idea.action}
                          className="group flex w-full items-center gap-3 rounded-[var(--radius-md)] px-3 py-2 text-left text-[13px] text-[var(--text-secondary)] transition-colors hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]"
                        >
                          <span className="flex h-[26px] w-[26px] shrink-0 items-center justify-center rounded-[var(--radius-sm)] border border-[var(--border)] bg-[var(--surface-raised)] text-[var(--text-tertiary)] transition-colors group-hover:border-[var(--border-strong)] group-hover:text-[var(--accent-text)]">
                            <Icon className="h-3.5 w-3.5" strokeWidth={1.75} />
                          </span>
                          <span>{idea.text}</span>
                        </button>
                      );
                    })}
                  </div>
                </div>
              ) : (
                /* Conversation Stream */
                <div
                  ref={scrollRef}
                  className="soyo-conv-prose flex-1 overflow-y-auto px-6 pb-3 pt-4"
                >
                  <div className="mx-auto flex max-w-[840px] flex-col gap-3.5">
                    {historyOmitted && (
                      <div className="rounded-[var(--radius-md)] border border-[var(--border-subtle)] bg-[var(--surface-raised)] px-3 py-1.5 text-[11.5px] text-[var(--text-tertiary)]">
                        {t.home.historyOmitted}
                      </div>
                    )}

                    {loading && messages.length === 0 && (
                      <div className="flex items-center justify-center gap-2 py-16 text-xs text-[var(--text-tertiary)]">
                        <SquaresLoader size={12} />
                        <span>{t.home.loadingRuntime}</span>
                      </div>
                    )}

                    {messages.map((msg) => {
                      if (msg.role === "user") {
                        return (
                          <div
                            key={msg.id}
                            className="soyo-msg-enter group flex max-w-[640px] flex-col items-end self-end"
                          >
                            <div className="rounded-[var(--radius-lg)] border border-[var(--border)] bg-[var(--surface-raised)] px-3.5 py-2 text-[var(--text-primary)]">
                              <pre className="whitespace-pre-wrap font-sans">
                                {msg.text}
                              </pre>
                            </div>
                            <div className="mt-1 flex items-center gap-2.5 px-2 text-[11px] text-[var(--text-tertiary)] opacity-0 transition-opacity duration-150 group-hover:opacity-100">
                              <span>{msg.label}</span>
                              <button
                                type="button"
                                disabled
                                title={t.home.branchUnsupported}
                                className="opacity-40"
                              >
                                <GitBranch className="h-3 w-3" strokeWidth={1.75} />
                              </button>
                              <button
                                type="button"
                                onClick={() => void submitPrompt(msg.text)}
                                disabled={runActive}
                                title={t.home.retryPrompt}
                                className="hover:text-[var(--text-primary)] disabled:opacity-40"
                              >
                                <RotateCw className="h-3 w-3" strokeWidth={1.75} />
                              </button>
                              <button
                                type="button"
                                onClick={() =>
                                  setEditDraft({
                                    text: msg.text,
                                    nonce: Date.now(),
                                  })
                                }
                                title={t.home.editMessage}
                                className="hover:text-[var(--text-primary)]"
                              >
                                <Pencil className="h-3 w-3" strokeWidth={1.75} />
                              </button>
                              <button
                                type="button"
                                onClick={() =>
                                  void handleCopyMessage(msg.id, msg.text)
                                }
                                title={t.home.copyMessage}
                                className="inline-flex items-center gap-1 hover:text-[var(--text-primary)]"
                              >
                                <Copy className="h-3 w-3" strokeWidth={1.75} />
                                {copiedId === msg.id && <span>{t.home.copied}</span>}
                              </button>
                              <button
                                type="button"
                                onClick={() => setExportOpen(true)}
                                title={t.home.exportShare}
                                className="hover:text-[var(--text-primary)]"
                              >
                                <Share2 className="h-3 w-3" strokeWidth={1.75} />
                              </button>
                            </div>
                          </div>
                        );
                      }

                      if (msg.role === "tool") {
                        return (
                          <ToolCardView
                            key={msg.id}
                            message={msg}
                            sessionId={sessionId}
                          />
                        );
                      }

                      if (msg.role === "run_summary") {
                        return (
                          <div
                            key={msg.id}
                            className="flex items-center justify-between px-1 py-0.5 text-[11.5px] text-[var(--text-tertiary)]"
                          >
                            <span className="font-mono uppercase text-[var(--text-secondary)]">
                              {msg.label}
                            </span>
                            <span className="font-mono">{msg.text}</span>
                          </div>
                        );
                      }

                      if (msg.role === "system") {
                        return (
                          <div
                            key={msg.id}
                            className="px-3 py-0.5 text-[12px] text-[var(--text-tertiary)]"
                          >
                            {msg.text}
                          </div>
                        );
                      }

                      /* Assistant replies render unbubbled directly on --surface-main with hover actions (§8.12) */
                      return (
                        <article key={msg.id} className="soyo-msg-enter group py-0.5">
                          <MarkdownRenderer
                            content={msg.text}
                            mode={markdownMode}
                            streaming={msg.streaming}
                          />
                          {!msg.streaming && msg.text.trim().length > 0 && (
                            <div className="mt-1 flex items-center gap-2.5 text-[11px] text-[var(--text-tertiary)] opacity-0 transition-opacity duration-150 group-hover:opacity-100">
                              <button
                                type="button"
                                onClick={() =>
                                  void handleCopyMessage(msg.id, msg.text)
                                }
                                title={t.home.copyMessage}
                                className="inline-flex items-center gap-1 hover:text-[var(--text-primary)]"
                              >
                                <Copy className="h-3 w-3" strokeWidth={1.75} />
                                {copiedId === msg.id ? t.home.copied : t.a11y.copyMessage}
                              </button>
                            </div>
                          )}
                        </article>
                      );
                    })}

                    {thinkingEntries.length > 0 && (
                      <ThinkingCardView entries={thinkingEntries} />
                    )}

                    {pendingApproval && (
                      <ApprovalDialog
                        request={pendingApproval}
                        onReply={replyApproval}
                      />
                    )}

                    {(runActive || summaryLine) && (
                      <div className="flex flex-wrap items-center gap-3.5 px-1 pt-1 font-mono text-[11px] tabular-nums text-[var(--text-tertiary)]">
                        {runActive ? (
                          <span
                            data-testid="run-working-indicator"
                            className="inline-flex items-center gap-1.5 text-[var(--success)]"
                          >
                            <SquaresLoader size={12} />
                            <span>{t.home.streaming}</span>
                          </span>
                        ) : null}
                        {summaryLine && <span>{summaryLine}</span>}
                        {lastUsage && (
                          <span
                            className="inline-flex items-center gap-1.5"
                            title={`${
                              lastUsage.estimated
                                ? t.settings.usageEstimated
                                : t.settings.usageSubtitle
                            }${
                              lastUsage.cached_input_tokens
                                ? ` · ${t.settings.cacheReadLower} ${formatTokenCount(lastUsage.cached_input_tokens)}`
                                : ""
                            }`}
                          >
                            <span>{lastUsage.estimated ? "~" : ""}</span>
                            <span className="inline-flex items-center gap-0.5">
                              <ArrowUp className="h-3 w-3" strokeWidth={1.75} />
                              {formatTokenCount(lastUsage.input_tokens)}
                            </span>
                            <span className="inline-flex items-center gap-0.5">
                              <ArrowDown className="h-3 w-3" strokeWidth={1.75} />
                              {formatTokenCount(lastUsage.output_tokens)}
                            </span>
                          </span>
                        )}
                      </div>
                    )}
                  </div>
                </div>
              )}

              {!isHomeEmpty && (
                <Composer
                  variant="docked"
                  disabled={!bootstrap?.config_ready && !previewMode}
                  runActive={runActive}
                  cancelling={cancelling}
                  hasSession={sessionId !== null}
                  approvalPending={Boolean(pendingApproval)}
                  workMode={workMode}
                  permissionMode={permissionMode}
                  selectedProfile={bootstrap?.selected_profile}
                  selectedModel={bootstrap?.selected_model}
                  profiles={bootstrap?.profiles ?? []}
                  thinkingEffort={bootstrap?.thinking_effort}
                  ultracodeEnabled={ultracodeEnabled}
                  submitMode={submitMode}
                  queuedPromptPreview={queuedPrompt?.prompt ?? null}
                  compactionPending={compactionPending}
                  contextUsedTokens={contextUsedTokens}
                  contextWindowTokens={contextWindowTokens}
                  contextEstimated={composerContextEstimated}
                  stagedAttachments={stagedAttachments}
                  forcePopover={forceComposerPopover}
                  draftSeed={editDraft}
                  onSubmit={(prompt) => submitPrompt(prompt)}
                  onCancel={cancelActiveRun}
                  onCompact={compactCurrentSession}
                  onCycleMode={cycleWorkMode}
                  onSelectProfile={selectProfile}
                  onSelectEffort={(eff, ultra) => setThinkingEffort(eff, ultra)}
                  onToggleSubmitMode={() =>
                    setSubmitMode(submitMode === "steer" ? "queue" : "steer")
                  }
                  onClearQueuedPrompt={clearQueuedPrompt}
                  onStageAttachments={stageAttachments}
                  onPasteAttachments={stagePastedAttachments}
                  onRemoveAttachment={removeStagedAttachment}
                  onOpenSettings={() => handleOpenSettings("general")}
                  onOpenPermissions={() => handleOpenSettings("permissions")}
                  onOpenSessions={() => setSessionsOpen(true)}
                  onOpenMemory={() => handleOpenSettings("memory")}
                  onOpenMcp={() => handleOpenSettings("mcp")}
                  onOpenExport={() => setExportOpen(true)}
                />
              )}
            </div>

            {/* Split Browser Preview Pane */}
            {browserPaneOpen && (
              <section
                data-testid="split-browser-pane"
                className="soyo-panel-enter flex w-[44%] min-w-[320px] flex-col border-l border-[var(--border-subtle)] bg-[var(--surface-sidebar)]"
              >
                <div className="flex h-[44px] shrink-0 items-center gap-1.5 border-b border-[var(--border-subtle)] px-3 text-[var(--text-tertiary)]">
                  <button
                    type="button"
                    onClick={() => setBrowserMiniTab("new")}
                    title={t.a11y.browserBack}
                    aria-label={t.a11y.browserBack}
                    className="flex h-6 w-6 items-center justify-center rounded-[var(--radius-sm)] transition-colors hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]"
                  >
                    <ArrowLeft className="h-3.5 w-3.5" strokeWidth={1.75} />
                  </button>
                  <button
                    type="button"
                    onClick={() => setBrowserMiniTab("sessions")}
                    title={t.a11y.browserForward}
                    aria-label={t.a11y.browserForward}
                    className="flex h-6 w-6 items-center justify-center rounded-[var(--radius-sm)] transition-colors hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]"
                  >
                    <ArrowRight className="h-3.5 w-3.5" strokeWidth={1.75} />
                  </button>
                  <button
                    type="button"
                    onClick={() => setBrowserRefreshTick((tick) => tick + 1)}
                    title={t.a11y.reloadPreview}
                    aria-label={t.a11y.reloadPreview}
                    className="flex h-6 w-6 items-center justify-center rounded-[var(--radius-sm)] transition-colors hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]"
                  >
                    <RotateCw className="h-3.5 w-3.5" strokeWidth={1.75} />
                  </button>
                  <div className="flex flex-1 items-center justify-center gap-1.5 rounded-[var(--radius-md)] border border-[var(--border)] bg-[var(--surface-raised)] px-2.5 py-1 text-[11.5px] text-[var(--text-secondary)]">
                    <Lock className="h-3 w-3 text-[var(--text-tertiary)]" strokeWidth={1.75} />
                    <span className="truncate font-mono text-[11px]">
                      localhost:1420
                    </span>
                  </div>
                  <button
                    type="button"
                    onClick={() => handleOpenSettings("general")}
                    title={t.a11y.openExternalSettings}
                    aria-label={t.a11y.openExternalSettings}
                    className="flex h-6 w-6 items-center justify-center rounded-[var(--radius-sm)] transition-colors hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]"
                  >
                    <ExternalLink className="h-3.5 w-3.5" strokeWidth={1.75} />
                  </button>
                  <button
                    type="button"
                    onClick={() => setBrowserPaneOpen(false)}
                    aria-label={t.a11y.closeSplitBrowser}
                    className="flex h-6 w-6 items-center justify-center rounded-[var(--radius-sm)] transition-colors hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]"
                  >
                    <X className="h-3.5 w-3.5" strokeWidth={1.75} />
                  </button>
                </div>
                <div
                  key={browserRefreshTick}
                  className="soyo-panel-enter flex min-h-0 flex-1 bg-[var(--surface-main)] text-[11px]"
                >
                  <div className="flex w-[118px] shrink-0 flex-col gap-0.5 border-r border-[var(--border-subtle)] bg-[var(--surface-sidebar)] p-2">
                    {(
                      [
                        ["new", t.home.browserNew],
                        ["search", t.sidebar.searchTitle],
                        ["sessions", t.sidebar.sessionsTitle],
                        ["memory", t.sidebar.memoryTitle],
                        ["customize", t.sidebar.customizeTitle],
                      ] as const
                    ).map(([id, label]) => (
                      <button
                        key={id}
                        type="button"
                        onClick={() => setBrowserMiniTab(id)}
                        className={`rounded-[var(--radius-sm)] px-2 py-1 text-left text-[11px] transition-colors ${
                          browserMiniTab === id
                            ? "bg-[var(--surface-raised)] text-[var(--text-primary)]"
                            : "text-[var(--text-secondary)] hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]"
                        }`}
                      >
                        {label}
                      </button>
                    ))}
                  </div>
                  <div className="flex min-w-0 flex-1 flex-col items-center justify-center gap-3.5 p-5 text-center">
                    <div className="flex items-center gap-2 text-[var(--text-primary)]">
                      <img
                        src="/app-icon.png"
                        alt={t.about.productName}
                        className="h-[18px] w-[18px] rounded-[22%] object-cover"
                      />
                      <span className="soyo-font-display text-[17px]">{greeting}</span>
                    </div>
                    <div className="w-full max-w-[320px] rounded-[var(--radius-lg)] border border-[var(--border)] bg-[var(--surface-composer)] p-2.5 text-left text-[11px] text-[var(--text-tertiary)]">
                      <div>{t.composer.placeholderHome}</div>
                      <div className="mt-2.5 flex items-center justify-between">
                        <span className="rounded-[var(--radius-xs)] bg-[var(--surface-hover)] px-1.5 py-0.5 text-[11px] text-[var(--text-secondary)]">
                          +
                        </span>
                        <div className="flex items-center gap-1.5">
                          <span className="rounded-[var(--radius-xs)] bg-[var(--surface-hover)] px-1.5 py-0.5 text-[11px] text-[var(--text-secondary)]">
                            {bootstrap?.selected_model ?? "claude-sonnet-4-5"}
                          </span>
                          <span className="flex h-[18px] w-[18px] items-center justify-center rounded-full bg-[var(--accent)] text-[var(--on-accent)]">
                            <ArrowUp className="h-2.5 w-2.5" strokeWidth={2} />
                          </span>
                        </div>
                      </div>
                    </div>
                    <div className="flex w-full max-w-[320px] flex-col gap-1 text-left">
                      <div className="px-1.5 py-0.5 text-[11px] text-[var(--text-tertiary)]">
                        {t.home.ideasForYou}
                      </div>
                      <div className="rounded-[var(--radius-sm)] bg-[var(--surface-raised)] px-2 py-1.5 text-[11px] text-[var(--text-secondary)]">
                        {t.home.ideaSummarize}
                      </div>
                      <div className="rounded-[var(--radius-sm)] bg-[var(--surface-raised)] px-2 py-1.5 text-[11px] text-[var(--text-secondary)]">
                        {t.home.ideaAuditMcp}
                      </div>
                    </div>
                  </div>
                </div>
              </section>
            )}
          </div>
        </div>

        {/* Right Inspector: docked >=1280px, overlay drawer 1024-1279px & <1024px */}
        {inspectorOpen && !isHomeEmpty && !browserPaneOpen && (
          isWideViewport ? (
            <ContextSidebar
              bootstrap={bootstrap}
              workMode={workMode}
              contextEntries={contextEntries}
              contextSkillsCount={contextSkillsCount}
              contextEstimated={composerContextEstimated}
              contextUsedTokens={contextUsedTokens}
              todos={todos}
              mcpServers={mcpServers}
              recentSessions={recentSessions}
              activeSessionId={sessionId}
              messages={messages}
            />
          ) : (
            <div
              data-testid="inspector-drawer-backdrop"
              onClick={() => setDrawerInspectorOpen(false)}
              className="fixed inset-x-0 bottom-0 top-[38px] z-40 flex justify-end bg-[var(--scrim)]"
            >
              <div
                onClick={(e) => e.stopPropagation()}
                className="flex h-full shadow-[var(--shadow-dialog)]"
              >
                <ContextSidebar
                  bootstrap={bootstrap}
                  workMode={workMode}
                  contextEntries={contextEntries}
                  contextSkillsCount={contextSkillsCount}
                  contextEstimated={composerContextEstimated}
                  contextUsedTokens={contextUsedTokens}
                  todos={todos}
                  mcpServers={mcpServers}
                  recentSessions={recentSessions}
                  activeSessionId={sessionId}
                  messages={messages}
                />
              </div>
            </div>
          )
        )}
      </div>

      {/* Search Overlay Modal (⌘K) */}
      <DialogShell
        open={searchOverlayOpen}
        onClose={() => setSearchOverlayOpen(false)}
        ariaLabel={t.extra.searchOverlayTitle}
        testId="search-overlay-modal"
        size="md"
        panelClassName="flex max-h-[75vh] flex-col"
      >
        <div className="flex items-center gap-2 border-b border-[var(--border-subtle)] px-4 py-3">
          <Search className="h-4 w-4 text-[var(--text-tertiary)]" strokeWidth={1.75} />
          <input
            type="text"
            autoFocus
            value={sessionQuery}
            onChange={(e) => setSessionQuery(e.target.value)}
            placeholder={t.extra.searchOverlayPlaceholder}
            aria-label={t.a11y.searchSessions}
            className="flex-1 bg-transparent text-[13px] text-[var(--text-primary)] placeholder:text-[var(--text-tertiary)] focus:outline-none"
          />
          <button
            type="button"
            onClick={() => setSearchOverlayOpen(false)}
            aria-label={t.a11y.searchOverlayClose}
            className="rounded-[var(--radius-sm)] p-1 text-[var(--text-tertiary)] hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]"
          >
            <X className="h-4 w-4" strokeWidth={1.75} />
          </button>
        </div>

        <div className="flex-1 overflow-y-auto p-3">
          {filteredSessions.length === 0 ? (
            <div className="py-8 text-center text-xs text-[var(--text-tertiary)]">
              {t.extra.searchOverlayEmpty}
            </div>
          ) : (
            <ul className="space-y-1">
              {filteredSessions.map((item) => (
                <li key={item.id}>
                  <button
                    type="button"
                    disabled={runActive}
                    onClick={() => {
                      setSearchOverlayOpen(false);
                      setForceHome(false);
                      void resumeSession(item.id, false);
                    }}
                    className={`flex w-full items-center justify-between gap-3 rounded-[var(--radius-md)] px-3 py-2 text-left text-xs transition-colors hover:bg-[var(--surface-hover)] disabled:opacity-50 ${
                      item.id === sessionId
                        ? "bg-[var(--surface-raised)] text-[var(--text-primary)]"
                        : "text-[var(--text-secondary)]"
                    }`}
                  >
                    <div className="min-w-0">
                      <div className="truncate font-medium text-[var(--text-primary)]">
                        {sanitizeDisplayText(
                          item.title ?? t.sidebar.untitledSession
                        )}
                      </div>
                      <div className="truncate text-[11px] text-[var(--text-tertiary)]">
                        {workspaceBase(
                          item.workspace,
                          t.sessionBrowser.workspaceFallback
                        )}{" "}
                        · {item.work_mode} ·{" "}
                        {item.last_status ?? t.sidebar.savedStatus}
                      </div>
                    </div>
                    <span
                      style={{ color: WORK_MODE_STYLES[item.work_mode].color }}
                      className="shrink-0 font-mono text-[11px]"
                    >
                      {WORK_MODE_STYLES[item.work_mode].label}
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>

        <div className="flex items-center justify-between border-t border-[var(--border-subtle)] px-4 py-2.5 text-xs">
          <span className="text-[11px] text-[var(--text-tertiary)]">
            {t.extra.searchOverlayTitle}
          </span>
          <Button
            variant="secondary"
            size="sm"
            onClick={() => {
              setSearchOverlayOpen(false);
              setSessionsOpen(true);
            }}
          >
            {t.sidebar.browseAllSessions}
          </Button>
        </div>
      </DialogShell>

      {/* Dialogs */}
      {pendingInput && (
        <ClarificationDialog request={pendingInput} onReply={replyInput} />
      )}
      <SessionBrowserDialog
        open={sessionsOpen}
        activeSessionId={sessionId}
        runActive={runActive}
        configChangeSessionId={configChangeSessionId}
        onClose={() => setSessionsOpen(false)}
        onResume={(id, accept) => {
          setForceHome(false);
          return resumeSession(id, accept);
        }}
        onClearConfigChange={clearConfigChangePrompt}
      />
      <SettingsDialog
        open={settingsOpen}
        bootstrap={bootstrap}
        workMode={workMode}
        permissionMode={permissionMode}
        lastUsage={lastUsage}
        runActive={runActive}
        initialTab={settingsTab}
        onClose={() => setSettingsOpen(false)}
        onBootstrapUpdated={applyBootstrap}
        onSelectMode={setWorkMode}
        onSelectPermission={setPermissionMode}
      />
      <ExportDialog
        open={exportOpen}
        sessionId={sessionId}
        workspace={bootstrap?.workspace ?? ""}
        onClose={() => setExportOpen(false)}
      />
      <LanguageDialog
        open={languageOpen}
        locale={locale}
        onSelectLocale={setLocale}
        onClose={() => setLanguageOpen(false)}
      />
      {needsSetup && !setupDismissed && bootstrap && (
        <SetupWizard
          bootstrap={bootstrap}
          onFinish={() => setSetupDismissed(true)}
          onSkip={() => setSetupDismissed(true)}
          onBootstrapUpdated={applyBootstrap}
        />
      )}
    </div>
  );
}

export default App;
