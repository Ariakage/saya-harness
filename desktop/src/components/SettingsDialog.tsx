import {
  Activity,
  ArrowDown,
  ArrowUp,
  Box,
  Check,
  Circle,
  Cpu,
  Database,
  Download,
  KeyRound,
  Palette,
  RefreshCw,
  Settings,
  Shield,
  SlidersHorizontal,
  X,
  type LucideIcon,
} from "lucide-react";
import { isTauri } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { check as checkForUpdate } from "@tauri-apps/plugin-updater";
import { type ReactNode, useCallback, useEffect, useState } from "react";
import {
  commands,
  type BootstrapStateDto_Serialize,
  type HelperStatusDto_Serialize,
  type PermissionModeDto,
  type SandboxBackendDto,
  type UsageDto_Serialize,
  type UsageStatsDto,
  type WorkModeDto,
} from "../bindings";
import { LANGUAGE_OPTIONS, getMessages, type LocaleId } from "../lib/i18n";
import { WORK_MODE_STYLES, formatTokenCount } from "../lib/sanitize";
import { isBrowserPreview } from "../preview/browserPreview";
import {
  type ConversationFont,
  type ConversationFontSize,
  type ThemePreference,
  useAppStore,
} from "../store/useAppStore";
import { ExportDialog } from "./ExportDialog";
import { McpCredentialsDialog } from "./McpCredentialsDialog";
import { MemoryDialog } from "./MemoryDialog";
import { ModulesPanel } from "./ModulesPanel";
import { SquaresLoader } from "./SquaresLoader";
import { Button } from "./ui/Button";
import { DialogShell } from "./ui/DialogShell";
import { Pill } from "./ui/Pill";

export type SettingsTab =
  | "general"
  | "appearance"
  | "profiles"
  | "permissions"
  | "modules"
  | "mcp"
  | "memory"
  | "usage"
  | "export";

export interface SettingsDialogProps {
  open: boolean;
  bootstrap: BootstrapStateDto_Serialize | null;
  workMode?: WorkModeDto;
  permissionMode?: PermissionModeDto;
  lastUsage?: UsageDto_Serialize | null;
  runActive: boolean;
  initialTab?: SettingsTab;
  onClose: () => void;
  onBootstrapUpdated: (boot: BootstrapStateDto_Serialize) => void;
  onSelectMode?: (mode: WorkModeDto) => Promise<boolean>;
  onSelectPermission?: (perm: PermissionModeDto) => Promise<boolean>;
  /** @deprecated MCP, Memory and Export are embedded as tabs (D-075). */
  onOpenMcpCreds?: () => void;
  /** @deprecated MCP, Memory and Export are embedded as tabs (D-075). */
  onOpenMemory?: () => void;
  /** @deprecated MCP, Memory and Export are embedded as tabs (D-075). */
  onOpenExport?: () => void;
}

const EFFORTS = ["default", "low", "medium", "high", "xhigh", "max"] as const;
const BACKENDS: SandboxBackendDto[] = ["auto", "seatbelt", "bubblewrap", "host"];
const WORK_MODES: WorkModeDto[] = ["plan", "build", "code", "work"];
const THEMES: ThemePreference[] = ["system", "light", "dark"];
const FONT_SIZES: ConversationFontSize[] = [13, 14, 16];
const CONVERSATION_FONTS: ConversationFont[] = ["default", "serif", "system"];

const USAGE_RANGE_DAYS = { "7d": 7, "30d": 30, "90d": 90 } as const;

const PURPOSE_LABEL_KEYS = {
  execution: "purposeExecution",
  review: "purposeReview",
  compaction: "purposeCompaction",
} as const;

const NAV_GROUPS: { tab: SettingsTab; icon: LucideIcon }[][] = [
  [
    { tab: "general", icon: Settings },
    { tab: "appearance", icon: Palette },
    { tab: "profiles", icon: Cpu },
    { tab: "permissions", icon: Shield },
  ],
  [
    { tab: "modules", icon: Box },
    { tab: "mcp", icon: KeyRound },
    { tab: "memory", icon: Database },
  ],
  [
    { tab: "usage", icon: Activity },
    { tab: "export", icon: Download },
  ],
];

const CARD =
  "rounded-[var(--radius-lg)] border border-[var(--border-subtle)] bg-[var(--surface-raised)] p-3";
const LABEL = "block text-[12px] text-[var(--text-secondary)]";
const ERROR_BOX =
  "rounded-[var(--radius-md)] border border-[var(--danger)] bg-[var(--danger-surface)] px-3 py-2 text-[12px] text-[var(--danger)]";

interface SegmentedOption<T extends string | number> {
  value: T;
  label: string;
  testId?: string;
}

