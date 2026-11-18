import {
  ArrowLeft,
  ArrowRight,
  Check,
  Cpu,
  FolderOpen,
  Globe,
  KeyRound,
  Shield,
} from "lucide-react";
import { isTauri } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { useState } from "react";
import {
  commands,
  type BootstrapStateDto_Serialize,
  type PermissionModeDto,
} from "../bindings";
import { LANGUAGE_OPTIONS, getMessages } from "../lib/i18n";
import { useAppStore } from "../store/useAppStore";

export interface SetupWizardProps {
  bootstrap: BootstrapStateDto_Serialize;
  onFinish: () => void;
  onSkip: () => void;
  onBootstrapUpdated: (boot: BootstrapStateDto_Serialize) => void;
}

interface Preset {
  id: string;
  protocol: string;
  baseUrl: string;
  auth: "bearer" | "x-api-key" | "none";
  keyEnv: string | null;
  contextWindow: number;
}

const PRESETS: Preset[] = [
  {
    id: "openai",
    protocol: "openai-responses",
    baseUrl: "https://api.openai.com/v1",
    auth: "bearer",
    keyEnv: "OPENAI_API_KEY",
    contextWindow: 128000,
  },
  {
    id: "anthropic",
    protocol: "anthropic-messages",
    baseUrl: "https://api.anthropic.com/v1",
    auth: "x-api-key",
    keyEnv: "ANTHROPIC_API_KEY",
    contextWindow: 200000,
  },
  {
    id: "compatible",
    protocol: "openai-chat-completions",
    baseUrl: "",
    auth: "bearer",
    keyEnv: "OPENAI_API_KEY",
    contextWindow: 128000,
  },
  {
    id: "local",
    protocol: "openai-chat-completions",
    baseUrl: "http://localhost:11434/v1",
    auth: "none",
    keyEnv: null,
    contextWindow: 65536,
  },
];

const STEPS = ["language", "workspace", "model", "permissions"] as const;
type Step = (typeof STEPS)[number];

