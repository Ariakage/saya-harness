import {
  ArrowLeft,
  ArrowRight,
  Bookmark,
  Box,
  ChevronDown,
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
  HelpCircle,
  History,
  Home,
  KeyRound,
  Lock,
  MessageSquare,
  MoreHorizontal,
  PanelRight,
  Pause,
  Pencil,
  Pin,
  Plus,
  RotateCw,
  Search,
  Settings,
  Share2,
  Shield,
  SlidersHorizontal,
  Sparkles,
  Square,
  X,
} from "lucide-react";
import { isTauri } from "@tauri-apps/api/core";
import { useEffect, useRef, useState } from "react";
import type { SessionItemDto_Serialize } from "./bindings";
import { ApprovalDialog } from "./components/ApprovalDialog";
import { ClarificationDialog } from "./components/ClarificationDialog";
import { Composer } from "./components/Composer";
import { ContextSidebar } from "./components/ContextSidebar";
import { ExportDialog } from "./components/ExportDialog";
import { MarkdownRenderer } from "./components/MarkdownRenderer";
import { McpCredentialsDialog } from "./components/McpCredentialsDialog";
import { MemoryDialog } from "./components/MemoryDialog";
import {
  PERMISSION_MODE_LABELS,
  WORK_MODE_STYLES,
  formatTokenCount,
} from "./lib/sanitize";
import { SessionBrowserDialog } from "./components/SessionBrowserDialog";
import { SettingsDialog, type SettingsTab } from "./components/SettingsDialog";
import { ThinkingCardView } from "./components/ThinkingCardView";
import { ToolCardView } from "./components/ToolCardView";
import { UpdaterBanner } from "./components/UpdaterBanner";
import {
  PREVIEW_APPROVAL_REQUEST,
  PREVIEW_INPUT_REQUEST,
  type PreviewScene,
  getPreviewScene,
  isBrowserPreview,
  setPreviewScene,
} from "./preview/browserPreview";
import { useAppStore } from "./store/useAppStore";

// Overlay titlebar only applies inside the packaged app on macOS.
// Traffic lights overlay the top-left ~72×28px of the content area.
const TITLEBAR_OVERLAY =
  isTauri() && /mac/i.test(typeof navigator !== "undefined" ? navigator.userAgent : "");

function workspaceBase(workspace: string | null | undefined): string {
  if (!workspace) return "workspace";
  const trimmed = workspace.replace(/\/+$/, "");
  return trimmed.split("/").pop() || workspace;
}

