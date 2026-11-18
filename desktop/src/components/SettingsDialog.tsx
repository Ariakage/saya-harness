import {
  Activity,
  Box,
  Cpu,
  Database,
  Download,
  KeyRound,
  Monitor,
  Settings,
  Shield,
  SlidersHorizontal,
  X,
} from "lucide-react";
import { isTauri } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { useEffect, useState } from "react";
import {
  commands,
  type BootstrapStateDto_Serialize,
  type PermissionModeDto,
  type SandboxBackendDto,
  type UsageDto_Serialize,
  type UsageStatsDto,
  type WorkModeDto,
} from "../bindings";
import { LANGUAGE_OPTIONS, getMessages, type LocaleId } from "../lib/i18n";
import { WORK_MODE_STYLES, formatTokenCount } from "../lib/sanitize";
import { useAppStore } from "../store/useAppStore";
import { ModulesPanel } from "./ModulesPanel";

export type SettingsTab =
  | "general"
  | "usage"
  | "profiles"
  | "permissions"
  | "modules";

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
  onOpenMcpCreds?: () => void;
  onOpenMemory?: () => void;
  onOpenExport?: () => void;
}

const EFFORTS = ["default", "low", "medium", "high", "xhigh", "max"] as const;
const BACKENDS: SandboxBackendDto[] = ["auto", "seatbelt", "bubblewrap", "host"];
const WORK_MODES: WorkModeDto[] = ["plan", "build", "code", "work"];

const USAGE_RANGE_DAYS = { "7d": 7, "30d": 30, "90d": 90 } as const;