function Segmented<T extends string | number>({
  value,
  options,
  onChange,
  ariaLabel,
  disabled = false,
}: {
  value: T;
  options: SegmentedOption<T>[];
  onChange: (next: T) => void;
  ariaLabel: string;
  disabled?: boolean;
}) {
  return (
    <div
      role="group"
      aria-label={ariaLabel}
      className="inline-flex gap-0.5 rounded-[var(--radius-md)] border border-[var(--border)] bg-[var(--surface-app)] p-0.5"
    >
      {options.map((opt) => (
        <button
          key={String(opt.value)}
          type="button"
          data-testid={opt.testId}
          aria-pressed={value === opt.value}
          disabled={disabled}
          onClick={() => onChange(opt.value)}
          className={`rounded-[var(--radius-sm)] px-2.5 py-1 text-[12px] leading-[18px] font-medium whitespace-nowrap transition-colors disabled:cursor-not-allowed disabled:opacity-40 ${
            value === opt.value
              ? "bg-[var(--surface-hover)] text-[var(--text-primary)]"
              : "text-[var(--text-secondary)] hover:text-[var(--text-primary)]"
          }`}
        >
          {opt.label}
        </button>
      ))}
    </div>
  );
}

function Switch({
  checked,
  onChange,
  ariaLabel,
  disabled = false,
}: {
  checked: boolean;
  onChange: (next: boolean) => void;
  ariaLabel: string;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={ariaLabel}
      disabled={disabled}
      onClick={() => onChange(!checked)}
      className="soyo-switch mt-0.5 disabled:opacity-40"
    >
      <span className="soyo-switch__knob" />
    </button>
  );
}

function SettingRow({
  title,
  description,
  children,
}: {
  title: string;
  description?: string;
  children: ReactNode;
}) {
  return (
    <div className="flex items-start justify-between gap-4">
      <div className="min-w-0 space-y-1">
        <div className="text-[13px] font-medium text-[var(--text-primary)]">
          {title}
        </div>
        {description && (
          <p className="text-[12px] leading-[18px] text-[var(--text-tertiary)]">
            {description}
          </p>
        )}
      </div>
      {children}
    </div>
  );
}