export function SetupWizard({
  bootstrap,
  onFinish,
  onSkip,
  onBootstrapUpdated,
}: SetupWizardProps) {
  const locale = useAppStore((s) => s.locale);
  const setLocale = useAppStore((s) => s.setLocale);
  const t = getMessages(locale);

  const [stepIdx, setStepIdx] = useState(0);
  const step = STEPS[stepIdx];

  const [workspace, setWorkspace] = useState(bootstrap.workspace);
  const [presetId, setPresetId] = useState<string>("openai");
  const preset = PRESETS.find((p) => p.id === presetId) ?? PRESETS[0];
  const [profileName, setProfileName] = useState("openai");
  const [baseUrl, setBaseUrl] = useState(PRESETS[0].baseUrl);
  const [model, setModel] = useState("");
  const [contextWindow, setContextWindow] = useState(
    String(PRESETS[0].contextWindow)
  );
  const [keyMode, setKeyMode] = useState<"paste" | "env">("paste");
  const [apiKey, setApiKey] = useState("");
  const [envName, setEnvName] = useState(PRESETS[0].keyEnv ?? "");
  const [permission, setPermission] = useState<PermissionModeDto>("auto");

  const [applying, setApplying] = useState(false);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);

  const choosePreset = (id: string) => {
    const next = PRESETS.find((p) => p.id === id);
    if (!next) return;
    setPresetId(id);
    setProfileName(id);
    setBaseUrl(next.baseUrl);
    setContextWindow(String(next.contextWindow));
    setEnvName(next.keyEnv ?? "");
    setErrorMsg(null);
  };

  const needsKey = preset.auth !== "none";
  const modelStepValid =
    baseUrl.trim() !== "" &&
    model.trim() !== "" &&
    Number(contextWindow) > 0 &&
    (!needsKey ||
      (keyMode === "env" ? envName.trim() !== "" : apiKey.trim() !== ""));

  const canContinue =
    step === "language" ||
    (step === "workspace" && workspace.trim() !== "") ||
    (step === "model" && modelStepValid) ||
    step === "permissions";

  const finish = async () => {
    setErrorMsg(null);
    setApplying(true);
    try {
      const res = await commands.setupEnvironment({
        profile_name: profileName.trim() || preset.id,
        protocol: preset.protocol,
        base_url: baseUrl.trim(),
        model: model.trim(),
        auth: preset.auth,
        api_key_env: needsKey ? envName.trim() || null : null,
        api_key: needsKey && keyMode === "paste" ? apiKey.trim() || null : null,
        context_window_tokens: Number(contextWindow) || null,
        max_output_tokens: 8192,
        workspace: workspace.trim() || null,
        permission_mode: permission,
        work_mode: null,
      });
      if (res.status === "ok") {
        onBootstrapUpdated(res.data);
        onFinish();
      } else {
        setErrorMsg(`${res.error.code}: ${res.error.message}`);
      }
    } finally {
      setApplying(false);
    }
  };

  const stepIcon = (s: Step) => {
    switch (s) {
      case "language":
        return <Globe className="h-3.5 w-3.5" />;
      case "workspace":
        return <FolderOpen className="h-3.5 w-3.5" />;
      case "model":
        return <Cpu className="h-3.5 w-3.5" />;
      case "permissions":
        return <Shield className="h-3.5 w-3.5" />;
    }
  };

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label="Soyo setup"
      data-testid="setup-wizard"
      className="fixed inset-0 z-[60] flex items-center justify-center bg-[#0d0d0d] p-6"
    >
      <div className="soyo-dialog-enter flex w-full max-w-[520px] flex-col rounded-[16px] border border-[#2b2b2b] bg-[#181818] shadow-[0_24px_60px_rgba(0,0,0,0.65)]">
        {/* Header */}
        <div className="flex items-center gap-3 border-b border-[#232323] px-6 py-4">
          <img
            src="/app-icon.png"
            alt="Soyo"
            className="h-8 w-8 rounded-[22%] object-cover"
          />
          <div className="min-w-0 flex-1">
            <div className="text-[15px] font-semibold text-[#e8e8e8]">
              {t.setup.welcome}
            </div>
            <div className="text-[11.5px] text-[#8a8a8a]">
              {t.setup.welcomeSubtitle}
            </div>
          </div>
          <button
            type="button"
            onClick={onSkip}
            className="text-[11px] text-[#6e6e6e] transition-colors hover:text-[#a3a3a3]"
          >
            {t.setup.skipLabel}
          </button>
        </div>

        {/* Step indicator */}
        <div className="flex items-center gap-1.5 px-6 pt-4">
          {STEPS.map((s, i) => (
            <div key={s} className="flex items-center gap-1.5">
              <span
                className={`flex h-5.5 w-5.5 items-center justify-center rounded-full text-[10px] ${
                  i < stepIdx
                    ? "bg-[#d9a08e] text-[#181818]"
                    : i === stepIdx
                      ? "border border-[#d9a08e] text-[#d9a08e]"
                      : "border border-[#3a3a3a] text-[#5e5e5e]"
                }`}
              >
                {i < stepIdx ? <Check className="h-3 w-3" /> : stepIcon(s)}
              </span>
              {i < STEPS.length - 1 && (
                <span className="h-px w-6 bg-[#2e2e2e]" />
              )}
            </div>
          ))}
          <span className="ml-auto text-[10.5px] text-[#6e6e6e]">
            {t.setup.stepOf(stepIdx + 1, STEPS.length)}
          </span>
        </div>

        {/* Body */}
        <div className="min-h-[300px] px-6 py-5 text-xs">
          {bootstrap.config_error && !errorMsg && (
            <div className="mb-3 rounded-[8px] border border-[#4a3f2a] bg-[#1c1a15] px-3 py-2 text-[#e8d9b0]">
              {bootstrap.config_error}
            </div>
          )}
          {errorMsg && (
            <div className="mb-3 rounded-[8px] border border-[#f87171]/35 bg-[#1c1515] px-3 py-2 text-[#f87171]">
              {errorMsg}
            </div>
          )}

          {step === "language" && (
            <div className="space-y-2" data-testid="setup-step-language">
              <div className="mb-3 text-[12px] text-[#a3a3a3]">
                {t.setup.stepLanguage}
              </div>
              {LANGUAGE_OPTIONS.map((opt) => (
                <button
                  key={opt.id}
                  type="button"
                  onClick={() => setLocale(opt.id)}
                  className={`flex w-full items-center justify-between rounded-[9px] border px-3.5 py-2.5 text-left transition-colors ${
                    locale === opt.id
                      ? "border-[#d9a08e] bg-[#211d1b] text-[#e8e8e8]"
                      : "border-[#2b2b2b] bg-[#1a1a1a] text-[#c9c9c9] hover:border-[#3a3a3a]"
                  }`}
                >
                  <span className="text-[12.5px]">{opt.nativeLabel}</span>
                  {locale === opt.id && (
                    <Check className="h-3.5 w-3.5 text-[#d9a08e]" />
                  )}
                </button>
              ))}
            </div>
          )}

          {step === "workspace" && (
            <div className="space-y-3" data-testid="setup-step-workspace">
              <div>
                <div className="text-[12px] text-[#a3a3a3]">
                  {t.setup.stepWorkspace}
                </div>
                <div className="mt-1 text-[11px] text-[#6e6e6e]">
                  {t.setup.workspaceHint}
                </div>
              </div>
              <div className="flex gap-2">
                <input
                  type="text"
                  value={workspace}
                  onChange={(e) => setWorkspace(e.target.value)}
                  className="soyo-input flex-1 px-2.5 py-1.5 font-mono text-xs text-[#d9d9d9]"
                  aria-label="Workspace directory"
                />
                <button
                  type="button"
                  disabled={!isTauri()}
                  onClick={() => {
                    void openDialog({
                      directory: true,
                      defaultPath: workspace || undefined,
                      title: "Choose workspace directory",
                    }).then((picked) => {
                      const path = Array.isArray(picked) ? picked[0] : picked;
                      if (path) setWorkspace(path);
                    });
                  }}
                  className="soyo-pill-btn font-medium text-[#c9c9c9] disabled:opacity-40"
                >
                  {t.setup.browse}
                </button>
              </div>
            </div>
          )}

          {step === "model" && (
            <div className="space-y-3" data-testid="setup-step-model">
              <div className="text-[12px] text-[#a3a3a3]">
                {t.setup.stepModel}
              </div>
              <div className="grid grid-cols-4 gap-1.5">
                {PRESETS.map((p) => (
                  <button
                    key={p.id}
                    type="button"
                    onClick={() => choosePreset(p.id)}
                    className={`rounded-[8px] border px-2 py-1.5 text-[11px] transition-colors ${
                      presetId === p.id
                        ? "border-[#d9a08e] bg-[#211d1b] text-[#e8e8e8]"
                        : "border-[#2b2b2b] bg-[#1a1a1a] text-[#a3a3a3] hover:border-[#3a3a3a]"
                    }`}
                  >
                    {t.setup.presets[p.id as keyof typeof t.setup.presets] ??
                      p.id}
                  </button>
                ))}
              </div>
              <div className="grid grid-cols-2 gap-2">
                <label className="block">
                  <span className="mb-1 block text-[11px] text-[#8a8a8a]">
                    {t.setup.profileName}
                  </span>
                  <input
                    type="text"
                    value={profileName}
                    onChange={(e) => setProfileName(e.target.value)}
                    className="soyo-input w-full px-2.5 py-1.5 font-mono text-xs text-[#d9d9d9]"
                  />
                </label>
                <label className="block">
                  <span className="mb-1 block text-[11px] text-[#8a8a8a]">
                    {t.setup.contextWindow}
                  </span>
                  <input
                    type="number"
                    min={1}
                    value={contextWindow}
                    onChange={(e) => setContextWindow(e.target.value)}
                    className="soyo-input w-full px-2.5 py-1.5 font-mono text-xs text-[#d9d9d9]"
                  />
                </label>
              </div>
              <label className="block">
                <span className="mb-1 block text-[11px] text-[#8a8a8a]">
                  {t.setup.baseUrl}
                </span>
                <input
                  type="text"
                  value={baseUrl}
                  onChange={(e) => setBaseUrl(e.target.value)}
                  placeholder="https://api.openai.com/v1"
                  className="soyo-input w-full px-2.5 py-1.5 font-mono text-xs text-[#d9d9d9]"
                />
              </label>
              <label className="block">
                <span className="mb-1 block text-[11px] text-[#8a8a8a]">
                  {t.setup.modelName}
                </span>
                <input
                  type="text"
                  value={model}
                  onChange={(e) => setModel(e.target.value)}
                  placeholder="gpt-5 / claude-sonnet-4-5 / …"
                  className="soyo-input w-full px-2.5 py-1.5 font-mono text-xs text-[#d9d9d9]"
                />
              </label>

              {needsKey ? (
                <div className="space-y-2 rounded-[10px] border border-[#262626] bg-[#1a1a1a] p-3">
                  <div className="flex items-center gap-2 text-[11.5px] text-[#c9c9c9]">
                    <KeyRound className="h-3.5 w-3.5 text-[#8a8a8a]" />
                    {t.setup.apiKeyLabel}
                  </div>
                  <div className="flex gap-1.5 text-[11px]">
                    <button
                      type="button"
                      onClick={() => setKeyMode("paste")}
                      className={`rounded-[6px] px-2.5 py-1 ${
                        keyMode === "paste"
                          ? "bg-[#2e2e2e] text-[#e8e8e8]"
                          : "text-[#8a8a8a] hover:text-[#c9c9c9]"
                      }`}
                    >
                      {t.setup.pasteKeyLabel}
                    </button>
                    <button
                      type="button"
                      onClick={() => setKeyMode("env")}
                      className={`rounded-[6px] px-2.5 py-1 ${
                        keyMode === "env"
                          ? "bg-[#2e2e2e] text-[#e8e8e8]"
                          : "text-[#8a8a8a] hover:text-[#c9c9c9]"
                      }`}
                    >
                      {t.setup.useEnvVarLabel}
                    </button>
                  </div>
                  <label className="block">
                    <span className="mb-1 block text-[11px] text-[#8a8a8a]">
                      {t.setup.envVarNameLabel}
                    </span>
                    <input
                      type="text"
                      value={envName}
                      onChange={(e) => setEnvName(e.target.value)}
                      className="soyo-input w-full px-2.5 py-1.5 font-mono text-xs text-[#d9d9d9]"
                    />
                  </label>
                  {keyMode === "paste" && (
                    <>
                      <input
                        type="password"
                        value={apiKey}
                        onChange={(e) => setApiKey(e.target.value)}
                        placeholder="sk-…"
                        aria-label="API key"
                        className="soyo-input w-full px-2.5 py-1.5 font-mono text-xs text-[#d9d9d9]"
                      />
                      <div className="text-[10.5px] leading-relaxed text-[#6e6e6e]">
                        {t.setup.apiKeyHint}
                      </div>
                    </>
                  )}
                </div>
              ) : (
                <div className="rounded-[10px] border border-[#262626] bg-[#1a1a1a] p-3 text-[11px] text-[#8a8a8a]">
                  {t.setup.noKeyNeeded}
                </div>
              )}
            </div>
          )}

          {step === "permissions" && (
            <div className="space-y-2" data-testid="setup-step-permissions">
              <div className="mb-3 text-[12px] text-[#a3a3a3]">
                {t.setup.stepPermissions}
              </div>
              {(
                [
                  ["auto", t.setup.permAuto, t.setup.permAutoHint],
                  ["manual", t.setup.permManual, t.setup.permManualHint],
                  ["bypass", t.setup.permBypass, t.setup.permBypassHint],
                ] as const
              ).map(([id, label, hint]) => (
                <button
                  key={id}
                  type="button"
                  onClick={() => setPermission(id)}
                  className={`flex w-full items-start gap-3 rounded-[9px] border px-3.5 py-2.5 text-left transition-colors ${
                    permission === id
                      ? "border-[#d9a08e] bg-[#211d1b]"
                      : "border-[#2b2b2b] bg-[#1a1a1a] hover:border-[#3a3a3a]"
                  }`}
                >
                  <span
                    className={`mt-0.5 flex h-3.5 w-3.5 shrink-0 items-center justify-center rounded-full border ${
                      permission === id
                        ? "border-[#d9a08e] bg-[#d9a08e]"
                        : "border-[#4a4a4a]"
                    }`}
                  >
                    {permission === id && (
                      <Check className="h-2.5 w-2.5 text-[#181818]" />
                    )}
                  </span>
                  <span>
                    <span className="block text-[12.5px] text-[#e8e8e8]">
                      {label}
                    </span>
                    <span className="mt-0.5 block text-[11px] text-[#8a8a8a]">
                      {hint}
                    </span>
                  </span>
                </button>
              ))}
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-between border-t border-[#232323] px-6 py-4">
          <button
            type="button"
            disabled={stepIdx === 0 || applying}
            onClick={() => {
              setErrorMsg(null);
              setStepIdx((i) => Math.max(0, i - 1));
            }}
            className="inline-flex items-center gap-1.5 text-[12px] text-[#8a8a8a] transition-colors hover:text-[#e8e8e8] disabled:opacity-30"
          >
            <ArrowLeft className="h-3.5 w-3.5" />
            {t.setup.backLabel}
          </button>
          {stepIdx < STEPS.length - 1 ? (
            <button
              type="button"
              disabled={!canContinue}
              onClick={() => {
                setErrorMsg(null);
                setStepIdx((i) => Math.min(STEPS.length - 1, i + 1));
              }}
              className="soyo-pill-btn inline-flex items-center gap-1.5 px-4 py-1.5 text-[12px] font-medium text-[#d9a08e] disabled:opacity-40"
            >
              {t.setup.continueLabel}
              <ArrowRight className="h-3.5 w-3.5" />
            </button>
          ) : (
            <button
              type="button"
              disabled={applying || !isTauri()}
              onClick={() => void finish()}
              className="soyo-pill-btn inline-flex items-center gap-1.5 bg-[#d9a08e] px-4 py-1.5 text-[12px] font-semibold text-[#181818] disabled:opacity-40"
            >
              {applying ? t.setup.applyingLabel : t.setup.finishLabel}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