const PURPOSE_LABEL_KEYS = {
  execution: "purposeExecution",
  review: "purposeReview",
  compaction: "purposeCompaction",
} as const;

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
  onOpenMcpCreds,
  onOpenMemory,
  onOpenExport,
}: SettingsDialogProps) {
  const locale = useAppStore((s) => s.locale);
  const setLocale = useAppStore((s) => s.setLocale);
  const nickname = useAppStore((s) => s.nickname);
  const avatarData = useAppStore((s) => s.avatarData);
  const userName = useAppStore((s) => s.userName);
  const setNickname = useAppStore((s) => s.setNickname);
  const setAvatar = useAppStore((s) => s.setAvatar);
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

  useEffect(() => {
    if (open) {
      setActiveTab(initialTab);
    }
  }, [open, initialTab]);

  useEffect(() => {
    if (!open) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onClose();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [open, onClose]);

  useEffect(() => {
    setNicknameInput(nickname ?? "");
  }, [nickname]);

  useEffect(() => {
    if (!open || !isTauri()) return;
    void commands.getGlobalPrompt().then((res) => {
      if (res.status === "ok") {
        setPromptText(res.data.content);
        setPromptPath(res.data.path);
        setPromptSaved(false);
      }
    });
  }, [open]);

  useEffect(() => {
    if (!open || activeTab !== "usage" || !isTauri()) return;
    let cancelled = false;
    setUsageLoading(true);
    setUsageError(null);
    void commands
      .getUsageStats(USAGE_RANGE_DAYS[usageRange])
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

  if (!open) {
    return null;
  }

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

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label="Desktop Settings"
      data-testid="settings-dialog"
      className="soyo-backdrop-enter fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4 backdrop-blur-xs"
    >
      <div className="soyo-dialog-enter flex h-[560px] max-h-[88vh] w-full max-w-[700px] overflow-hidden rounded-[14px] border border-[#2b2b2b] bg-[#181818] shadow-[0_24px_60px_rgba(0,0,0,0.65)]">
        {/* Left Navigation (Frame 04 & Frame 10) */}
        <nav className="flex w-[184px] shrink-0 flex-col gap-0.5 border-r border-[#232323] bg-[#141414] px-2 py-3 text-[12.5px]">
          <button
            type="button"
            onClick={() => setActiveTab("general")}
            className={`flex items-center gap-2 rounded-[7px] px-2.5 py-1.5 text-left transition-colors ${
              activeTab === "general"
                ? "bg-[#222222] text-[#e8e8e8]"
                : "text-[#a3a3a3] hover:bg-[#1c1c1c] hover:text-[#e8e8e8]"
            }`}
          >
            <Settings className="h-3.5 w-3.5 text-[#8a8a8a]" />
            <span>{t.settings.navGeneral}</span>
          </button>

          <button
            type="button"
            onClick={() => setActiveTab("usage")}
            className={`flex items-center gap-2 rounded-[7px] px-2.5 py-1.5 text-left transition-colors ${
              activeTab === "usage"
                ? "bg-[#222222] text-[#e8e8e8]"
                : "text-[#a3a3a3] hover:bg-[#1c1c1c] hover:text-[#e8e8e8]"
            }`}
          >
            <Activity className="h-3.5 w-3.5 text-[#8a8a8a]" />
            <span>{t.settings.navUsage}</span>
          </button>

          <button
            type="button"
            onClick={() => setActiveTab("profiles")}
            className={`flex items-center gap-2 rounded-[7px] px-2.5 py-1.5 text-left transition-colors ${
              activeTab === "profiles"
                ? "bg-[#222222] text-[#e8e8e8]"
                : "text-[#a3a3a3] hover:bg-[#1c1c1c] hover:text-[#e8e8e8]"
            }`}
          >
            <Cpu className="h-3.5 w-3.5 text-[#8a8a8a]" />
            <span>{t.settings.navProfiles}</span>
          </button>

          <button
            type="button"
            onClick={() => setActiveTab("permissions")}
            className={`flex items-center gap-2 rounded-[7px] px-2.5 py-1.5 text-left transition-colors ${
              activeTab === "permissions"
                ? "bg-[#222222] text-[#e8e8e8]"
                : "text-[#a3a3a3] hover:bg-[#1c1c1c] hover:text-[#e8e8e8]"
            }`}
          >
            <Shield className="h-3.5 w-3.5 text-[#8a8a8a]" />
            <span>{t.settings.navPermissions}</span>
          </button>

          <button
            type="button"
            onClick={() => setActiveTab("modules")}
            className={`flex items-center gap-2 rounded-[7px] px-2.5 py-1.5 text-left transition-colors ${
              activeTab === "modules"
                ? "bg-[#222222] text-[#e8e8e8]"
                : "text-[#a3a3a3] hover:bg-[#1c1c1c] hover:text-[#e8e8e8]"
            }`}
          >
            <Box className="h-3.5 w-3.5 text-[#8a8a8a]" />
            <span>{t.settings.navModules}</span>
          </button>

          <div className="my-2 h-px bg-[#232323]" />

          {onOpenMcpCreds && (
            <button
              type="button"
              onClick={() => {
                onClose();
                onOpenMcpCreds();
              }}
              className="flex items-center gap-2 rounded-[7px] px-2.5 py-1.5 text-left text-[#a3a3a3] hover:bg-[#1c1c1c] hover:text-[#e8e8e8]"
            >
              <KeyRound className="h-3.5 w-3.5 text-[#8a8a8a]" />
              <span>{t.settings.navMcp}</span>
            </button>
          )}

          {onOpenMemory && (
            <button
              type="button"
              onClick={() => {
                onClose();
                onOpenMemory();
              }}
              className="flex items-center gap-2 rounded-[7px] px-2.5 py-1.5 text-left text-[#a3a3a3] hover:bg-[#1c1c1c] hover:text-[#e8e8e8]"
            >
              <Database className="h-3.5 w-3.5 text-[#8a8a8a]" />
              <span>{t.settings.navMemory}</span>
            </button>
          )}

          {onOpenExport && (
            <button
              type="button"
              onClick={() => {
                onClose();
                onOpenExport();
              }}
              className="flex items-center gap-2 rounded-[7px] px-2.5 py-1.5 text-left text-[#a3a3a3] hover:bg-[#1c1c1c] hover:text-[#e8e8e8]"
            >
              <Download className="h-3.5 w-3.5 text-[#8a8a8a]" />
              <span>{t.settings.navExport}</span>
            </button>
          )}

          <div className="mt-auto flex items-center gap-2 px-2.5 py-1 text-[11px] text-[#6e6e6e]">
            <Monitor className="h-3 w-3" />
            <span>Soyo v{bootstrap?.version ?? "0.2.0"}</span>
          </div>
        </nav>

        {/* Right Body */}
        <div className="flex min-w-0 flex-1 flex-col">
          <div className="flex items-center justify-between border-b border-[#232323] px-4 py-3">
            <div className="flex items-center gap-2">
              <SlidersHorizontal className="h-3.5 w-3.5 text-[#d9a08e]" />
              <h2 className="text-[14px] font-semibold text-[#e8e8e8]">
                {activeTab === "usage"
                  ? t.settings.titleUsage
                  : activeTab === "profiles"
                    ? t.settings.titleProfiles
                    : activeTab === "permissions"
                      ? t.settings.titlePermissions
                      : activeTab === "modules"
                        ? t.settings.titleModules
                        : t.settings.titleGeneral}
              </h2>
            </div>
            <button
              type="button"
              onClick={onClose}
              aria-label="Close settings"
              className="rounded-[7px] p-1 text-[#8a8a8a] transition-colors hover:bg-[#222222] hover:text-[#e8e8e8]"
            >
              <X className="h-4 w-4" />
            </button>
          </div>

          <div
            key={activeTab}
            className="soyo-panel-enter flex-1 space-y-4 overflow-y-auto p-4 text-xs"
          >
            {errorMsg && (
              <div className="rounded-[8px] border border-[#f87171]/35 bg-[#1c1515] px-3 py-2 text-[#f87171]">
                {errorMsg}
              </div>
            )}

            {activeTab === "usage" && (
              <div className="space-y-3" data-testid="usage-panel">
                <div className="flex items-center justify-between">
                  <span className="text-[12px] text-[#a3a3a3]">
                    {t.settings.usageSubtitle}
                  </span>
                  <div className="inline-flex gap-0.5 rounded-[8px] border border-[#2b2b2b] bg-[#1e1e1e] p-0.5">
                    {(["7d", "30d", "90d"] as const).map((r) => (
                      <button
                        key={r}
                        type="button"
                        onClick={() => setUsageRange(r)}
                        className={`rounded-[6px] px-3 py-1 text-[11.5px] ${
                          usageRange === r
                            ? "bg-[#2e2e2e] text-[#e8e8e8]"
                            : "text-[#a3a3a3] hover:text-[#e8e8e8]"
                        }`}
                      >
                        {r}
                      </button>
                    ))}
                  </div>
                </div>

                {usageLoading && (
                  <div className="rounded-[10px] border border-[#262626] bg-[#1a1a1a] p-3 text-center text-[11.5px] text-[#8a8a8a]">
                    {t.settings.usageLoading}
                  </div>
                )}
                {usageError && !usageLoading && (
                  <div className="rounded-[8px] border border-[#f87171]/35 bg-[#1c1515] px-3 py-2 text-[#f87171]">
                    {usageError}
                  </div>
                )}
                {!usageLoading && !usageError && !usageHasData && (
                  <div className="rounded-[10px] border border-[#262626] bg-[#1a1a1a] p-3 text-center text-[11.5px] text-[#8a8a8a]">
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
                        const total =
                          totals.input_tokens + totals.output_tokens;
                        return (
                          <div
                            key={p.purpose}
                            className="rounded-[10px] border border-[#262626] bg-[#1a1a1a] p-3"
                          >
                            <div className="mb-1.5 text-[12px] text-[#c9c9c9]">
                              {labelKey ? t.settings[labelKey] : p.purpose}
                            </div>
                            <div className="text-[16px] font-semibold text-[#e8e8e8]">
                              {est(totals.estimated)}
                              {formatTokenCount(total)}{" "}
                              <span className="text-[10.5px] font-normal text-[#6e6e6e]">
                                {t.settings.tokensUnit}
                              </span>
                            </div>
                            <div className="mt-1.5 space-y-0.5 text-[10.5px] text-[#8a8a8a]">
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
                                  {formatTokenCount(
                                    totals.cached_input_tokens
                                  )}
                                </span>
                              </div>
                              <div className="flex justify-between pt-0.5 text-[#6e6e6e]">
                                <span>{t.settings.runsLabel}</span>
                                <span>{totals.runs}</span>
                              </div>
                            </div>
                          </div>
                        );
                      })}
                    </div>
                    {usageStats?.totals.estimated && (
                      <div className="px-1 text-[10.5px] text-[#6e6e6e]">
                        ~ {t.settings.usageEstimated}
                      </div>
                    )}
                  </>
                )}

                {usageHasData && usageDays.length > 0 && (
                  <div className="rounded-[10px] border border-[#262626] bg-[#1a1a1a] p-3">
                    <div className="flex items-center justify-between">
                      <span className="text-[12.5px] text-[#e8e8e8]">
                        {t.settings.tokensPerDay}
                      </span>
                      <div className="flex items-center gap-3 text-[11px] text-[#6e6e6e]">
                        <span className="inline-flex items-center gap-1">
                          <span className="h-[7px] w-[7px] rounded-[2px] bg-[#d9a08e]" />
                          {t.settings.inputLower}
                        </span>
                        <span className="inline-flex items-center gap-1">
                          <span className="h-[7px] w-[7px] rounded-[2px] bg-[#8aa8d8]" />
                          {t.settings.outputLower}
                        </span>
                        <span className="inline-flex items-center gap-1">
                          <span className="h-[7px] w-[7px] rounded-[2px] bg-[#6b6b6b]" />
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
                              className={`pointer-events-none absolute bottom-full z-50 mb-1.5 hidden ${alignClass} whitespace-nowrap rounded-[7px] border border-[#383838] bg-[#161616] px-2.5 py-1.5 text-left text-[10.5px] shadow-[0_12px_32px_rgba(0,0,0,0.8)] group-hover:block`}
                            >
                              <div className="mb-0.5 font-medium text-[#e8e8e8]">
                                {formatDay(d.day_start)}
                                <span className="ml-1.5 text-[#8a8a8a]">
                                  {est(d.estimated)}
                                  {formatTokenCount(inp + outp + cache)}{" "}
                                  {t.settings.totalLower}
                                </span>
                              </div>
                              <div className="flex items-center gap-1.5 text-[#c9c9c9]">
                                <span
                                  className="h-1.5 w-1.5 rounded-[1px]"
                                  style={{ backgroundColor: "#d9a08e" }}
                                />
                                {t.settings.inputLower}{" "}
                                {formatTokenCount(inp)}
                              </div>
                              <div className="flex items-center gap-1.5 text-[#c9c9c9]">
                                <span
                                  className="h-1.5 w-1.5 rounded-[1px]"
                                  style={{ backgroundColor: "#8aa8d8" }}
                                />
                                {t.settings.outputLower}{" "}
                                {formatTokenCount(outp)}
                              </div>
                              <div className="flex items-center gap-1.5 text-[#c9c9c9]">
                                <span
                                  className="h-1.5 w-1.5 rounded-[1px]"
                                  style={{ backgroundColor: "#6b6b6b" }}
                                />
                                {t.settings.cacheReadLower}{" "}
                                {formatTokenCount(cache)}
                              </div>
                            </div>
                            <div className="flex h-full w-full flex-col overflow-hidden rounded-[2px] transition-[filter] duration-150 group-hover:brightness-110">
                              {cache > 0 && (
                                <div
                                  style={{
                                    height: `${Math.round(cache * scalePx)}px`,
                                    backgroundColor: "#6b6b6b",
                                  }}
                                />
                              )}
                              {outp > 0 && (
                                <div
                                  style={{
                                    height: `${Math.round(outp * scalePx)}px`,
                                    backgroundColor: "#8aa8d8",
                                  }}
                                />
                              )}
                              {inp > 0 && (
                                <div
                                  style={{
                                    height: `${Math.round(inp * scalePx)}px`,
                                    backgroundColor: "#d9a08e",
                                  }}
                                />
                              )}
                            </div>
                          </div>
                        );
                      })}
                    </div>
                    <div className="mt-1 flex justify-between text-[10px] text-[#5e5e5e]">
                      <span>{t.settings.windowLabel(usageRange)}</span>
                      <span>{t.settings.today}</span>
                    </div>
                  </div>
                )}

                <div className="flex items-center gap-3.5 px-1 text-[11px] text-[#6e6e6e]">
                  <span className="text-[#4ade80]">● {t.settings.liveSession}</span>
                  <span>
                    {lastUsage
                      ? `↑${formatTokenCount(lastUsage.input_tokens)} ↓${formatTokenCount(lastUsage.output_tokens)}`
                      : t.settings.idle}
                  </span>
                </div>
              </div>
            )}

            {(activeTab === "general" || activeTab === "profiles") && (
              <>
                <div className="space-y-1.5">
                  <label
                    htmlFor="settings-workspace-input"
                    className="block text-[12px] text-[#a3a3a3]"
                  >
                    {t.settings.workspaceDir}
                  </label>
                  <div className="flex gap-2">
                    <input
                      id="settings-workspace-input"
                      type="text"
                      value={workspaceInput}
                      disabled={runActive || saving}
                      onChange={(e) => setWorkspaceInput(e.target.value)}
                      className="soyo-input flex-1 px-2.5 py-1.5 font-mono text-xs text-[#d9d9d9]"
                    />
                    <button
                      type="button"
                      disabled={runActive || saving}
                      title="Choose a directory"
                      onClick={() => {
                        if (!isTauri()) return;
                        void openDialog({
                          directory: true,
                          defaultPath: workspaceInput || undefined,
                          title: "Choose workspace directory",
                        }).then((picked) => {
                          const path = Array.isArray(picked)
                            ? picked[0]
                            : picked;
                          if (!path) return;
                          setWorkspaceInput(path);
                          void runUpdate(() => commands.setWorkspace(path));
                        });
                      }}
                      className="soyo-pill-btn font-medium text-[#c9c9c9] disabled:opacity-40"
                    >
                      {t.settings.browse}
                    </button>
                    <button
                      type="button"
                      disabled={runActive || saving || !workspaceInput.trim()}
                      onClick={() =>
                        void runUpdate(() => commands.setWorkspace(workspaceInput))
                      }
                      className="soyo-pill-btn font-medium text-[#d9a08e] disabled:opacity-40"
                    >
                      {t.settings.apply}
                    </button>
                  </div>
                </div>

                {activeTab === "general" && (
                  <div className="space-y-1.5">
                    <label
                      htmlFor="settings-language-select"
                      className="block text-[12px] text-[#a3a3a3]"
                    >
                      {t.settings.languageLabel}
                    </label>
                    <select
                      id="settings-language-select"
                      aria-label="Interface language"
                      value={locale}
                      onChange={(e) => setLocale(e.target.value as LocaleId)}
                      className="soyo-select w-full px-2.5 py-1.5 text-xs text-[#d9d9d9]"
                    >
                      {LANGUAGE_OPTIONS.map((opt) => (
                        <option key={opt.id} value={opt.id} className="bg-[#181818]">
                          {opt.nativeLabel} — {opt.subtitle}
                        </option>
                      ))}
                    </select>
                  </div>
                )}

                {activeTab === "general" && (
                  <div className="space-y-2 rounded-[10px] border border-[#262626] bg-[#1a1a1a] p-3">
                    <div className="text-[12px] font-medium text-[#e8e8e8]">
                      {t.settings.profileLabel}
                    </div>
                    <div className="flex items-center gap-3">
                      <img
                        src={avatarData ?? "/app-icon.png"}
                        alt={t.settings.avatarLabel}
                        className="h-9 w-9 rounded-[22%] object-cover ring-1 ring-[#2a2a2a]"
                      />
                      <div className="flex gap-2">
                        <button
                          type="button"
                          disabled={saving}
                          title={t.settings.changeAvatar}
                          onClick={() => {
                            if (!isTauri()) return;
                            void openDialog({
                              multiple: false,
                              title: "Choose avatar image",
                              filters: [
                                {
                                  name: "Images",
                                  extensions: ["png", "jpg", "jpeg", "webp", "gif"],
                                },
                              ],
                            }).then((picked) => {
                              const path = Array.isArray(picked) ? picked[0] : picked;
                              if (path) void setAvatar(path);
                            });
                          }}
                          className="soyo-pill-btn text-[#c9c9c9] disabled:opacity-40"
                        >
                          {t.settings.changeAvatar}
                        </button>
                        {avatarData && (
                          <button
                            type="button"
                            disabled={saving}
                            onClick={() => void setAvatar(null)}
                            className="soyo-pill-btn text-[#8a8a8a] disabled:opacity-40"
                          >
                            {t.settings.resetAvatar}
                          </button>
                        )}
                      </div>
                    </div>
                    <div className="space-y-1">
                      <label
                        htmlFor="settings-nickname-input"
                        className="block text-[12px] text-[#a3a3a3]"
                      >
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
                          className="soyo-input flex-1 px-2.5 py-1.5 text-xs text-[#d9d9d9]"
                        />
                        <button
                          type="button"
                          aria-label="Apply nickname"
                          disabled={saving}
                          onClick={() =>
                            void setNickname(nicknameInput.trim() || null)
                          }
                          className="soyo-pill-btn font-medium text-[#d9a08e] disabled:opacity-40"
                        >
                          {t.settings.apply}
                        </button>
                      </div>
                      <p className="text-[10.5px] text-[#6e6e6e]">
                        {t.settings.nicknameHint}
                      </p>
                    </div>
                  </div>
                )}

                {activeTab === "general" && (
                  <div className="space-y-2 rounded-[10px] border border-[#262626] bg-[#1a1a1a] p-3">
                    <div className="flex items-center justify-between">
                      <div className="text-[12px] font-medium text-[#e8e8e8]">
                        {t.settings.globalPromptLabel}
                      </div>
                      {promptSaved && (
                        <span className="text-[10.5px] text-[#4ade80]">
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
                      className="soyo-input w-full resize-y px-2.5 py-2 font-mono text-[11.5px] leading-relaxed text-[#d9d9d9]"
                    />
                    <div className="flex items-center justify-between gap-2">
                      <p className="min-w-0 truncate text-[10.5px] text-[#6e6e6e]">
                        {t.settings.globalPromptHint(promptPath || "~/.soyo/AGENTS.md")}
                      </p>
                      <button
                        type="button"
                        disabled={saving || !isTauri()}
                        onClick={() => {
                          void commands.setGlobalPrompt(promptText).then((res) => {
                            if (res.status === "ok") {
                              setPromptPath(res.data.path);
                              setPromptSaved(true);
                            }
                          });
                        }}
                        className="soyo-pill-btn shrink-0 font-medium text-[#d9a08e] disabled:opacity-40"
                      >
                        {t.settings.savePrompt}
                      </button>
                    </div>
                  </div>
                )}

                <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
                  <div className="space-y-1.5">
                    <label
                      htmlFor="settings-profile-select"
                      className="block text-[12px] text-[#a3a3a3]"
                    >
                      {t.settings.modelProfile}
                    </label>
                    <select
                      id="settings-profile-select"
                      value={bootstrap?.selected_profile ?? ""}
                      disabled={runActive || saving}
                      onChange={(e) =>
                        void runUpdate(() => commands.selectProfile(e.target.value))
                      }
                      className="soyo-select w-full px-2.5 py-1.5 text-xs text-[#d9d9d9]"
                    >
                      {(bootstrap?.profiles ?? []).map((p) => (
                        <option key={p.name} value={p.name} className="bg-[#181818]">
                          {p.name} ({p.model})
                        </option>
                      ))}
                    </select>
                  </div>

                  <div className="space-y-1.5">
                    <label
                      htmlFor="settings-effort-select"
                      className="block text-[12px] text-[#a3a3a3]"
                    >
                      {t.settings.thinkingEffort}
                    </label>
                    <select
                      id="settings-effort-select"
                      value={bootstrap?.thinking_effort ?? "default"}
                      disabled={runActive || saving}
                      onChange={(e) => {
                        const val = e.target.value;
                        void runUpdate(() =>
                          commands.setThinkingEffort(val === "default" ? null : val)
                        );
                      }}
                      className="soyo-select w-full px-2.5 py-1.5 text-xs text-[#d9d9d9]"
                    >
                      {EFFORTS.map((eff) => (
                        <option key={eff} value={eff} className="bg-[#181818]">
                          {eff}
                        </option>
                      ))}
                    </select>
                  </div>

                  <div className="space-y-1.5">
                    <label
                      htmlFor="settings-sandbox-select"
                      className="block text-[12px] text-[#a3a3a3]"
                    >
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
                      className="soyo-select w-full px-2.5 py-1.5 text-xs text-[#d9d9d9]"
                    >
                      {BACKENDS.map((bk) => (
                        <option key={bk} value={bk} className="bg-[#181818]">
                          {bk}
                        </option>
                      ))}
                    </select>
                  </div>
                </div>

                {activeTab === "general" && (
                  <div className="space-y-2 rounded-[10px] border border-[#262626] bg-[#141414] p-3">
                    <div className="flex items-center justify-between">
                      <span className="font-medium text-[#e8e8e8]">
                        {t.settings.autoCompact} ({bootstrap?.auto_compact_trigger_percent ?? 80}
                        %)
                      </span>
                      <button
                        type="button"
                        disabled={runActive || saving}
                        onClick={() =>
                          void runUpdate(() =>
                            commands.setAutocompact(!bootstrap?.auto_compact, null)
                          )
                        }
                        className={`soyo-pill font-semibold ${
                          bootstrap?.auto_compact
                            ? "text-[#4ade80]"
                            : "text-[#8a8a8a]"
                        } disabled:opacity-40`}
                      >
                        {bootstrap?.auto_compact ? t.settings.on : t.settings.off}
                      </button>
                    </div>
                    <div className="flex items-center gap-2">
                      <input
                        type="number"
                        aria-label="Auto-compact trigger tokens"
                        value={triggerInput}
                        disabled={runActive || saving}
                        onChange={(e) => setTriggerInput(e.target.value)}
                        placeholder={t.settings.triggerPlaceholder}
                        className="soyo-input flex-1 px-2.5 py-1 font-mono text-xs text-[#d9d9d9]"
                      />
                      <button
                        type="button"
                        disabled={runActive || saving}
                        onClick={() => {
                          const parsed = triggerInput.trim()
                            ? Number(triggerInput.trim())
                            : null;
                          void runUpdate(() =>
                            commands.setAutocompact(true, parsed)
                          );
                        }}
                        className="soyo-pill-btn text-[#e8e8e8] disabled:opacity-40"
                      >
                        {t.settings.setTrigger}
                      </button>
                    </div>
                  </div>
                )}
              </>
            )}

            {(activeTab === "general" || activeTab === "permissions") && (
              <div className="space-y-3 rounded-[10px] border border-[#262626] bg-[#141414] p-3">
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <span className="text-[12px] text-[#a3a3a3]">
                    {t.settings.workMode}
                  </span>
                  <div className="inline-flex gap-0.5 rounded-[8px] border border-[#2b2b2b] bg-[#1e1e1e] p-0.5">
                    {WORK_MODES.map((m) => (
                      <button
                        key={m}
                        type="button"
                        disabled={!onSelectMode}
                        onClick={() => void onSelectMode?.(m)}
                        className={`rounded-[6px] px-2.5 py-1 text-[11px] font-medium ${
                          workMode === m
                            ? "bg-[#2e2e2e] text-[#e8e8e8]"
                            : "text-[#8a8a8a] hover:text-[#e8e8e8]"
                        }`}
                      >
                        {WORK_MODE_STYLES[m].label}
                      </button>
                    ))}
                  </div>
                </div>

                <div className="flex items-start justify-between gap-4 border-t border-[#232323] pt-3">
                  <div className="space-y-1">
                    <div className="font-medium text-[#e8e8e8]">
                      {t.settings.allowBypassTitle}
                    </div>
                    <p className="text-[11px] leading-[1.5] text-[#7a7a7a]">
                      {t.settings.allowBypassDesc}
                    </p>
                  </div>
                  <button
                    type="button"
                    role="switch"
                    aria-checked={permissionMode === "bypass"}
                    aria-label="Toggle bypass permissions mode"
                    disabled={runActive || !onSelectPermission}
                    onClick={() =>
                      void onSelectPermission?.(
                        permissionMode === "bypass" ? "auto" : "bypass"
                      )
                    }
                    className="soyo-switch mt-0.5 disabled:opacity-40"
                  >
                    <span className="soyo-switch__knob" />
                  </button>
                </div>
              </div>
            )}

            {(activeTab === "general" || activeTab === "modules") && (
              <div className="border-t border-[#232323] pt-3">
                <ModulesPanel runActive={runActive} />
              </div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