export function SettingsDialog({
  open,
  bootstrap,
  workMode = "work",
  permissionMode = "auto",
  lastUsage = null,
  runActive,
  initialTab = "general",
  onClose,
  onBootstrapUpdated,
  onSelectMode,
  onSelectPermission,
}: SettingsDialogProps) {
  const locale = useAppStore((s) => s.locale);
  const setLocale = useAppStore((s) => s.setLocale);
  const nickname = useAppStore((s) => s.nickname);
  const avatarData = useAppStore((s) => s.avatarData);
  const userName = useAppStore((s) => s.userName);
  const sessionId = useAppStore((s) => s.sessionId);
  const setNickname = useAppStore((s) => s.setNickname);
  const setAvatar = useAppStore((s) => s.setAvatar);
  const theme = useAppStore((s) => s.theme);
  const fontSize = useAppStore((s) => s.fontSize);
  const conversationFont = useAppStore((s) => s.conversationFont);
  const reduceMotion = useAppStore((s) => s.reduceMotion);
  const helperReconnecting = useAppStore((s) => s.helperReconnecting);
  const reconnectHelper = useAppStore((s) => s.reconnectHelper);
  const setTheme = useAppStore((s) => s.setTheme);
  const setFontSize = useAppStore((s) => s.setFontSize);
  const setConversationFont = useAppStore((s) => s.setConversationFont);
  const setReduceMotion = useAppStore((s) => s.setReduceMotion);
  const t = getMessages(locale);
  const [activeTab, setActiveTab] = useState<SettingsTab>(initialTab);
  const [workspaceInput, setWorkspaceInput] = useState("");
  const [triggerInput, setTriggerInput] = useState("");
  const [nicknameInput, setNicknameInput] = useState("");
  const [promptText, setPromptText] = useState("");
  const [promptPath, setPromptPath] = useState("");
  const [promptSaved, setPromptSaved] = useState(false);
  const [usageRange, setUsageRange] = useState<"7d" | "30d" | "90d">("30d");
  const [usageStats, setUsageStats] = useState<UsageStatsDto | null>(null);
  const [usageLoading, setUsageLoading] = useState(false);
  const [usageError, setUsageError] = useState<string | null>(null);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [helperStatus, setHelperStatus] =
    useState<HelperStatusDto_Serialize | null>(null);
  const [helperLoading, setHelperLoading] = useState(false);
  const [updateState, setUpdateState] = useState<
    | { kind: "idle" }
    | { kind: "checking" }
    | { kind: "upToDate" }
    | { kind: "available"; version: string }
    | { kind: "unavailable" }
  >({ kind: "idle" });

  useEffect(() => {
    if (open) {
      setActiveTab(initialTab);
    }
  }, [open, initialTab]);

  useEffect(() => {
    setNicknameInput(nickname ?? "");
  }, [nickname]);

  useEffect(() => {
    if (!open || (!isTauri() && !isBrowserPreview())) return;
    void commands.getGlobalPrompt().then((res) => {
      if (res.status === "ok" && res.data) {
        setPromptText(res.data.content);
        setPromptPath(res.data.path);
        setPromptSaved(false);
      }
    });
  }, [open]);

  const refreshHelper = useCallback(async () => {
    setHelperLoading(true);
    try {
      const res = await commands.getHelperStatus();
      setHelperStatus(res.status === "ok" ? res.data : null);
    } catch {
      setHelperStatus(null);
    } finally {
      setHelperLoading(false);
    }
  }, []);

  useEffect(() => {
    if (!open || activeTab !== "general") return;
    void refreshHelper();
  }, [open, activeTab, refreshHelper]);

  useEffect(() => {
    if (!open || activeTab !== "usage" || (!isTauri() && !isBrowserPreview())) {
      return;
    }
    let cancelled = false;
    setUsageLoading(true);
    setUsageError(null);
    void commands
      .getUsageStats(
        USAGE_RANGE_DAYS[usageRange],
        -new Date().getTimezoneOffset()
      )
      .then((res) => {
        if (cancelled) return;
        if (res.status === "ok") {
          setUsageStats(res.data);
        } else {
          setUsageStats(null);
          setUsageError(`${res.error.code}: ${res.error.message}`);
        }
        setUsageLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [open, activeTab, usageRange]);

  useEffect(() => {
    if (bootstrap) {
      setWorkspaceInput(bootstrap.workspace);
      setTriggerInput(
        bootstrap.auto_compact_trigger_tokens != null
          ? String(bootstrap.auto_compact_trigger_tokens)
          : ""
      );
    }
  }, [bootstrap, open]);

  const runUpdate = async (
    action: () => Promise<
      | { status: "ok"; data: BootstrapStateDto_Serialize }
      | { status: "error"; error: { code: string; message: string } }
    >
  ) => {
    setErrorMsg(null);
    setSaving(true);
    try {
      const res = await action();
      if (res.status === "ok") {
        onBootstrapUpdated(res.data);
      } else {
        setErrorMsg(`${res.error.code}: ${res.error.message}`);
      }
    } finally {
      setSaving(false);
    }
  };

  const handleCheckUpdates = async () => {
    setUpdateState({ kind: "checking" });
    try {
      const candidate = await checkForUpdate();
      if (candidate) {
        const version = candidate.version;
        await candidate.close().catch(() => {});
        setUpdateState({ kind: "available", version });
      } else {
        setUpdateState({ kind: "upToDate" });
      }
    } catch {
      setUpdateState({ kind: "unavailable" });
    }
  };

  const handleRefreshHelper = () => {
    void reconnectHelper();
    void refreshHelper();
  };

  const usageDays = usageStats?.days ?? [];
  const maxBarSum = Math.max(
    1,
    ...usageDays.map(
      (d) => d.input_tokens + d.output_tokens + d.cached_input_tokens
    )
  );
  const scalePx = 88 / maxBarSum;
  const usagePurposes = usageStats?.by_purpose ?? [];
  const usageHasData = (usageStats?.totals.runs ?? 0) > 0;
  const est = (estimated: boolean) => (estimated ? "~" : "");
  const formatDay = (dayStart: number) =>
    new Date(dayStart * 1000).toLocaleDateString(undefined, {
      month: "numeric",
      day: "numeric",
    });

  const navLabels: Record<SettingsTab, string> = {
    general: t.settings.navGeneral,
    appearance: t.extra.navAppearance,
    profiles: t.settings.navProfiles,
    permissions: t.settings.navPermissions,
    modules: t.settings.navModules,
    mcp: t.settings.navMcp,
    memory: t.settings.navMemory,
    usage: t.settings.navUsage,
    export: t.settings.navExport,
  };
  const titles: Record<SettingsTab, string> = {
    general: t.settings.titleGeneral,
    appearance: t.extra.titleAppearance,
    profiles: t.settings.titleProfiles,
    permissions: t.settings.titlePermissions,
    modules: t.settings.titleModules,
    mcp: t.extra.titleMcp,
    memory: t.extra.titleMemory,
    usage: t.settings.titleUsage,
    export: t.extra.titleExport,
  };

  const workspaceSection = (
    <div className="space-y-1.5">
      <label htmlFor="settings-workspace-input" className={LABEL}>
        {t.settings.workspaceDir}
      </label>
      <div className="flex gap-2">
        <input
          id="settings-workspace-input"
          type="text"
          value={workspaceInput}
          disabled={runActive || saving}
          onChange={(e) => setWorkspaceInput(e.target.value)}
          className="soyo-input min-w-0 flex-1 px-2.5 py-1.5 font-mono text-[12px]"
        />
        <Button
          disabled={runActive || saving}
          title={t.settings.browse}
          onClick={() => {
            if (!isTauri()) return;
            void openDialog({
              directory: true,
              defaultPath: workspaceInput || undefined,
              title: t.settings.workspaceDir,
            }).then((picked) => {
              const path = Array.isArray(picked) ? picked[0] : picked;
              if (!path) return;
              setWorkspaceInput(path);
              void runUpdate(() => commands.setWorkspace(path));
            });
          }}
        >
          {t.settings.browse}
        </Button>
        <Button
          variant="primary"
          disabled={runActive || saving || !workspaceInput.trim()}
          onClick={() =>
            void runUpdate(() => commands.setWorkspace(workspaceInput))
          }
        >
          {t.settings.apply}
        </Button>
      </div>
    </div>
  );

  const helperTone = helperReconnecting
    ? "warning"
    : helperStatus?.connected
      ? "success"
      : "danger";
  const helperLabel = helperReconnecting
    ? t.about.helperReconnecting
    : helperStatus?.connected
      ? t.about.helperConnected
      : t.about.helperDisconnected;

  return (
    <DialogShell
      open={open}
      onClose={onClose}
      ariaLabel={titles[activeTab]}
      testId="settings-dialog"
      size="lg"
      panelClassName="flex h-[560px] max-h-[88vh]"
    >
      {/* Left navigation: 3 groups, 9 tabs (D-075 §8.8) */}
      <nav className="flex w-[184px] shrink-0 flex-col gap-0.5 border-r border-[var(--border-subtle)] bg-[var(--surface-sidebar)] px-2 py-3 text-[13px]">
        {NAV_GROUPS.map((group, groupIdx) => (
          <div key={groupIdx} className="flex flex-col gap-0.5">
            {groupIdx > 0 && (
              <div className="my-2 h-px bg-[var(--border-subtle)]" />
            )}
            {group.map(({ tab, icon: Icon }) => (
              <button
                key={tab}
                type="button"
                data-testid={`settings-nav-${tab}`}
                aria-current={activeTab === tab ? "page" : undefined}
                onClick={() => setActiveTab(tab)}
                className={`flex items-center gap-2 rounded-[var(--radius-md)] px-2.5 py-1.5 text-left transition-colors ${
                  activeTab === tab
                    ? "bg-[var(--surface-hover)] text-[var(--text-primary)]"
                    : "text-[var(--text-secondary)] hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]"
                }`}
              >
                <Icon
                  className="h-3.5 w-3.5 shrink-0 text-[var(--text-tertiary)]"
                  strokeWidth={1.75}
                />
                <span className="truncate">{navLabels[tab]}</span>
              </button>
            ))}
          </div>
        ))}
        <div className="mt-auto px-2.5 py-1 text-[11px] text-[var(--text-tertiary)]">
          Soyo v{bootstrap?.version ?? "0.3.0"}
        </div>
      </nav>

      {/* Right body */}
      <div className="flex min-w-0 flex-1 flex-col">
        <div className="flex items-center justify-between border-b border-[var(--border-subtle)] px-4 py-3">
          <div className="flex items-center gap-2">
            <SlidersHorizontal
              className="h-3.5 w-3.5 text-[var(--accent-text)]"
              strokeWidth={1.75}
            />
            <h2 className="text-[14px] font-semibold text-[var(--text-primary)]">
              {titles[activeTab]}
            </h2>
          </div>
          <Button
            variant="ghost"
            size="icon"
            onClick={onClose}
            aria-label={t.a11y.closeSettings}
          >
            <X className="h-4 w-4" strokeWidth={1.75} />
          </Button>
        </div>

        <div
          key={activeTab}
          className="soyo-panel-enter flex-1 space-y-4 overflow-y-auto p-4 text-[12px]"
        >
          {errorMsg && <div className={ERROR_BOX}>{errorMsg}</div>}

          {activeTab === "general" && (
            <>
              {workspaceSection}

              <div className={`${CARD} space-y-2`}>
                <div className="text-[13px] font-medium text-[var(--text-primary)]">
                  {t.settings.profileLabel}
                </div>
                <div className="flex items-center gap-3">
                  <img
                    src={avatarData ?? "/app-icon.png"}
                    alt={t.settings.avatarLabel}
                    className="h-9 w-9 rounded-[var(--radius-lg)] object-cover ring-1 ring-[var(--border)]"
                  />
                  <div className="flex gap-2">
                    <Button
                      disabled={saving}
                      title={t.settings.changeAvatar}
                      onClick={() => {
                        if (!isTauri()) return;
                        void openDialog({
                          multiple: false,
                          title: t.settings.changeAvatar,
                          filters: [
                            {
                              name: t.settings.avatarLabel,
                              extensions: ["png", "jpg", "jpeg", "webp", "gif"],
                            },
                          ],
                        }).then((picked) => {
                          const path = Array.isArray(picked)
                            ? picked[0]
                            : picked;
                          if (path) void setAvatar(path);
                        });
                      }}
                    >
                      {t.settings.changeAvatar}
                    </Button>
                    {avatarData && (
                      <Button
                        variant="ghost"
                        disabled={saving}
                        onClick={() => void setAvatar(null)}
                      >
                        {t.settings.resetAvatar}
                      </Button>
                    )}
                  </div>
                </div>
                <div className="space-y-1">
                  <label htmlFor="settings-nickname-input" className={LABEL}>
                    {t.settings.nicknameLabel}
                  </label>
                  <div className="flex gap-2">
                    <input
                      id="settings-nickname-input"
                      type="text"
                      value={nicknameInput}
                      disabled={saving}
                      placeholder={userName ?? t.home.fallbackName}
                      onChange={(e) => setNicknameInput(e.target.value)}
                      className="soyo-input min-w-0 flex-1 px-2.5 py-1.5 text-[12px]"
                    />
                    <Button
                      variant="primary"
                      aria-label={`${t.settings.apply} ${t.settings.nicknameLabel}`}
                      disabled={saving}
                      onClick={() =>
                        void setNickname(nicknameInput.trim() || null)
                      }
                    >
                      {t.settings.apply}
                    </Button>
                  </div>
                  <p className="text-[11px] text-[var(--text-tertiary)]">
                    {t.settings.nicknameHint}
                  </p>
                </div>
              </div>

              <div className={`${CARD} space-y-2`}>
                <div className="flex items-center justify-between">
                  <div className="text-[13px] font-medium text-[var(--text-primary)]">
                    {t.settings.globalPromptLabel}
                  </div>
                  {promptSaved && (
                    <span className="inline-flex items-center gap-1 text-[11px] text-[var(--success)]">
                      <Check className="h-3 w-3" strokeWidth={1.75} />
                      {t.settings.promptSaved}
                    </span>
                  )}
                </div>
                <textarea
                  aria-label={t.settings.globalPromptLabel}
                  rows={6}
                  value={promptText}
                  disabled={saving}
                  onChange={(e) => {
                    setPromptText(e.target.value);
                    setPromptSaved(false);
                  }}
                  className="soyo-input w-full resize-y px-2.5 py-2 font-mono text-[12px] leading-relaxed"
                />
                <div className="flex items-center justify-between gap-2">
                  <p className="min-w-0 truncate text-[11px] text-[var(--text-tertiary)]">
                    {t.settings.globalPromptHint(
                      promptPath || "~/.soyo/AGENTS.md"
                    )}
                  </p>
                  <Button
                    variant="primary"
                    disabled={saving || !isTauri()}
                    onClick={() => {
                      void commands.setGlobalPrompt(promptText).then((res) => {
                        if (res.status === "ok") {
                          setPromptPath(res.data.path);
                          setPromptSaved(true);
                        }
                      });
                    }}
                  >
                    {t.settings.savePrompt}
                  </Button>
                </div>
              </div>

              <div className={`${CARD} space-y-2`}>
                <div className="flex items-center justify-between">
                  <span className="text-[13px] font-medium text-[var(--text-primary)]">
                    {t.settings.autoCompact} (
                    {bootstrap?.auto_compact_trigger_percent ?? 80}%)
                  </span>
                  <Button
                    variant="secondary"
                    disabled={runActive || saving}
                    onClick={() =>
                      void runUpdate(() =>
                        commands.setAutocompact(!bootstrap?.auto_compact, null)
                      )
                    }
                    className={
                      bootstrap?.auto_compact
                        ? "text-[var(--success)]"
                        : "text-[var(--text-tertiary)]"
                    }
                  >
                    {bootstrap?.auto_compact ? t.settings.on : t.settings.off}
                  </Button>
                </div>
                <div className="flex items-center gap-2">
                  <input
                    type="number"
                    aria-label={t.a11y.triggerTokens}
                    value={triggerInput}
                    disabled={runActive || saving}
                    onChange={(e) => setTriggerInput(e.target.value)}
                    placeholder={t.settings.triggerPlaceholder}
                    className="soyo-input min-w-0 flex-1 px-2.5 py-1 font-mono text-[12px]"
                  />
                  <Button
                    disabled={runActive || saving}
                    onClick={() => {
                      const parsed = triggerInput.trim()
                        ? Number(triggerInput.trim())
                        : null;
                      void runUpdate(() =>
                        commands.setAutocompact(true, parsed)
                      );
                    }}
                  >
                    {t.settings.setTrigger}
                  </Button>
                </div>
              </div>

              {/* About */}
              <section
                data-testid="settings-about"
                className={`${CARD} space-y-3`}
              >
                <div className="text-[13px] font-medium text-[var(--text-primary)]">
                  {t.about.title}
                </div>
                <div className="flex items-center justify-between gap-3">
                  <span className="text-[12px] text-[var(--text-secondary)]">
                    {t.about.productName}
                  </span>
                  <span className="text-[12px] text-[var(--text-primary)]">
                    {t.about.versionLabel} {bootstrap?.version ?? "0.3.0"}
                  </span>
                </div>
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <div className="flex min-w-0 flex-wrap items-center gap-2">
                    <span className="text-[12px] text-[var(--text-secondary)]">
                      {t.about.helperLabel}
                    </span>
                    <Pill tone={helperTone} data-testid="helper-status-pill">
                      {helperLoading || helperReconnecting ? (
                        <SquaresLoader size={12} />
                      ) : helperStatus?.connected ? (
                        <Check className="h-3 w-3" strokeWidth={1.75} />
                      ) : (
                        <Circle className="h-3 w-3" strokeWidth={1.75} />
                      )}
                      {helperLabel}
                    </Pill>
                  </div>
                  <Button
                    variant="ghost"
                    disabled={helperLoading || helperReconnecting}
                    onClick={handleRefreshHelper}
                  >
                    <RefreshCw className="h-3.5 w-3.5" strokeWidth={1.75} />
                    {t.about.refreshHelperBtn}
                  </Button>
                </div>
                {helperStatus && (
                  <p className="text-[11px] text-[var(--text-tertiary)]">
                    {t.about.helperMeta(
                      helperStatus.pid,
                      helperStatus.active_runs,
                      helperStatus.active_clients
                    )}
                  </p>
                )}
                <div className="flex flex-wrap items-center gap-3">
                  <Button
                    disabled={updateState.kind === "checking"}
                    onClick={() => void handleCheckUpdates()}
                  >
                    {updateState.kind === "checking" && (
                      <SquaresLoader size={12} />
                    )}
                    {updateState.kind === "checking"
                      ? t.about.checkingUpdates
                      : t.about.checkUpdatesBtn}
                  </Button>
                  <span
                    role="status"
                    className="text-[11px] text-[var(--text-tertiary)]"
                  >
                    {updateState.kind === "upToDate" && t.about.upToDate}
                    {updateState.kind === "available" &&
                      t.about.updateAvailable(updateState.version)}
                    {updateState.kind === "unavailable" &&
                      t.about.updateUnavailable}
                  </span>
                </div>
              </section>
            </>
          )}

          {activeTab === "appearance" && (
            <div className="space-y-5" data-testid="appearance-panel">
              <SettingRow title={t.appearance.themeLabel}>
                <Segmented
                  ariaLabel={t.appearance.themeLabel}
                  value={theme}
                  onChange={(next) => void setTheme(next)}
                  options={THEMES.map((value) => ({
                    value,
                    testId: `appearance-theme-${value}`,
                    label:
                      value === "system"
                        ? t.appearance.themeSystem
                        : value === "light"
                          ? t.appearance.themeLight
                          : t.appearance.themeDark,
                  }))}
                />
              </SettingRow>

              <div className="space-y-1.5">
                <label htmlFor="settings-language-select" className={LABEL}>
                  {t.appearance.languageLabel}
                </label>
                <select
                  id="settings-language-select"
                  value={locale}
                  onChange={(e) => setLocale(e.target.value as LocaleId)}
                  className="soyo-select w-full px-2.5 py-1.5"
                >
                  {LANGUAGE_OPTIONS.map((opt) => (
                    <option key={opt.id} value={opt.id}>
                      {opt.nativeLabel} — {opt.subtitle}
                    </option>
                  ))}
                </select>
                <p className="text-[11px] text-[var(--text-tertiary)]">
                  {t.appearance.languageHint}
                </p>
              </div>

              <SettingRow title={t.appearance.fontSizeLabel}>
                <Segmented
                  ariaLabel={t.appearance.fontSizeLabel}
                  value={fontSize}
                  onChange={(next) => void setFontSize(next)}
                  options={FONT_SIZES.map((value) => ({
                    value,
                    testId: `appearance-font-size-${value}`,
                    label:
                      value === 13
                        ? t.appearance.fontSizeSmall
                        : value === 14
                          ? t.appearance.fontSizeDefault
                          : t.appearance.fontSizeLarge,
                  }))}
                />
              </SettingRow>

              <SettingRow title={t.appearance.convFontLabel}>
                <Segmented
                  ariaLabel={t.appearance.convFontLabel}
                  value={conversationFont}
                  onChange={(next) => void setConversationFont(next)}
                  options={CONVERSATION_FONTS.map((value) => ({
                    value,
                    testId: `appearance-conv-font-${value}`,
                    label:
                      value === "default"
                        ? t.appearance.convFontDefault
                        : value === "serif"
                          ? t.appearance.convFontSerif
                          : t.appearance.convFontSystem,
                  }))}
                />
              </SettingRow>

              <SettingRow
                title={t.appearance.reduceMotionLabel}
                description={t.appearance.reduceMotionDesc}
              >
                <Switch
                  checked={reduceMotion}
                  ariaLabel={t.appearance.reduceMotionLabel}
                  onChange={(next) => void setReduceMotion(next)}
                />
              </SettingRow>
            </div>
          )}

          {activeTab === "profiles" && (
            <>
              {workspaceSection}
              <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
                <div className="space-y-1.5">
                  <label htmlFor="settings-profile-select" className={LABEL}>
                    {t.settings.modelProfile}
                  </label>
                  <select
                    id="settings-profile-select"
                    value={bootstrap?.selected_profile ?? ""}
                    disabled={runActive || saving}
                    onChange={(e) =>
                      void runUpdate(() =>
                        commands.selectProfile(e.target.value)
                      )
                    }
                    className="soyo-select w-full px-2.5 py-1.5"
                  >
                    {(bootstrap?.profiles ?? []).map((p) => (
                      <option key={p.name} value={p.name}>
                        {p.name} ({p.model})
                      </option>
                    ))}
                  </select>
                </div>

                <div className="space-y-1.5">
                  <label htmlFor="settings-effort-select" className={LABEL}>
                    {t.settings.thinkingEffort}
                  </label>
                  <select
                    id="settings-effort-select"
                    value={bootstrap?.thinking_effort ?? "default"}
                    disabled={runActive || saving}
                    onChange={(e) => {
                      const val = e.target.value;
                      void runUpdate(() =>
                        commands.setThinkingEffort(
                          val === "default" ? null : val
                        )
                      );
                    }}
                    className="soyo-select w-full px-2.5 py-1.5"
                  >
                    {EFFORTS.map((eff) => (
                      <option key={eff} value={eff}>
                        {eff}
                      </option>
                    ))}
                  </select>
                </div>

                <div className="space-y-1.5">
                  <label htmlFor="settings-sandbox-select" className={LABEL}>
                    {t.settings.sandboxBackend}
                  </label>
                  <select
                    id="settings-sandbox-select"
                    value={bootstrap?.sandbox_backend ?? "auto"}
                    disabled={runActive || saving}
                    onChange={(e) =>
                      void runUpdate(() =>
                        commands.setSandboxBackend(
                          e.target.value as SandboxBackendDto
                        )
                      )
                    }
                    className="soyo-select w-full px-2.5 py-1.5"
                  >
                    {BACKENDS.map((bk) => (
                      <option key={bk} value={bk}>
                        {bk}
                      </option>
                    ))}
                  </select>
                </div>
              </div>
            </>
          )}

          {activeTab === "permissions" && (
            <div className={`${CARD} space-y-3`}>
              <div className="flex flex-wrap items-center justify-between gap-2">
                <span className="text-[12px] text-[var(--text-secondary)]">
                  {t.settings.workMode}
                </span>
                <Segmented
                  ariaLabel={t.a11y.workMode}
                  value={workMode}
                  disabled={!onSelectMode}
                  onChange={(next) => void onSelectMode?.(next)}
                  options={WORK_MODES.map((value) => ({
                    value,
                    label: WORK_MODE_STYLES[value].label,
                  }))}
                />
              </div>

              <div className="border-t border-[var(--border-subtle)] pt-3">
                <SettingRow
                  title={t.settings.allowBypassTitle}
                  description={t.settings.allowBypassDesc}
                >
                  <Switch
                    checked={permissionMode === "bypass"}
                    ariaLabel={t.a11y.toggleBypass}
                    disabled={runActive || !onSelectPermission}
                    onChange={(next) =>
                      void onSelectPermission?.(next ? "bypass" : "auto")
                    }
                  />
                </SettingRow>
              </div>
            </div>
          )}

          {activeTab === "modules" && <ModulesPanel runActive={runActive} />}

          {activeTab === "mcp" && (
            <McpCredentialsDialog
              open
              embedded
              runActive={runActive}
              onClose={onClose}
            />
          )}

          {activeTab === "memory" && (
            <MemoryDialog
              open
              embedded
              runActive={runActive}
              onClose={onClose}
            />
          )}

          {activeTab === "export" &&
            (sessionId ? (
              <ExportDialog
                open
                embedded
                sessionId={sessionId}
                workspace={bootstrap?.workspace ?? ""}
                onClose={onClose}
              />
            ) : (
              <div className={`${CARD} text-center text-[12px] text-[var(--text-tertiary)]`}>
                {t.extra.exportUnavailableHint}
              </div>
            ))}

          {activeTab === "usage" && (
            <div className="space-y-3" data-testid="usage-panel">
              <div className="flex items-center justify-between gap-3">
                <span className="text-[12px] text-[var(--text-secondary)]">
                  {t.settings.usageSubtitle}
                </span>
                <Segmented
                  ariaLabel={t.settings.navUsage}
                  value={usageRange}
                  onChange={setUsageRange}
                  options={(["7d", "30d", "90d"] as const).map((r) => ({
                    value: r,
                    label: r,
                  }))}
                />
              </div>

              {usageLoading && (
                <div
                  className={`${CARD} flex items-center justify-center gap-2 text-[12px] text-[var(--text-tertiary)]`}
                >
                  <SquaresLoader size={12} />
                  {t.settings.usageLoading}
                </div>
              )}
              {usageError && !usageLoading && (
                <div className={ERROR_BOX}>{usageError}</div>
              )}
              {!usageLoading && !usageError && !usageHasData && (
                <div
                  className={`${CARD} text-center text-[12px] text-[var(--text-tertiary)]`}
                >
                  {t.settings.usageEmpty}
                </div>
              )}
              {usageHasData && (
                <>
                  <div className="grid grid-cols-3 gap-2.5">
                    {usagePurposes.map((p) => {
                      const labelKey =
                        PURPOSE_LABEL_KEYS[
                          p.purpose as keyof typeof PURPOSE_LABEL_KEYS
                        ];
                      const totals = p.totals;
                      const total = totals.input_tokens + totals.output_tokens;
                      return (
                        <div key={p.purpose} className={CARD}>
                          <div className="mb-1.5 text-[12px] text-[var(--text-secondary)]">
                            {labelKey ? t.settings[labelKey] : p.purpose}
                          </div>
                          <div className="text-[16px] font-semibold text-[var(--text-primary)]">
                            {est(totals.estimated)}
                            {formatTokenCount(total)}{" "}
                            <span className="text-[11px] font-normal text-[var(--text-tertiary)]">
                              {t.settings.tokensUnit}
                            </span>
                          </div>
                          <div className="mt-1.5 space-y-0.5 text-[11px] text-[var(--text-tertiary)]">
                            <div className="flex justify-between">
                              <span>{t.settings.inputLabel}</span>
                              <span>
                                {est(totals.estimated)}
                                {formatTokenCount(totals.input_tokens)}
                              </span>
                            </div>
                            <div className="flex justify-between">
                              <span>{t.settings.outputLabel}</span>
                              <span>
                                {est(totals.estimated)}
                                {formatTokenCount(totals.output_tokens)}
                              </span>
                            </div>
                            <div className="flex justify-between">
                              <span>{t.settings.cacheReadLabel}</span>
                              <span>
                                {est(totals.estimated)}
                                {formatTokenCount(totals.cached_input_tokens)}
                              </span>
                            </div>
                            <div className="flex justify-between pt-0.5">
                              <span>{t.settings.runsLabel}</span>
                              <span>{totals.runs}</span>
                            </div>
                          </div>
                        </div>
                      );
                    })}
                  </div>
                  {usageStats?.totals.estimated && (
                    <div className="px-1 text-[11px] text-[var(--text-tertiary)]">
                      ~ {t.settings.usageEstimated}
                    </div>
                  )}
                </>
              )}

              {usageHasData && usageDays.length > 0 && (
                <div className={CARD}>
                  <div className="flex items-center justify-between">
                    <span className="text-[13px] text-[var(--text-primary)]">
                      {t.settings.tokensPerDay}
                    </span>
                    <div className="flex items-center gap-3 text-[11px] text-[var(--text-tertiary)]">
                      <span className="inline-flex items-center gap-1">
                        <span className="h-[7px] w-[7px] rounded-[var(--radius-sm)] bg-[var(--accent)]" />
                        {t.settings.inputLower}
                      </span>
                      <span className="inline-flex items-center gap-1">
                        <span className="h-[7px] w-[7px] rounded-[var(--radius-sm)] bg-[var(--info)]" />
                        {t.settings.outputLower}
                      </span>
                      <span className="inline-flex items-center gap-1">
                        <span className="h-[7px] w-[7px] rounded-[var(--radius-sm)] bg-[var(--text-tertiary)]" />
                        {t.settings.cacheReadLower}
                      </span>
                    </div>
                  </div>

                  {/* Stacked bar chart with pixel heights <= 88px (§4.7); hover shows per-day breakdown */}
                  <div className="mt-2 flex h-[96px] items-end gap-1 pt-2">
                    {usageDays.map((d, idx) => {
                      const inp = d.input_tokens;
                      const outp = d.output_tokens;
                      const cache = d.cached_input_tokens;
                      const alignClass =
                        idx <= 2
                          ? "left-0"
                          : idx >= usageDays.length - 3
                            ? "right-0"
                            : "left-1/2 -translate-x-1/2";
                      return (
                        <div
                          key={d.day_start}
                          className="group relative flex min-w-[4px] flex-1 flex-col hover:z-30 focus-within:z-30"
                          style={{
                            height: `${Math.round((inp + outp + cache) * scalePx)}px`,
                          }}
                        >
                          <div
                            className={`pointer-events-none absolute bottom-full z-50 mb-1.5 hidden ${alignClass} rounded-[var(--radius-md)] border border-[var(--border-strong)] bg-[var(--surface-overlay)] px-2.5 py-1.5 text-left text-[11px] whitespace-nowrap shadow-[var(--shadow-popover)] group-hover:block`}
                          >
                            <div className="mb-0.5 font-medium text-[var(--text-primary)]">
                              {formatDay(d.day_start)}
                              <span className="ml-1.5 text-[var(--text-tertiary)]">
                                {est(d.estimated)}
                                {formatTokenCount(inp + outp + cache)}{" "}
                                {t.settings.totalLower}
                              </span>
                            </div>
                            <div className="flex items-center gap-1.5 text-[var(--text-secondary)]">
                              <span className="h-1.5 w-1.5 rounded-[var(--radius-sm)] bg-[var(--accent)]" />
                              {t.settings.inputLower} {formatTokenCount(inp)}
                            </div>
                            <div className="flex items-center gap-1.5 text-[var(--text-secondary)]">
                              <span className="h-1.5 w-1.5 rounded-[var(--radius-sm)] bg-[var(--info)]" />
                              {t.settings.outputLower} {formatTokenCount(outp)}
                            </div>
                            <div className="flex items-center gap-1.5 text-[var(--text-secondary)]">
                              <span className="h-1.5 w-1.5 rounded-[var(--radius-sm)] bg-[var(--text-tertiary)]" />
                              {t.settings.cacheReadLower}{" "}
                              {formatTokenCount(cache)}
                            </div>
                          </div>
                          <div className="flex h-full w-full flex-col overflow-hidden rounded-[var(--radius-sm)] transition-[filter] duration-150 group-hover:brightness-110">
                            {cache > 0 && (
                              <div
                                className="bg-[var(--text-tertiary)]"
                                style={{
                                  height: `${Math.round(cache * scalePx)}px`,
                                }}
                              />
                            )}
                            {outp > 0 && (
                              <div
                                className="bg-[var(--info)]"
                                style={{
                                  height: `${Math.round(outp * scalePx)}px`,
                                }}
                              />
                            )}
                            {inp > 0 && (
                              <div
                                className="bg-[var(--accent)]"
                                style={{
                                  height: `${Math.round(inp * scalePx)}px`,
                                }}
                              />
                            )}
                          </div>
                        </div>
                      );
                    })}
                  </div>
                  <div className="mt-1 flex justify-between text-[11px] text-[var(--text-tertiary)]">
                    <span>{t.settings.windowLabel(usageRange)}</span>
                    <span>{t.settings.today}</span>
                  </div>
                </div>
              )}

              <div className="flex items-center gap-3.5 px-1 text-[11px] text-[var(--text-tertiary)]">
                <span className="inline-flex items-center gap-1 text-[var(--success)]">
                  <Circle
                    className="h-2 w-2"
                    fill="currentColor"
                    strokeWidth={1.75}
                  />
                  {t.settings.liveSession}
                </span>
                <span className="inline-flex items-center gap-1">
                  {lastUsage ? (
                    <>
                      <ArrowUp className="h-3 w-3" strokeWidth={1.75} />
                      {formatTokenCount(lastUsage.input_tokens)}
                      <ArrowDown className="h-3 w-3" strokeWidth={1.75} />
                      {formatTokenCount(lastUsage.output_tokens)}
                    </>
                  ) : (
                    t.settings.idle
                  )}
                </span>
              </div>
            </div>
          )}
        </div>
      </div>
    </DialogShell>
  );
}