function groupSessionsByProject(
  sessions: SessionItemDto_Serialize[]
): Array<[string, SessionItemDto_Serialize[]]> {
  const map = new Map<string, SessionItemDto_Serialize[]>();
  for (const item of sessions) {
    const project = workspaceBase(item.workspace);
    const list = map.get(project) ?? [];
    list.push(item);
    map.set(project, list);
  }
  return Array.from(map.entries());
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
  const configChangeSessionId = useAppStore((s) => s.configChangeSessionId);
  const recoveryBanner = useAppStore((s) => s.recoveryBanner);
  const error = useAppStore((s) => s.error);

  const initApp = useAppStore((s) => s.initApp);
  const applyBootstrap = useAppStore((s) => s.applyBootstrap);
  const setThinkingEffort = useAppStore((s) => s.setThinkingEffort);
  const selectProfile = useAppStore((s) => s.selectProfile);
  const setSubmitMode = useAppStore((s) => s.setSubmitMode);
  const startNewSession = useAppStore((s) => s.startNewSession);
  const stageAttachments = useAppStore((s) => s.stageAttachments);
  const removeStagedAttachment = useAppStore((s) => s.removeStagedAttachment);
  const submitPrompt = useAppStore((s) => s.submitPrompt);
  const cancelActiveRun = useAppStore((s) => s.cancelActiveRun);
  const compactCurrentSession = useAppStore((s) => s.compactCurrentSession);
  const replyApproval = useAppStore((s) => s.replyApproval);
  const replyInput = useAppStore((s) => s.replyInput);
  const setWorkMode = useAppStore((s) => s.setWorkMode);
  const cycleWorkMode = useAppStore((s) => s.cycleWorkMode);
  const setPermissionMode = useAppStore((s) => s.setPermissionMode);
  const resumeSession = useAppStore((s) => s.resumeSession);
  const clearConfigChangePrompt = useAppStore((s) => s.clearConfigChangePrompt);
  const toggleMarkdownMode = useAppStore((s) => s.toggleMarkdownMode);
  const clearError = useAppStore((s) => s.clearError);

  const previewMode = isBrowserPreview();
  const [scene, setScene] = useState<PreviewScene>(() => getPreviewScene());
  const [sessionsOpen, setSessionsOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [settingsTab, setSettingsTab] = useState<SettingsTab>("general");
  const [mcpCredsOpen, setMcpCredsOpen] = useState(false);
  const [memoryOpen, setMemoryOpen] = useState(false);
  const [exportOpen, setExportOpen] = useState(false);
  const [userMenuOpen, setUserMenuOpen] = useState(false);
  const [browserPaneOpen, setBrowserPaneOpen] = useState(false);
  const [inspectorOpen, setInspectorOpen] = useState(true);
  const [copiedId, setCopiedId] = useState<string | null>(null);
  const [editDraft, setEditDraft] = useState<{
    text: string;
    nonce: number;
  } | null>(null);
  const [forceHome, setForceHome] = useState(false);
  type SidebarPanel =
    | "nav"
    | "search"
    | "sessions"
    | "memory"
    | "mcp"
    | "export"
    | "customize";
  const [sidebarPanel, setSidebarPanel] = useState<SidebarPanel | null>(null);
  const [topMenu, setTopMenu] = useState<"mode" | "perm" | "more" | null>(null);
  const [sessionQuery, setSessionQuery] = useState("");
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const railRef = useRef<HTMLElement | null>(null);
  const topbarClusterRef = useRef<HTMLDivElement | null>(null);

  const applyPreviewOverlay = (targetScene: PreviewScene) => {
    if (!previewMode) return;
    useAppStore.setState({
      pendingApproval:
        targetScene === "approval" ? PREVIEW_APPROVAL_REQUEST : null,
      pendingInput:
        targetScene === "clarification" ? PREVIEW_INPUT_REQUEST : null,
    });
    setForceHome(targetScene === "home");
    setSidebarPanel(null);
    setBrowserPaneOpen(targetScene === "browser");
    if (targetScene === "peripheral") {
      setSettingsTab("general");
      setSettingsOpen(true);
    } else if (targetScene === "usage") {
      setSettingsTab("usage");
      setSettingsOpen(true);
    } else {
      setSettingsOpen(false);
    }
    setMcpCredsOpen(false);
    setMemoryOpen(false);
    setExportOpen(false);
  };

  useEffect(() => {
    void initApp().then(() => {
      applyPreviewOverlay(getPreviewScene());
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [initApp]);

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

  // Close lightweight popovers (topbar menus, rail user menu) on outside press.
  useEffect(() => {
    if (!topMenu && !userMenuOpen) return;
    const onDown = (e: MouseEvent) => {
      const t = e.target as Node;
      if (
        topMenu &&
        topbarClusterRef.current &&
        !topbarClusterRef.current.contains(t)
      ) {
        setTopMenu(null);
      }
      if (
        userMenuOpen &&
        railRef.current &&
        !railRef.current.contains(t)
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

  const isHomeEmpty =
    forceHome ||
    (!loading &&
      messages.length === 0 &&
      thinkingEntries.length === 0 &&
      !pendingApproval);

  const currentProject = workspaceBase(bootstrap?.workspace);
  const groupedSessions = groupSessionsByProject(recentSessions);
  const activePanel: SidebarPanel =
    sidebarPanel ?? (isHomeEmpty ? "nav" : "sessions");
  const openPanel = (panel: SidebarPanel) => {
    setSidebarPanel(panel);
    if (panel === "nav") setForceHome(true);
    if (panel === "sessions") setForceHome(false);
  };
  const query = sessionQuery.trim().toLowerCase();
  const filteredSessions = query
    ? recentSessions.filter(
        (item) =>
          (item.title ?? "").toLowerCase().includes(query) ||
          workspaceBase(item.workspace).toLowerCase().includes(query)
      )
    : recentSessions;
  const forceComposerPopover =
    previewMode && scene === "effort" ? "effort" : null;

  return (
    <div className="flex h-screen w-screen overflow-hidden bg-[#101010] text-[#e8e8e8]">
      {/* Column 1: Left Icon Rail (48px, #0d0d0d, §2 & Frame 02) */}
      <aside
        ref={railRef}
        aria-label="Primary rail"
        className="relative flex w-[48px] shrink-0 flex-col items-center gap-0.5 border-r border-[#1f1f1f] bg-[#0d0d0d] py-2.5"
      >
        {/* Traffic-light clearance + window drag area (overlay titlebar) */}
        {TITLEBAR_OVERLAY && (
          <div
            data-tauri-drag-region
            className="h-[26px] w-full shrink-0"
            aria-hidden="true"
          />
        )}
        {(
          [
            { panel: "nav", title: "Home", icon: <Home className="h-4 w-4" /> },
            {
              panel: "sessions",
              title: "Sessions",
              icon: <MessageSquare className="h-4 w-4" />,
            },
            {
              panel: "search",
              title: "Search",
              icon: <Search className="h-4 w-4" />,
            },
            {
              panel: "memory",
              title: "Memory",
              icon: <Database className="h-4 w-4" />,
            },
            {
              panel: "mcp",
              title: "MCP & Credentials",
              icon: <Shield className="h-4 w-4" />,
            },
            {
              panel: "customize",
              title: "Customize",
              icon: <Settings className="h-4 w-4" />,
            },
          ] as const
        ).map(({ panel, title, icon }) => (
          <button
            key={panel}
            type="button"
            onClick={() => openPanel(panel)}
            title={title}
            aria-label={`Rail ${title}`}
            className={`flex h-9 w-9 items-center justify-center rounded-[9px] transition-colors ${
              activePanel === panel
                ? "bg-[#1e1e1e] text-[#e8e8e8]"
                : "text-[#8a8a8a] hover:bg-[#1a1a1a] hover:text-[#e8e8e8]"
            }`}
          >
            {icon}
          </button>
        ))}

        <div className="mt-auto flex flex-col items-center gap-1">
          <button
            type="button"
            onClick={() => handleOpenSettings("usage")}
            title="Usage & Tokens"
            aria-label="Usage overview"
            className="flex h-9 w-9 items-center justify-center rounded-[9px] text-[#8a8a8a] transition-colors hover:bg-[#1a1a1a] hover:text-[#e8e8e8]"
          >
            <HelpCircle className="h-4 w-4" />
          </button>

          <button
            type="button"
            onClick={() => setUserMenuOpen((prev) => !prev)}
            title="Account & Profile Menu"
            aria-label="User menu"
            className="flex h-9 w-9 items-center justify-center rounded-[9px] hover:bg-[#1a1a1a]"
          >
            <img
              src="/app-icon.png"
              alt="Soyo"
              className="h-[22px] w-[22px] rounded-full border border-[#2e2e2e] object-cover"
            />
          </button>
        </div>

        {/* User Menu Popover (Frame 05) */}
        {userMenuOpen && (
          <div
            data-testid="user-menu-popover"
            className="absolute bottom-12 left-2 z-40 w-[200px] rounded-[11px] border border-[#2b2b2b] bg-[#1c1c1c] p-1.5 text-[12.5px] shadow-[0_12px_40px_rgba(0,0,0,0.55)]"
          >
            <div className="px-2.5 pb-1.5 pt-1 text-[11px] text-[#6e6e6e]">
              <div className="font-medium text-[#e8e8e8]">soyo-desktop</div>
              <div className="truncate font-mono text-[10.5px]">
                {bootstrap?.selected_profile ?? "default"}
              </div>
            </div>
            <button
              type="button"
              onClick={() => handleOpenSettings("general")}
              className="flex w-full items-center gap-2 rounded-[7px] px-2.5 py-1.5 text-left text-[#c9c9c9] hover:bg-[#262626]"
            >
              <Settings className="h-3.5 w-3.5 text-[#8a8a8a]" />
              <span>General & Preferences</span>
            </button>
            <button
              type="button"
              onClick={() => handleOpenSettings("usage")}
              className="flex w-full items-center gap-2 rounded-[7px] px-2.5 py-1.5 text-left text-[#c9c9c9] hover:bg-[#262626]"
            >
              <SlidersHorizontal className="h-3.5 w-3.5 text-[#8a8a8a]" />
              <span>Usage analytics</span>
            </button>
            <button
              type="button"
              onClick={() => handleOpenSettings("profiles")}
              className="flex w-full items-center gap-2 rounded-[7px] px-2.5 py-1.5 text-left text-[#c9c9c9] hover:bg-[#262626]"
            >
              <Bookmark className="h-3.5 w-3.5 text-[#8a8a8a]" />
              <span>Model providers</span>
            </button>
            <div className="my-1 h-px bg-[#2a2a2a]" />
            <button
              type="button"
              onClick={() => setUserMenuOpen(false)}
              className="flex w-full items-center gap-2 rounded-[7px] px-2.5 py-1.5 text-left text-[#8a8a8a] hover:bg-[#262626] hover:text-[#e8e8e8]"
            >
              <X className="h-3.5 w-3.5" />
              <span>Close menu</span>
            </button>
          </div>
        )}
      </aside>

      {/* Column 2: Sidebar Panel (~248px, #0f0f0f, §2 & Frames 01/02) */}
      <aside
        data-tauri-drag-region
        className="flex w-[248px] shrink-0 flex-col border-r border-[#1f1f1f] bg-[#0f0f0f] p-2.5 text-xs"
      >
        {/* Traffic lights extend ~24px into this column on overlay titlebar */}
        {TITLEBAR_OVERLAY && (
          <div
            data-tauri-drag-region
            className="h-[24px] shrink-0"
            aria-hidden="true"
          />
        )}
        {activePanel === "nav" && (
          <>
            {/* Nav list — Frame 01 (.nav): items switch panels, no dialogs */}
            <nav className="grid grid-cols-1 gap-0.5 text-[12.5px]">
              <button
                type="button"
                disabled={runActive}
                onClick={() => {
                  setForceHome(false);
                  setSidebarPanel("sessions");
                  void startNewSession();
                }}
                className="flex items-center gap-2 rounded-[7px] bg-[#1c1c1c] px-2 py-1.5 text-left font-medium text-[#e8e8e8] hover:bg-[#222222] disabled:opacity-40"
              >
                <Plus className="h-3.5 w-3.5 text-[#d9a08e]" />
                <span>New</span>
              </button>
              {(
                [
                  ["search", "Search", <Search key="i" className="h-3.5 w-3.5 text-[#8a8a8a]" />],
                  ["sessions", "Sessions", <MessageSquare key="i" className="h-3.5 w-3.5 text-[#8a8a8a]" />],
                  ["memory", "Memory", <Database key="i" className="h-3.5 w-3.5 text-[#8a8a8a]" />],
                  ["mcp", "MCP & Creds", <Shield key="i" className="h-3.5 w-3.5 text-[#8a8a8a]" />],
                  ["export", "Export", <Download key="i" className="h-3.5 w-3.5 text-[#8a8a8a]" />],
                  ["customize", "Customize", <Settings key="i" className="h-3.5 w-3.5 text-[#8a8a8a]" />],
                ] as const
              ).map(([panel, label, icon]) => (
                <button
                  key={panel}
                  type="button"
                  onClick={() => openPanel(panel)}
                  className="flex items-center gap-2 rounded-[7px] px-2 py-1.5 text-left text-[#c9c9c9] hover:bg-[#1a1a1a] hover:text-[#e8e8e8]"
                >
                  {icon}
                  <span>{label}</span>
                </button>
              ))}
            </nav>

            {/* Recents — flat list per Frame 01 */}
            <div className="mt-3 flex items-center justify-between px-2 py-1 text-[11px] text-[#6b6b6b]">
              <span>Recents</span>
              <ChevronDown className="h-3 w-3" />
            </div>
            <div className="flex-1 space-y-0.5 overflow-y-auto pr-0.5">
              {recentSessions.length === 0 ? (
                <div className="px-2 py-2 text-[11px] text-[#6e6e6e]">
                  No recent sessions yet.
                </div>
              ) : (
                recentSessions.slice(0, 8).map((item) => (
                  <button
                    key={item.id}
                    type="button"
                    disabled={runActive}
                    onClick={() => {
                      setForceHome(false);
                      void resumeSession(item.id, false);
                    }}
                    className={`flex w-full flex-col gap-0.5 rounded-[8px] px-2.5 py-1.5 text-left transition-colors disabled:opacity-50 ${
                      item.id === sessionId
                        ? "bg-[#1c1c1c] text-[#e8e8e8]"
                        : "text-[#a3a3a3] hover:bg-[#1a1a1a] hover:text-[#d9d9d9]"
                    }`}
                  >
                    <span className="truncate text-[12.5px]">
                      {item.title ?? "Untitled session"}
                    </span>
                    <span className="truncate text-[10.5px] text-[#6b6b6b]">
                      {item.work_mode} · {item.last_status ?? "saved"}
                    </span>
                  </button>
                ))
              )}
            </div>
            <button
              type="button"
              onClick={() => openPanel("sessions")}
              className="px-2.5 py-1.5 text-left text-[12px] text-[#6e6e6e] hover:text-[#a3a3a3]"
            >
              View all
            </button>
          </>
        )}

        {activePanel === "search" && (
          <>
            <div className="flex items-center justify-between px-1 pb-2 pt-0.5">
              <span className="text-[14px] font-semibold text-[#e8e8e8]">
                Search
              </span>
            </div>
            <div className="flex items-center gap-2 rounded-[8px] border border-[#2b2b2b] bg-[#161616] px-2.5 py-1.5">
              <Search className="h-3.5 w-3.5 shrink-0 text-[#6e6e6e]" />
              <input
                value={sessionQuery}
                onChange={(e) => setSessionQuery(e.target.value)}
                placeholder="Search sessions…"
                aria-label="Search sessions"
                className="w-full bg-transparent text-[12.5px] text-[#e8e8e8] placeholder:text-[#6e6e6e] focus:outline-none"
              />
            </div>
            <div className="mt-2 flex-1 space-y-0.5 overflow-y-auto pr-0.5">
              {filteredSessions.length === 0 ? (
                <div className="px-2 py-2 text-[11px] text-[#6e6e6e]">
                  No matching sessions.
                </div>
              ) : (
                filteredSessions.map((item) => (
                  <button
                    key={item.id}
                    type="button"
                    disabled={runActive}
                    onClick={() => {
                      setForceHome(false);
                      void resumeSession(item.id, false);
                    }}
                    className={`flex w-full flex-col gap-0.5 rounded-[8px] px-2.5 py-1.5 text-left transition-colors disabled:opacity-50 ${
                      item.id === sessionId
                        ? "bg-[#1c1c1c] text-[#e8e8e8]"
                        : "text-[#a3a3a3] hover:bg-[#1a1a1a] hover:text-[#d9d9d9]"
                    }`}
                  >
                    <span className="truncate text-[12.5px] font-medium">
                      {item.title ?? "Untitled session"}
                    </span>
                    <span className="truncate text-[11px] text-[#6e6e6e]">
                      {workspaceBase(item.workspace)} · {item.work_mode} ·{" "}
                      {item.last_status ?? "saved"}
                    </span>
                  </button>
                ))
              )}
            </div>
            <button
              type="button"
              onClick={() => setSessionsOpen(true)}
              className="px-2.5 py-1.5 text-left text-[12px] text-[#6e6e6e] hover:text-[#a3a3a3]"
            >
              Browse all sessions…
            </button>
          </>
        )}

        {activePanel === "sessions" && (
          <>
            <div className="flex items-center justify-between px-1 pb-2 pt-0.5">
              <span className="text-[14px] font-semibold text-[#e8e8e8]">
                Sessions
              </span>
              <button
                type="button"
                onClick={() => setSessionsOpen(true)}
                aria-label="More session actions"
                className="flex h-6 w-6 items-center justify-center rounded-[6px] text-[#8a8a8a] hover:bg-[#1a1a1a] hover:text-[#e8e8e8]"
              >
                <MoreHorizontal className="h-4 w-4" />
              </button>
            </div>

            {/* + New session accent link (§3.2) */}
            <button
              type="button"
              disabled={runActive}
              onClick={() => {
                setForceHome(false);
                void startNewSession();
              }}
              className="flex items-center gap-2 rounded-[7px] px-2 py-1.5 text-left text-[13px] font-medium text-[#d9a08e] transition-colors hover:bg-[#1a1a1a] disabled:opacity-40"
            >
              <Plus className="h-3.5 w-3.5" />
              <span>New session</span>
            </button>

            {/* Pinned & Project-Grouped Recent Sessions (§2 & Frame 02) */}
            <div className="mt-2 flex-1 space-y-2.5 overflow-y-auto pr-0.5">
              <div>
                <div className="px-2 py-1 text-[11px] text-[#6b6b6b]">
                  Pinned 1
                </div>
                <div className="rounded-[8px] px-2.5 py-1.5 hover:bg-[#1a1a1a]">
                  <div className="flex items-center gap-1.5 text-[12.5px] text-[#d9d9d9]">
                    <Pin className="h-3 w-3 shrink-0 text-[#8a8a8a]" />
                    <span className="truncate">Hi, I&apos;m Soyo…</span>
                  </div>
                  <div className="mt-0.5 pl-[18px] text-[11px] text-[#d9a08e]">
                    Answer questions
                  </div>
                </div>
              </div>

              {groupedSessions.length === 0 ? (
                <div className="px-2 py-2 text-[11px] text-[#6e6e6e]">
                  No recent sessions yet.
                </div>
              ) : (
                groupedSessions.map(([proj, items]) => (
                  <div key={proj} className="space-y-0.5">
                    <div className="flex items-center justify-between px-2 py-1 text-[11px] text-[#6b6b6b]">
                      <span className="inline-flex items-center gap-1.5 truncate">
                        <Folder className="h-3 w-3 shrink-0" />
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
                          className={`flex w-full flex-col gap-0.5 rounded-[8px] px-2.5 py-1.5 text-left transition-colors disabled:opacity-50 ${
                            isCurrent
                              ? "bg-[#1c1c1c] text-[#e8e8e8]"
                              : "text-[#a3a3a3] hover:bg-[#1a1a1a] hover:text-[#d9d9d9]"
                          }`}
                        >
                          <span className="truncate text-[12.5px] font-medium">
                            {item.title ?? "Untitled session"}
                          </span>
                          <span className="truncate text-[11px] text-[#6e6e6e]">
                            {item.work_mode} · {item.last_status ?? "saved"}
                          </span>
                        </button>
                      );
                    })}
                  </div>
                ))
              )}
            </div>
          </>
        )}

        {activePanel === "memory" && (
          <>
            <div className="px-1 pb-2 pt-0.5">
              <span className="text-[14px] font-semibold text-[#e8e8e8]">
                Memory
              </span>
            </div>
            <p className="px-2 py-1 text-[11.5px] leading-relaxed text-[#8a8a8a]">
              Global memory entries persist across sessions and are injected
              into the system context.
            </p>
            <button
              type="button"
              onClick={() => setMemoryOpen(true)}
              className="mt-1 flex items-center gap-2 rounded-[7px] px-2 py-1.5 text-left text-[13px] font-medium text-[#d9a08e] hover:bg-[#1a1a1a]"
            >
              <Database className="h-3.5 w-3.5" />
              <span>Open memory</span>
            </button>
            <div className="flex-1" />
          </>
        )}

        {activePanel === "mcp" && (
          <>
            <div className="px-1 pb-2 pt-0.5">
              <span className="text-[14px] font-semibold text-[#e8e8e8]">
                MCP & Creds
              </span>
            </div>
            <p className="px-2 py-1 text-[11.5px] leading-relaxed text-[#8a8a8a]">
              {mcpServers.length === 0
                ? "No MCP servers configured for this workspace."
                : `${mcpServers.length} MCP server${mcpServers.length > 1 ? "s" : ""} configured.`}
            </p>
            {mcpServers.slice(0, 4).map((server) => (
              <div
                key={server.server}
                className="flex items-center gap-2 rounded-[7px] px-2 py-1.5 text-[12.5px] text-[#c9c9c9]"
              >
                <Shield className="h-3.5 w-3.5 shrink-0 text-[#8a8a8a]" />
                <span className="truncate">{server.server}</span>
                <span className="ml-auto shrink-0 text-[10.5px] text-[#6e6e6e]">
                  {server.state}
                </span>
              </div>
            ))}
            <button
              type="button"
              onClick={() => setMcpCredsOpen(true)}
              className="mt-1 flex items-center gap-2 rounded-[7px] px-2 py-1.5 text-left text-[13px] font-medium text-[#d9a08e] hover:bg-[#1a1a1a]"
            >
              <KeyRound className="h-3.5 w-3.5" />
              <span>Manage servers & credentials</span>
            </button>
            <div className="flex-1" />
          </>
        )}

        {activePanel === "export" && (
          <>
            <div className="px-1 pb-2 pt-0.5">
              <span className="text-[14px] font-semibold text-[#e8e8e8]">
                Export
              </span>
            </div>
            <p className="px-2 py-1 text-[11.5px] leading-relaxed text-[#8a8a8a]">
              Export the current session as a handoff document or archive.
            </p>
            <button
              type="button"
              onClick={() => setExportOpen(true)}
              className="mt-1 flex items-center gap-2 rounded-[7px] px-2 py-1.5 text-left text-[13px] font-medium text-[#d9a08e] hover:bg-[#1a1a1a]"
            >
              <Download className="h-3.5 w-3.5" />
              <span>Export session…</span>
            </button>
            <button
              type="button"
              onClick={() => setSessionsOpen(true)}
              className="flex items-center gap-2 rounded-[7px] px-2 py-1.5 text-left text-[12.5px] text-[#c9c9c9] hover:bg-[#1a1a1a] hover:text-[#e8e8e8]"
            >
              <History className="h-3.5 w-3.5 text-[#8a8a8a]" />
              <span>Browse sessions to export</span>
            </button>
            <div className="flex-1" />
          </>
        )}

        {activePanel === "customize" && (
          <>
            <div className="px-1 pb-2 pt-0.5">
              <span className="text-[14px] font-semibold text-[#e8e8e8]">
                Customize
              </span>
            </div>
            <nav className="grid grid-cols-1 gap-0.5 text-[12.5px]">
              {(
                [
                  ["general", "General & preferences"],
                  ["profiles", "Model providers & profiles"],
                  ["permissions", "Permissions & sandbox"],
                  ["modules", "Modules & skills"],
                  ["usage", "Usage & tokens"],
                ] as const
              ).map(([tab, label]) => (
                <button
                  key={tab}
                  type="button"
                  onClick={() => handleOpenSettings(tab)}
                  className="flex items-center gap-2 rounded-[7px] px-2 py-1.5 text-left text-[#c9c9c9] hover:bg-[#1a1a1a] hover:text-[#e8e8e8]"
                >
                  <SlidersHorizontal className="h-3.5 w-3.5 text-[#8a8a8a]" />
                  <span>{label}</span>
                </button>
              ))}
            </nav>
            <div className="flex-1" />
          </>
        )}

        {/* Bottom User Chip (Frame 01 / 05) */}
        <button
          type="button"
          onClick={() => setUserMenuOpen((prev) => !prev)}
          className="mt-2 flex items-center gap-2 rounded-[9px] px-2.5 py-2 text-left text-[12px] text-[#a3a3a3] hover:bg-[#1a1a1a] hover:text-[#e8e8e8]"
        >
          <img
            src="/app-icon.png"
            alt="Soyo"
            className="h-5 w-5 shrink-0 rounded-full object-cover"
          />
          <span className="truncate">
            {bootstrap?.selected_profile ?? "anthropic-official"}
          </span>
        </button>
      </aside>

      {/* Column 3: Main Surface (#141414) */}
      <div className="flex min-w-0 flex-1 flex-col bg-[#141414]">
        {/* Topbar (44px, §2 & Frame 02/09) */}
        {/* Frame 01 has no topbar on Home */}
        {!isHomeEmpty && (
        <header
          data-tauri-drag-region
          className="flex min-h-[44px] flex-wrap items-center justify-between gap-2 border-b border-[#1f1f1f] px-3.5 py-1.5 text-[12.5px] text-[#a3a3a3]"
        >
          <div className="flex min-w-0 items-center gap-2">
            <img
              src="/app-icon.png"
              alt="Soyo"
              className="h-5 w-5 shrink-0 rounded-full border border-[#2e2e2e] object-cover"
            />
            <span className="truncate font-medium text-[#e8e8e8]">
              {sessionTitle}
            </span>
            <span className="shrink-0 text-[#6b6b6b]">· {currentProject}</span>
          </div>

          <div
            ref={topbarClusterRef}
            className="relative flex flex-wrap items-center gap-1.5"
            onBlur={(e) => {
              if (!e.currentTarget.contains(e.relatedTarget as Node)) {
                setTopMenu(null);
              }
            }}
          >
            {previewMode && (
              <div className="flex flex-wrap items-center gap-1 rounded-[8px] border border-[#2e2e2e] bg-[#1a1a1a] px-2 py-0.5 text-[10.5px]">
                <Sparkles className="h-3 w-3 text-[#d9a08e]" />
                <span className="mr-0.5 text-[#d9a08e]">Preview</span>
                {PREVIEW_SCENES.map((item) => (
                  <button
                    key={item}
                    type="button"
                    onClick={() => handleSceneChange(item)}
                    className={`rounded px-1.5 py-0.5 ${
                      scene === item
                        ? "bg-[#d9a08e] font-semibold text-[#1c120e]"
                        : "text-[#a3a3a3] hover:bg-[#242424] hover:text-[#e8e8e8]"
                    }`}
                  >
                    {item}
                  </button>
                ))}
              </div>
            )}

            {/* Goal Permanent Topbar Pill (§4.8, amber palette #1c1a16 / #3d3527 / #e8d9b0) */}
            {(runActive || (previewMode && scene === "subagent")) && (
              <div
                data-testid="goal-topbar-pill"
                className="inline-flex items-center gap-2 rounded-[9px] border border-[#3d3527] bg-[#1c1a16] px-2.5 py-1 text-[11.5px] text-[#e8d9b0]"
              >
                <Flag className="h-3 w-3 text-[#e8d9b0]" />
                <span className="max-w-[140px] truncate">{sessionTitle}</span>
                <button
                  type="button"
                  onClick={() => void cancelActiveRun()}
                  title="Pause / interrupt active run"
                  aria-label="Pause goal run"
                  className="flex h-4 w-4 items-center justify-center rounded-[5px] border border-[#3d3527] bg-[#262019] text-[#c9b890]"
                >
                  <Pause className="h-2.5 w-2.5" />
                </button>
                <button
                  type="button"
                  onClick={() => void cancelActiveRun()}
                  title="Stop active run"
                  aria-label="Stop goal run"
                  className="flex h-4 w-4 items-center justify-center rounded-[5px] border border-[#3d3527] bg-[#262019] text-[#c9b890]"
                >
                  <Square className="h-2 w-2 fill-current" />
                </button>
              </div>
            )}

            {/* Mode pill — design topbar badge; opens mode picker (§2 topbar) */}
            <div className="relative">
              <button
                type="button"
                onClick={() => setTopMenu(topMenu === "mode" ? null : "mode")}
                className="devin-pill text-[12px]"
                title="Work mode"
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
                  <span className="text-[#8a8a8a]">
                    → {WORK_MODE_STYLES[pendingMode].label}
                  </span>
                )}
                <ChevronDown className="h-3 w-3 text-[#6e6e6e]" />
              </button>
              {topMenu === "mode" && (
                <div
                  role="menu"
                  className="absolute right-0 top-full z-50 mt-1 w-[180px] rounded-[11px] border border-[#2b2b2b] bg-[#1c1c1c] p-1.5 text-[12.5px] shadow-[0_12px_40px_rgba(0,0,0,0.55)]"
                >
                  {(Object.keys(WORK_MODE_STYLES) as Array<keyof typeof WORK_MODE_STYLES>).map(
                    (mode) => (
                      <button
                        key={mode}
                        type="button"
                        onClick={() => {
                          void setWorkMode(mode);
                          setTopMenu(null);
                        }}
                        className="flex w-full items-center gap-2 rounded-[7px] px-2.5 py-1.5 text-left text-[#c9c9c9] hover:bg-[#262626]"
                      >
                        <span
                          className="h-1.5 w-1.5 rounded-full"
                          style={{ backgroundColor: WORK_MODE_STYLES[mode].color }}
                        />
                        <span>{WORK_MODE_STYLES[mode].label}</span>
                        {mode === workMode && (
                          <span className="ml-auto text-[#4ade80]">✓</span>
                        )}
                        {pendingMode === mode && pendingMode !== workMode && (
                          <span className="ml-auto text-[#fac454]">next</span>
                        )}
                      </button>
                    )
                  )}
                </div>
              )}
            </div>

            {/* Permission pill — plain badge per design */}
            <div className="relative">
              <button
                type="button"
                onClick={() => setTopMenu(topMenu === "perm" ? null : "perm")}
                className="devin-pill text-[12px] text-[#a3a3a3]"
                title="Permission mode"
                aria-haspopup="menu"
              >
                <span className={permissionMode === "manual" ? "text-[#fac454]" : "text-[#8aa8d8]"}>
                  {PERMISSION_MODE_LABELS[permissionMode]}
                </span>
                <ChevronDown className="h-3 w-3 text-[#6e6e6e]" />
              </button>
              {topMenu === "perm" && (
                <div
                  role="menu"
                  className="absolute right-0 top-full z-50 mt-1 w-[180px] rounded-[11px] border border-[#2b2b2b] bg-[#1c1c1c] p-1.5 text-[12.5px] shadow-[0_12px_40px_rgba(0,0,0,0.55)]"
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
                      className="flex w-full items-center gap-2 rounded-[7px] px-2.5 py-1.5 text-left text-[#c9c9c9] hover:bg-[#262626] disabled:opacity-40"
                    >
                      <span>{PERMISSION_MODE_LABELS[perm]}</span>
                      {perm === permissionMode && (
                        <span className="ml-auto text-[#4ade80]">✓</span>
                      )}
                    </button>
                  ))}
                </div>
              )}
            </div>

            {/* ⋯ overflow menu — design's single trailing chip */}
            <div className="relative">
              <button
                type="button"
                onClick={() => setTopMenu(topMenu === "more" ? null : "more")}
                aria-label="More actions"
                aria-haspopup="menu"
                className="devin-pill-btn text-[#a3a3a3]"
              >
                <MoreHorizontal className="h-4 w-4" />
              </button>
              {topMenu === "more" && (
                <div
                  role="menu"
                  className="absolute right-0 top-full z-50 mt-1 w-[210px] rounded-[11px] border border-[#2b2b2b] bg-[#1c1c1c] p-1.5 text-[12.5px] shadow-[0_12px_40px_rgba(0,0,0,0.55)]"
                >
                  <button
                    type="button"
                    disabled={runActive}
                    onClick={() => {
                      setTopMenu(null);
                      setForceHome(false);
                      void startNewSession();
                    }}
                    className="flex w-full items-center gap-2 rounded-[7px] px-2.5 py-1.5 text-left text-[#d9a08e] hover:bg-[#262626] disabled:opacity-40"
                  >
                    <Plus className="h-3.5 w-3.5" />
                    <span>New session</span>
                  </button>
                  <button
                    type="button"
                    onClick={() => {
                      toggleMarkdownMode();
                      setTopMenu(null);
                    }}
                    className="flex w-full items-center gap-2 rounded-[7px] px-2.5 py-1.5 text-left text-[#c9c9c9] hover:bg-[#262626]"
                  >
                    {markdownMode === "render" ? (
                      <Code2 className="h-3.5 w-3.5 text-[#8a8a8a]" />
                    ) : (
                      <Eye className="h-3.5 w-3.5 text-[#8a8a8a]" />
                    )}
                    <span>
                      {markdownMode === "render"
                        ? "Show markdown source"
                        : "Render markdown"}
                    </span>
                  </button>
                  <button
                    type="button"
                    onClick={() => {
                      setBrowserPaneOpen((v) => !v);
                      setTopMenu(null);
                    }}
                    className="flex w-full items-center gap-2 rounded-[7px] px-2.5 py-1.5 text-left text-[#c9c9c9] hover:bg-[#262626]"
                  >
                    <Globe className="h-3.5 w-3.5 text-[#8a8a8a]" />
                    <span>Browser preview</span>
                    {browserPaneOpen && (
                      <span className="ml-auto text-[#4ade80]">✓</span>
                    )}
                  </button>
                  <button
                    type="button"
                    onClick={() => {
                      setInspectorOpen((v) => !v);
                      setTopMenu(null);
                    }}
                    className="flex w-full items-center gap-2 rounded-[7px] px-2.5 py-1.5 text-left text-[#c9c9c9] hover:bg-[#262626]"
                  >
                    <PanelRight className="h-3.5 w-3.5 text-[#8a8a8a]" />
                    <span>Inspector</span>
                    {inspectorOpen && (
                      <span className="ml-auto text-[#4ade80]">✓</span>
                    )}
                  </button>
                  <div className="my-1 h-px bg-[#2a2a2a]" />
                  <button
                    type="button"
                    onClick={() => {
                      setTopMenu(null);
                      setExportOpen(true);
                    }}
                    className="flex w-full items-center gap-2 rounded-[7px] px-2.5 py-1.5 text-left text-[#c9c9c9] hover:bg-[#262626]"
                  >
                    <Download className="h-3.5 w-3.5 text-[#8a8a8a]" />
                    <span>Export session…</span>
                  </button>
                  <button
                    type="button"
                    onClick={() => {
                      setTopMenu(null);
                      setSessionsOpen(true);
                    }}
                    className="flex w-full items-center gap-2 rounded-[7px] px-2.5 py-1.5 text-left text-[#c9c9c9] hover:bg-[#262626]"
                  >
                    <History className="h-3.5 w-3.5 text-[#8a8a8a]" />
                    <span>Browse sessions…</span>
                  </button>
                </div>
              )}
            </div>
          </div>
        </header>
        )}

        <UpdaterBanner />

        {error && (
          <div className="flex items-center justify-between gap-2 border-b border-[#f87171]/35 bg-[#1c1515] px-4 py-2 text-xs text-[#f87171]">
            <span>
              <strong>{error.code}:</strong> {error.message}
            </span>
            <button
              type="button"
              onClick={clearError}
              aria-label="Dismiss error"
              className="rounded p-0.5 hover:bg-[#2a1b1b]"
            >
              <X className="h-3.5 w-3.5" />
            </button>
          </div>
        )}

        {bootstrap && !bootstrap.config_ready && (
          <div className="border-b border-[#4a3f2a] bg-[#1c1a15] px-4 py-2 text-xs text-[#e8d9b0]">
            Configuration incomplete ({bootstrap.config_path}):{" "}
            {bootstrap.config_error ?? "Select or configure a profile first."}
          </div>
        )}

        {recoveryBanner && (
          <div className="border-b border-[#2e2e2e] bg-[#1a1a1a] px-4 py-1.5 text-xs text-[#8aa8d8]">
            {recoveryBanner}
          </div>
        )}

        {/* Main Workspace Split Area */}
        <div className="flex min-h-0 flex-1 overflow-hidden">
          <div className="flex min-w-0 flex-1 flex-col">
            {isHomeEmpty ? (
              /* Frame 01: Home Empty State */
              <div
                data-testid="home-empty-state"
                data-tauri-drag-region
                className="flex flex-1 flex-col items-center justify-center overflow-y-auto px-6 pb-10"
              >
                <div className="mb-8 flex items-center gap-3 font-serif text-[30px] tracking-tight text-[#e8e8e8]">
                  <img
                    src="/app-icon.png"
                    alt="Soyo"
                    className="h-[30px] w-[30px] rounded-full object-cover"
                  />
                  <span>Good morning, ariakage.</span>
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
                  onRemoveAttachment={removeStagedAttachment}
                  onOpenSettings={() => handleOpenSettings("general")}
                  onOpenSessions={() => setSessionsOpen(true)}
                  onOpenMemory={() => setMemoryOpen(true)}
                  onOpenMcp={() => setMcpCredsOpen(true)}
                  onOpenExport={() => setExportOpen(true)}
                />

                <button
                  type="button"
                  onClick={() => handleOpenSettings("general")}
                  className="mt-2.5 inline-flex items-center gap-1.5 rounded-[10px] border border-[#2b2b2b] bg-[#1d1d1d] px-3 py-1.5 text-[12px] text-[#a3a3a3] hover:border-[#3a3a3a] hover:text-[#e8e8e8]"
                >
                  <Folder className="h-3.5 w-3.5 text-[#8a8a8a]" />
                  <span>{currentProject || "Project or folder"}</span>
                </button>

                <div className="mt-8 w-full max-w-[620px] space-y-0.5">
                  <div className="px-3 py-1 text-[11.5px] text-[#6b6b6b]">
                    Ideas for you
                  </div>
                  {[
                    {
                      icon: Clock,
                      text: "Summarize today's sessions into a handoff note",
                      action: () => setExportOpen(true),
                    },
                    {
                      icon: Box,
                      text: "Audit MCP servers and credential scopes",
                      action: () => setMcpCredsOpen(true),
                    },
                    {
                      icon: SlidersHorizontal,
                      text: "Customize Soyo for this workspace",
                      action: () => handleOpenSettings("general"),
                    },
                  ].map((idea) => {
                    const Icon = idea.icon;
                    return (
                      <button
                        key={idea.text}
                        type="button"
                        onClick={idea.action}
                        className="flex w-full items-center gap-3 rounded-[9px] px-3 py-2 text-left text-[13px] text-[#bdbdbd] transition-colors hover:bg-[#1a1a1a]"
                      >
                        <span className="flex h-[26px] w-[26px] shrink-0 items-center justify-center rounded-[7px] border border-[#2b2b2b] bg-[#1d1d1d] text-[#8a8a8a]">
                          <Icon className="h-3.5 w-3.5" />
                        </span>
                        <span>{idea.text}</span>
                      </button>
                    );
                  })}
                </div>
              </div>
            ) : (
              /* Frames 02, 03, 09: Message Stream */
              <div
                ref={scrollRef}
                className="flex-1 overflow-y-auto px-6 pb-3 pt-4"
              >
                <div className="mx-auto flex max-w-[840px] flex-col gap-3.5">
                  {historyOmitted && (
                    <div className="rounded-[8px] border border-[#262626] bg-[#1a1a1a] px-3 py-1.5 text-[11.5px] text-[#8a8a8a]">
                      Earlier history omitted to stay within desktop restore
                      window; full history remains on disk.
                    </div>
                  )}

                  {loading && messages.length === 0 && (
                    <div className="py-16 text-center text-xs text-[#6e6e6e]">
                      Loading runtime state...
                    </div>
                  )}

                  {messages.map((msg) => {
                    if (msg.role === "user") {
                      return (
                        <div
                          key={msg.id}
                          className="group flex flex-col items-end self-end max-w-[640px]"
                        >
                          <div className="rounded-[14px] border border-[#2e2e2e] bg-[#242424] px-3.5 py-2 text-[13.5px] leading-relaxed text-[#ececec]">
                            <pre className="whitespace-pre-wrap font-sans">
                              {msg.text}
                            </pre>
                          </div>
                          {/* Hover actions row (§4.2): timestamp · branch · retry · edit · copy */}
                          <div className="mt-1 flex items-center gap-2.5 px-2 text-[11px] text-[#6e6e6e] opacity-0 transition-opacity group-hover:opacity-100">
                            <span>{msg.label}</span>
                            <button
                              type="button"
                              disabled
                              title="Branch session from this message (not yet supported by the runtime)"
                              className="opacity-40"
                            >
                              <GitBranch className="h-3 w-3" />
                            </button>
                            <button
                              type="button"
                              onClick={() => void submitPrompt(msg.text)}
                              disabled={runActive}
                              title="Retry prompt"
                              className="hover:text-[#e8e8e8] disabled:opacity-40"
                            >
                              <RotateCw className="h-3 w-3" />
                            </button>
                            <button
                              type="button"
                              onClick={() =>
                                setEditDraft({ text: msg.text, nonce: Date.now() })
                              }
                              title="Edit message in composer"
                              className="hover:text-[#e8e8e8]"
                            >
                              <Pencil className="h-3 w-3" />
                            </button>
                            <button
                              type="button"
                              onClick={() => void handleCopyMessage(msg.id, msg.text)}
                              title="Copy message"
                              className="inline-flex items-center gap-1 hover:text-[#e8e8e8]"
                            >
                              <Copy className="h-3 w-3" />
                              {copiedId === msg.id && <span>Copied</span>}
                            </button>
                            <button
                              type="button"
                              onClick={() => setExportOpen(true)}
                              title="Export / share"
                              className="hover:text-[#e8e8e8]"
                            >
                              <Share2 className="h-3 w-3" />
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
                          className="flex items-center justify-between px-1 py-0.5 text-[11.5px] text-[#6e6e6e]"
                        >
                          <span className="font-mono uppercase text-[#8a8a8a]">
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
                          className="px-3 py-0.5 text-[12px] text-[#6e6e6e]"
                        >
                          {msg.text}
                        </div>
                      );
                    }

                    /* Hard Rule (§4.2): Assistant messages render borderless directly on #141414 */
                    return (
                      <article key={msg.id} className="py-0.5">
                        <MarkdownRenderer
                          content={msg.text}
                          mode={markdownMode}
                          streaming={msg.streaming}
                        />
                      </article>
                    );
                  })}

                  {thinkingEntries.length > 0 && (
                    <ThinkingCardView entries={thinkingEntries} />
                  )}

                  {/* Inline Approval Card in stream (§4.4, Frame 03) */}
                  {pendingApproval && (
                    <ApprovalDialog
                      request={pendingApproval}
                      onReply={replyApproval}
                    />
                  )}

                  {(runActive || summaryLine) && (
                    <div className="flex flex-wrap items-center gap-3.5 px-1 pt-1 font-mono text-[11px] tabular-nums text-[#6e6e6e]">
                      {runActive ? (
                        <span className="text-[#4ade80]">● streaming</span>
                      ) : null}
                      {summaryLine && <span>{summaryLine}</span>}
                      {lastUsage && (
                        <span>
                          ↑{formatTokenCount(lastUsage.input_tokens)} ↓
                          {formatTokenCount(lastUsage.output_tokens)}
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
                onStageAttachments={stageAttachments}
                onRemoveAttachment={removeStagedAttachment}
                onOpenSettings={() => handleOpenSettings("general")}
                onOpenSessions={() => setSessionsOpen(true)}
                onOpenMemory={() => setMemoryOpen(true)}
                onOpenMcp={() => setMcpCredsOpen(true)}
                onOpenExport={() => setExportOpen(true)}
              />
            )}
          </div>

          {/* Split Browser Preview Pane (Frame 08, §4.9) */}
          {browserPaneOpen && (
            <section
              data-testid="split-browser-pane"
              className="flex w-[44%] min-w-[320px] flex-col border-l border-[#1f1f1f] bg-[#0e0e0e]"
            >
              <div className="flex h-[44px] items-center gap-2 border-b border-[#1f1f1f] px-3 text-[#8a8a8a]">
                <ArrowLeft className="h-3.5 w-3.5" />
                <ArrowRight className="h-3.5 w-3.5" />
                <RotateCw className="h-3.5 w-3.5" />
                <div className="flex flex-1 items-center justify-center gap-1.5 rounded-[8px] border border-[#262626] bg-[#1a1a1a] px-2.5 py-1 text-[11.5px] text-[#a3a3a3]">
                  <Lock className="h-3 w-3 text-[#6e6e6e]" />
                  <span>localhost:1420</span>
                </div>
                <ExternalLink className="h-3.5 w-3.5" />
                <button
                  type="button"
                  onClick={() => setBrowserPaneOpen(false)}
                  aria-label="Close split browser"
                  className="rounded p-1 hover:bg-[#1a1a1a] hover:text-[#e8e8e8]"
                >
                  <X className="h-3.5 w-3.5" />
                </button>
              </div>
              <div className="flex flex-1 flex-col items-center justify-center gap-3 bg-[#141414] p-6 text-center">
                <div className="flex items-center gap-2 font-serif text-[17px] text-[#e8e8e8]">
                  <img
                    src="/app-icon.png"
                    alt="Soyo"
                    className="h-[18px] w-[18px] rounded-full object-cover"
                  />
                  <span>Good morning, ariakage.</span>
                </div>
                <div className="w-full max-w-[300px] rounded-[10px] border border-[#2b2b2b] bg-[#1d1d1d] p-2.5 text-left text-[11px] text-[#6e6e6e]">
                  <div>How can I help you today?</div>
                  <div className="mt-2 flex items-center justify-between">
                    <span className="rounded bg-[#262626] px-1.5 py-0.5 text-[10px] text-[#a3a3a3]">
                      +
                    </span>
                    <span className="rounded bg-[#262626] px-1.5 py-0.5 text-[10px] text-[#a3a3a3]">
                      {bootstrap?.selected_model ?? "claude-sonnet-4-5"}
                    </span>
                  </div>
                </div>
              </div>
            </section>
          )}
        </div>
      </div>

      {/* Column 4: Right Inspector (308px, #0f0f0f, §2) */}
      {inspectorOpen && !isHomeEmpty && !browserPaneOpen && (
        <ContextSidebar
          bootstrap={bootstrap}
          workMode={workMode}
          contextEntries={contextEntries}
          contextSkillsCount={contextSkillsCount}
          contextEstimated={contextEstimated}
          todos={todos}
          mcpServers={mcpServers}
          recentSessions={recentSessions}
          activeSessionId={sessionId}
        />
      )}

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
        onOpenMcpCreds={() => setMcpCredsOpen(true)}
        onOpenMemory={() => setMemoryOpen(true)}
        onOpenExport={() => setExportOpen(true)}
      />
      <McpCredentialsDialog
        open={mcpCredsOpen}
        runActive={runActive}
        onClose={() => setMcpCredsOpen(false)}
      />
      <MemoryDialog
        open={memoryOpen}
        runActive={runActive}
        onClose={() => setMemoryOpen(false)}
      />
      <ExportDialog
        open={exportOpen}
        sessionId={sessionId}
        workspace={bootstrap?.workspace ?? ""}
        onClose={() => setExportOpen(false)}
      />
    </div>
  );
}

export default App;
