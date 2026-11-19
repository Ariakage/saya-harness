import { AlertTriangle, Blocks, Briefcase, Puzzle, XCircle } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import {
  commands,
  type BackgroundJobDto_Serialize,
  type ExtensionItemDto_Serialize,
  type ModuleStatusDto_Serialize,
} from "../bindings";
import { getMessages } from "../lib/i18n";
import { sanitizeDisplayText } from "../lib/sanitize";
import { useAppStore } from "../store/useAppStore";
import { Button } from "./ui/Button";
import { Pill } from "./ui/Pill";

export interface ModulesPanelProps {
  runActive: boolean;
}

export function ModulesPanel({ runActive }: ModulesPanelProps) {
  const locale = useAppStore((s) => s.locale);
  const t = getMessages(locale);
  const [modules, setModules] = useState<ModuleStatusDto_Serialize[]>([]);
  const [extensions, setExtensions] = useState<ExtensionItemDto_Serialize[]>([]);
  const [jobs, setJobs] = useState<BackgroundJobDto_Serialize[]>([]);
  const [statusMessage, setStatusMessage] = useState<string | null>(null);

  const loadAll = useCallback(async () => {
    const [modRes, extRes, jobRes] = await Promise.all([
      commands.listModules(),
      commands.listExtensions(),
      commands.listJobs(),
    ]);
    if (modRes.status === "ok" && Array.isArray(modRes.data)) setModules(modRes.data);
    if (extRes.status === "ok" && Array.isArray(extRes.data)) setExtensions(extRes.data);
    if (jobRes.status === "ok" && Array.isArray(jobRes.data)) setJobs(jobRes.data);
  }, []);

  useEffect(() => {
    void loadAll();
  }, [loadAll]);

  const handleToggleModule = async (mod: ModuleStatusDto_Serialize) => {
    setStatusMessage(null);
    const res = await commands.setModuleEnabled(mod.module, !mod.enabled);
    if (res.status === "ok") {
      setModules(res.data);
    } else {
      setStatusMessage(`${res.error.code}: ${res.error.message}`);
    }
  };

  const handleCancelJob = async (jobId: string) => {
    setStatusMessage(null);
    const res = await commands.cancelBackgroundJob(jobId);
    if (res.status === "ok") {
      await loadAll();
    } else {
      setStatusMessage(`${res.error.code}: ${res.error.message}`);
    }
  };

  return (
    <div className="space-y-4 text-xs" data-testid="modules-panel">
      {statusMessage && (
        <div className="rounded-[var(--radius-md)] border border-[var(--danger)] bg-[var(--danger-surface)] px-2.5 py-1.5 text-[11px] text-[var(--danger)]">
          {statusMessage}
        </div>
      )}

      <section className="space-y-2">
        <div className="flex items-center gap-1.5 font-medium text-[var(--text-primary)]">
          <Blocks className="h-3.5 w-3.5 text-[var(--accent-text)]" strokeWidth={1.75} />
          <span>{t.modulesPanel.runtimeModules}</span>
        </div>
        {modules.length === 0 ? (
          <div className="text-[11px] text-[var(--text-tertiary)]">
            {t.modulesPanel.noModules}
          </div>
        ) : (
          <ul className="space-y-2">
            {modules.map((mod) => (
              <li
                key={mod.module}
                className="flex items-start justify-between gap-3 rounded-[var(--radius-md)] border border-[var(--border-subtle)] bg-[var(--surface-raised)] p-2.5"
              >
                <div className="min-w-0 space-y-1">
                  <div className="flex items-center gap-2">
                    <span className="font-mono font-semibold text-[var(--text-primary)]">
                      {mod.module}
                    </span>
                    <Pill tone="neutral">{mod.source}</Pill>
                  </div>
                  <div className="text-[11px] text-[var(--text-tertiary)]">
                    {sanitizeDisplayText(mod.description)}
                  </div>
                  {mod.issue && (
                    <div className="flex items-center gap-1 text-[11px] text-[var(--warning)]">
                      <AlertTriangle className="h-3 w-3 shrink-0" strokeWidth={1.75} />
                      <span>{sanitizeDisplayText(mod.issue)}</span>
                    </div>
                  )}
                </div>
                <Button
                  variant="secondary"
                  size="sm"
                  disabled={runActive}
                  onClick={() => void handleToggleModule(mod)}
                  className={
                    mod.enabled ? "text-[var(--success)]" : "text-[var(--text-tertiary)]"
                  }
                >
                  {mod.enabled ? t.modulesPanel.enabled : t.modulesPanel.disabled}
                </Button>
              </li>
            ))}
          </ul>
        )}
      </section>

      <section className="space-y-2">
        <div className="flex items-center gap-1.5 font-medium text-[var(--text-primary)]">
          <Puzzle className="h-3.5 w-3.5 text-[var(--ultra-bright)]" strokeWidth={1.75} />
          <span>{t.modulesPanel.extensionsHeader(extensions.length)}</span>
        </div>
        {extensions.length === 0 ? (
          <div className="text-[11px] text-[var(--text-tertiary)]">
            {t.modulesPanel.noExtensions}
          </div>
        ) : (
          <ul className="max-h-40 space-y-1 overflow-y-auto">
            {extensions.map((ext) => (
              <li
                key={`${ext.kind}:${ext.id}`}
                className="flex items-center justify-between gap-2 rounded-[var(--radius-md)] border border-[var(--border-subtle)] bg-[var(--surface-raised)] px-2.5 py-1.5 text-[11px]"
              >
                <div className="min-w-0">
                  <span className="mr-1.5 rounded bg-[var(--info-surface)] px-1.5 py-0.5 font-mono text-[11px] text-[var(--info)]">
                    {ext.kind}
                  </span>
                  <span className="font-medium text-[var(--text-primary)]">
                    {sanitizeDisplayText(ext.name)}
                  </span>
                </div>
                <span className="shrink-0 font-mono text-[11px] text-[var(--text-tertiary)]">
                  {ext.scope}
                </span>
              </li>
            ))}
          </ul>
        )}
      </section>

      <section className="space-y-2">
        <div className="flex items-center gap-1.5 font-medium text-[var(--text-primary)]">
          <Briefcase className="h-3.5 w-3.5 text-[var(--warning)]" strokeWidth={1.75} />
          <span>{t.modulesPanel.jobsHeader(jobs.length)}</span>
        </div>
        {jobs.length === 0 ? (
          <div className="text-[11px] text-[var(--text-tertiary)]">
            {t.modulesPanel.noJobs}
          </div>
        ) : (
          <ul className="max-h-36 space-y-1 overflow-y-auto">
            {jobs.map((job) => (
              <li
                key={job.job_id}
                className="flex items-center justify-between gap-2 rounded-[var(--radius-md)] border border-[var(--border-subtle)] bg-[var(--surface-raised)] px-2.5 py-1.5 text-[11px]"
              >
                <div className="min-w-0">
                  <div className="truncate font-mono text-[var(--text-primary)]">
                    {job.tool ?? t.modulesPanel.jobFallback} · {job.job_id}
                  </div>
                  <div className="truncate text-[11px] text-[var(--text-tertiary)]">
                    {t.modulesPanel.stateLabel}: {job.state}
                    {job.summary ? ` · ${sanitizeDisplayText(job.summary)}` : ""}
                  </div>
                </div>
                {job.state === "running" && runActive && (
                  <Button
                    variant="danger"
                    size="sm"
                    onClick={() => void handleCancelJob(job.job_id)}
                  >
                    <XCircle className="h-3 w-3" strokeWidth={1.75} />
                    {t.modulesPanel.cancel}
                  </Button>
                )}
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}
