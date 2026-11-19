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
import { SquaresLoader } from "./SquaresLoader";
import { Button } from "./ui/Button";
import { DialogShell } from "./ui/DialogShell";

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
    <DialogShell
      open
      ariaLabel={t.setup.dialogLabel}
      testId="setup-wizard"
      size="md"
      panelClassName="flex max-w-[520px] flex-col"
    >
      {/* Header */}
      <div className="flex items-center gap-3 border-b border-[var(--border-subtle)] px-6 py-4">
        <img
          src="/app-icon.png"
          alt={t.about.productName}
          className="h-8 w-8 rounded-[22%] object-cover"
        />
        <div className="min-w-0 flex-1">
          <div className="text-[16px] font-semibold text-[var(--text-primary)]">
            {t.setup.welcome}
          </div>
          <div className="text-[12px] text-[var(--text-tertiary)]">
            {t.setup.welcomeSubtitle}
          </div>
        </div>
        <button
          type="button"
          onClick={onSkip}
          className="text-[11px] text-[var(--text-tertiary)] transition-colors hover:text-[var(--text-secondary)]"
        >
          {t.setup.skipLabel}
        </button>
      </div>

      {/* Step indicator */}
      <div className="flex items-center gap-1.5 px-6 pt-4">
        {STEPS.map((s, i) => (
          <div key={s} className="flex items-center gap-1.5">
            <span
              className={`flex h-5.5 w-5.5 items-center justify-center rounded-full text-[11px] ${
                i < stepIdx
                  ? "bg-[var(--accent)] text-[var(--on-accent)]"
                  : i === stepIdx
                    ? "border border-[var(--accent)] text-[var(--accent-text)]"
                    : "border border-[var(--border)] text-[var(--text-tertiary)]"
              }`}
            >
              {i < stepIdx ? (
                <Check className="h-3 w-3" strokeWidth={1.75} />
              ) : (
                stepIcon(s)
              )}
            </span>
            {i < STEPS.length - 1 && (
              <span className="h-px w-6 bg-[var(--border)]" />
            )}
          </div>
        ))}
        <span className="ml-auto text-[11px] text-[var(--text-tertiary)]">
          {t.setup.stepOf(stepIdx + 1, STEPS.length)}
        </span>
      </div>

      {/* Body */}
      <div className="min-h-[300px] px-6 py-5 text-xs">
        {bootstrap.config_error && !errorMsg && (
          <div className="mb-3 rounded-[var(--radius-md)] border border-[var(--warning)] bg-[var(--warning-surface)] px-3 py-2 text-[var(--warning)]">
            {bootstrap.config_error}
          </div>
        )}
        {errorMsg && (
          <div className="mb-3 rounded-[var(--radius-md)] border border-[var(--danger)] bg-[var(--danger-surface)] px-3 py-2 text-[var(--danger)]">
            {errorMsg}
          </div>
        )}

        {step === "language" && (
          <div className="space-y-2" data-testid="setup-step-language">
            <div className="mb-3 text-[12px] text-[var(--text-secondary)]">
              {t.setup.stepLanguage}
            </div>
            {LANGUAGE_OPTIONS.map((opt) => (
              <button
                key={opt.id}
                type="button"
                onClick={() => setLocale(opt.id)}
                className={`flex w-full items-center justify-between rounded-[var(--radius-md)] border px-3.5 py-2.5 text-left transition-colors ${
                  locale === opt.id
                    ? "border-[var(--accent)] bg-[var(--accent-surface)] text-[var(--text-primary)]"
                    : "border-[var(--border)] bg-[var(--surface-raised)] text-[var(--text-secondary)] hover:border-[var(--border-strong)]"
                }`}
              >
                <span className="text-[13px]">{opt.nativeLabel}</span>
                {locale === opt.id && (
                  <Check
                    className="h-3.5 w-3.5 text-[var(--accent-text)]"
                    strokeWidth={1.75}
                  />
                )}
              </button>
            ))}
          </div>
        )}

        {step === "workspace" && (
          <div className="space-y-3" data-testid="setup-step-workspace">
            <div>
              <div className="text-[12px] text-[var(--text-secondary)]">
                {t.setup.stepWorkspace}
              </div>
              <div className="mt-1 text-[11px] text-[var(--text-tertiary)]">
                {t.setup.workspaceHint}
              </div>
            </div>
            <div className="flex gap-2">
              <input
                type="text"
                value={workspace}
                onChange={(e) => setWorkspace(e.target.value)}
                className="soyo-input flex-1 px-2.5 py-1.5 font-mono text-xs text-[var(--text-primary)]"
                aria-label={t.setup.workspaceInputLabel}
              />
              <Button
                variant="secondary"
                size="sm"
                disabled={!isTauri()}
                onClick={() => {
                  void openDialog({
                    directory: true,
                    defaultPath: workspace || undefined,
                    title: t.setup.chooseWorkspaceTitle,
                  }).then((picked) => {
                    const path = Array.isArray(picked) ? picked[0] : picked;
                    if (path) setWorkspace(path);
                  });
                }}
              >
                {t.setup.browse}
              </Button>
            </div>
          </div>
        )}

        {step === "model" && (
          <div className="space-y-3" data-testid="setup-step-model">
            <div className="text-[12px] text-[var(--text-secondary)]">
              {t.setup.stepModel}
            </div>
            <div className="grid grid-cols-4 gap-1.5">
              {PRESETS.map((p) => (
                <button
                  key={p.id}
                  type="button"
                  onClick={() => choosePreset(p.id)}
                  className={`rounded-[var(--radius-md)] border px-2 py-1.5 text-[11px] transition-colors ${
                    presetId === p.id
                      ? "border-[var(--accent)] bg-[var(--accent-surface)] text-[var(--text-primary)]"
                      : "border-[var(--border)] bg-[var(--surface-raised)] text-[var(--text-secondary)] hover:border-[var(--border-strong)]"
                  }`}
                >
                  {t.setup.presets[p.id as keyof typeof t.setup.presets] ?? p.id}
                </button>
              ))}
            </div>
            <div className="grid grid-cols-2 gap-2">
              <label className="block">
                <span className="mb-1 block text-[11px] text-[var(--text-tertiary)]">
                  {t.setup.profileName}
                </span>
                <input
                  type="text"
                  value={profileName}
                  onChange={(e) => setProfileName(e.target.value)}
                  className="soyo-input w-full px-2.5 py-1.5 font-mono text-xs text-[var(--text-primary)]"
                />
              </label>
              <label className="block">
                <span className="mb-1 block text-[11px] text-[var(--text-tertiary)]">
                  {t.setup.contextWindow}
                </span>
                <input
                  type="number"
                  min={1}
                  value={contextWindow}
                  onChange={(e) => setContextWindow(e.target.value)}
                  className="soyo-input w-full px-2.5 py-1.5 font-mono text-xs text-[var(--text-primary)]"
                />
              </label>
            </div>
            <label className="block">
              <span className="mb-1 block text-[11px] text-[var(--text-tertiary)]">
                {t.setup.baseUrl}
              </span>
              <input
                type="text"
                value={baseUrl}
                onChange={(e) => setBaseUrl(e.target.value)}
                placeholder="https://api.openai.com/v1"
                className="soyo-input w-full px-2.5 py-1.5 font-mono text-xs text-[var(--text-primary)]"
              />
            </label>
            <label className="block">
              <span className="mb-1 block text-[11px] text-[var(--text-tertiary)]">
                {t.setup.modelName}
              </span>
              <input
                type="text"
                value={model}
                onChange={(e) => setModel(e.target.value)}
                placeholder="gpt-5 / claude-sonnet-4-5 / …"
                className="soyo-input w-full px-2.5 py-1.5 font-mono text-xs text-[var(--text-primary)]"
              />
            </label>

            {needsKey ? (
              <div className="space-y-2 rounded-[var(--radius-lg)] border border-[var(--border-subtle)] bg-[var(--surface-raised)] p-3">
                <div className="flex items-center gap-2 text-[12px] text-[var(--text-secondary)]">
                  <KeyRound className="h-3.5 w-3.5 text-[var(--text-tertiary)]" strokeWidth={1.75} />
                  {t.setup.apiKeyLabel}
                </div>
                <div className="flex gap-1.5 text-[11px]">
                  <button
                    type="button"
                    onClick={() => setKeyMode("paste")}
                    className={`rounded-[var(--radius-sm)] px-2.5 py-1 ${
                      keyMode === "paste"
                        ? "bg-[var(--surface-hover)] text-[var(--text-primary)]"
                        : "text-[var(--text-tertiary)] hover:text-[var(--text-secondary)]"
                    }`}
                  >
                    {t.setup.pasteKeyLabel}
                  </button>
                  <button
                    type="button"
                    onClick={() => setKeyMode("env")}
                    className={`rounded-[var(--radius-sm)] px-2.5 py-1 ${
                      keyMode === "env"
                        ? "bg-[var(--surface-hover)] text-[var(--text-primary)]"
                        : "text-[var(--text-tertiary)] hover:text-[var(--text-secondary)]"
                    }`}
                  >
                    {t.setup.useEnvVarLabel}
                  </button>
                </div>
                <label className="block">
                  <span className="mb-1 block text-[11px] text-[var(--text-tertiary)]">
                    {t.setup.envVarNameLabel}
                  </span>
                  <input
                    type="text"
                    value={envName}
                    onChange={(e) => setEnvName(e.target.value)}
                    className="soyo-input w-full px-2.5 py-1.5 font-mono text-xs text-[var(--text-primary)]"
                  />
                </label>
                {keyMode === "paste" && (
                  <>
                    <input
                      type="password"
                      value={apiKey}
                      onChange={(e) => setApiKey(e.target.value)}
                      placeholder="sk-…"
                      aria-label={t.setup.apiKeyLabel}
                      className="soyo-input w-full px-2.5 py-1.5 font-mono text-xs text-[var(--text-primary)]"
                    />
                    <div className="text-[11px] leading-relaxed text-[var(--text-tertiary)]">
                      {t.setup.apiKeyHint}
                    </div>
                  </>
                )}
              </div>
            ) : (
              <div className="rounded-[var(--radius-lg)] border border-[var(--border-subtle)] bg-[var(--surface-raised)] p-3 text-[11px] text-[var(--text-tertiary)]">
                {t.setup.noKeyNeeded}
              </div>
            )}
          </div>
        )}

        {step === "permissions" && (
          <div className="space-y-2" data-testid="setup-step-permissions">
            <div className="mb-3 text-[12px] text-[var(--text-secondary)]">
              {t.setup.stepPermissions}
            </div>
            {(
              [
                ["auto", t.setup.permAuto, t.setup.permAutoHint],
                ["manual", t.setup.permManual, t.setup.permManualHint],
                ["bypass", t.setup.permBypass, t.setup.permBypassHint],
              ] as const
            ).map(([id, label, hint]) => {
              const selected = permission === id;
              const isBypass = id === "bypass";
              return (
                <button
                  key={id}
                  type="button"
                  onClick={() => setPermission(id)}
                  className={`flex w-full items-start gap-3 rounded-[var(--radius-md)] border px-3.5 py-2.5 text-left transition-colors ${
                    selected
                      ? isBypass
                        ? "border-[var(--danger)] bg-[var(--danger-surface)]"
                        : "border-[var(--accent)] bg-[var(--accent-surface)]"
                      : "border-[var(--border)] bg-[var(--surface-raised)] hover:border-[var(--border-strong)]"
                  }`}
                >
                  <span
                    className={`mt-0.5 flex h-3.5 w-3.5 shrink-0 items-center justify-center rounded-full border ${
                      selected
                        ? isBypass
                          ? "border-[var(--danger)] bg-[var(--danger)] text-[var(--on-accent)]"
                          : "border-[var(--accent)] bg-[var(--accent)] text-[var(--on-accent)]"
                        : "border-[var(--border-strong)]"
                    }`}
                  >
                    {selected && <Check className="h-2.5 w-2.5" strokeWidth={2.2} />}
                  </span>
                  <span>
                    <span
                      className={`block text-[13px] ${
                        isBypass && selected
                          ? "font-medium text-[var(--danger)]"
                          : "text-[var(--text-primary)]"
                      }`}
                    >
                      {label}
                    </span>
                    <span className="mt-0.5 block text-[11px] text-[var(--text-tertiary)]">
                      {hint}
                    </span>
                  </span>
                </button>
              );
            })}
          </div>
        )}
      </div>

      {/* Footer */}
      <div className="flex items-center justify-between border-t border-[var(--border-subtle)] px-6 py-4">
        <button
          type="button"
          disabled={stepIdx === 0 || applying}
          onClick={() => {
            setErrorMsg(null);
            setStepIdx((i) => Math.max(0, i - 1));
          }}
          className="inline-flex items-center gap-1.5 text-[12px] text-[var(--text-tertiary)] transition-colors hover:text-[var(--text-primary)] disabled:opacity-30"
        >
          <ArrowLeft className="h-3.5 w-3.5" strokeWidth={1.75} />
          {t.setup.backLabel}
        </button>
        {stepIdx < STEPS.length - 1 ? (
          <Button
            variant="secondary"
            size="md"
            disabled={!canContinue}
            onClick={() => {
              setErrorMsg(null);
              setStepIdx((i) => Math.min(STEPS.length - 1, i + 1));
            }}
            className="text-[var(--accent-text)]"
          >
            {t.setup.continueLabel}
            <ArrowRight className="h-3.5 w-3.5" strokeWidth={1.75} />
          </Button>
        ) : (
          <Button
            variant="primary"
            size="md"
            disabled={applying || !isTauri()}
            onClick={() => void finish()}
          >
            {applying && <SquaresLoader size={12} />}
            {applying ? t.setup.applyingLabel : t.setup.finishLabel}
          </Button>
        )}
      </div>
    </DialogShell>
  );
}
