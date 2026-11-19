import { isTauri } from "@tauri-apps/api/core";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { Download, X } from "lucide-react";
import { useState } from "react";
import {
  commands,
  type ExportFormatDto,
  type ExportResultDto,
  type ExportScopeDto,
} from "../bindings";
import { getMessages } from "../lib/i18n";
import { useAppStore } from "../store/useAppStore";
import { SquaresLoader } from "./SquaresLoader";
import { Button } from "./ui/Button";
import { DialogShell } from "./ui/DialogShell";

export interface ExportDialogProps {
  open: boolean;
  embedded?: boolean;
  sessionId: string | null;
  workspace: string;
  onClose: () => void;
}

export function ExportDialog({
  open,
  embedded = false,
  sessionId,
  workspace,
  onClose,
}: ExportDialogProps) {
  const locale = useAppStore((s) => s.locale);
  const t = getMessages(locale);
  const [targetPath, setTargetPath] = useState("session-export.md");
  const [format, setFormat] = useState<ExportFormatDto>("markdown");
  const [scope, setScope] = useState<ExportScopeDto>("transcript");
  const [includeTools, setIncludeTools] = useState(true);
  const [result, setResult] = useState<ExportResultDto | null>(null);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  if (!open) {
    return null;
  }

  const handleExport = async () => {
    if (!targetPath.trim()) return;
    setErrorMsg(null);
    setResult(null);
    setBusy(true);
    try {
      const res = await commands.exportSession(
        sessionId,
        targetPath.trim(),
        format,
        scope,
        includeTools
      );
      if (res.status === "ok") {
        setResult(res.data);
      } else {
        setErrorMsg(`${res.error.code}: ${res.error.message}`);
      }
    } finally {
      setBusy(false);
    }
  };

  const body = (
    <div className="space-y-3 text-xs">
      {!sessionId && (
        <div className="rounded-[var(--radius-md)] border border-[var(--border-subtle)] bg-[var(--surface-raised)] px-3 py-2 text-[var(--text-secondary)]">
          {t.extra.exportUnavailableHint}
        </div>
      )}

      {errorMsg && (
        <div className="rounded-[var(--radius-md)] border border-[var(--danger)] bg-[var(--danger-surface)] px-3 py-2 text-[var(--danger)]">
          {errorMsg}
        </div>
      )}

      {result && (
        <div className="space-y-1 rounded-[var(--radius-md)] border border-[var(--success)] bg-[var(--success-surface)] p-2.5 text-[var(--success)]">
          <div className="font-medium">{t.exportDialog.published}</div>
          <div className="truncate font-mono text-[11px]">
            {result.path} ({t.exportDialog.resultSummary(String(result.bytes), result.format)})
          </div>
        </div>
      )}

      <div className="space-y-1">
        <label
          htmlFor="export-target-path"
          className="block text-[12px] text-[var(--text-secondary)]"
        >
          {t.exportDialog.outputPathLabel(workspace || t.inspector.workspace)}
        </label>
        <div className="flex gap-2">
          <input
            id="export-target-path"
            type="text"
            value={targetPath}
            onChange={(e) => setTargetPath(e.target.value)}
            className="soyo-input flex-1 px-2.5 py-1.5 font-mono text-xs"
          />
          {isTauri() && (
            <Button
              variant="secondary"
              size="sm"
              title={t.exportDialog.chooseLocation}
              onClick={() => {
                void saveDialog({
                  defaultPath: targetPath || undefined,
                  title: t.exportDialog.title,
                  filters: [
                    format === "json"
                      ? { name: "JSON", extensions: ["json"] }
                      : { name: "Markdown", extensions: ["md"] },
                  ],
                }).then((picked) => {
                  if (picked) setTargetPath(picked);
                });
              }}
            >
              {t.exportDialog.browse}
            </Button>
          )}
        </div>
      </div>

      <div className="grid grid-cols-2 gap-3">
        <div className="space-y-1">
          <label
            htmlFor="export-format-select"
            className="block text-[12px] text-[var(--text-secondary)]"
          >
            {t.exportDialog.format}
          </label>
          <select
            id="export-format-select"
            value={format}
            onChange={(e) => {
              const next = e.target.value as ExportFormatDto;
              setFormat(next);
              setTargetPath((prev) =>
                prev.replace(/\.(md|json)$/i, next === "json" ? ".json" : ".md")
              );
            }}
            className="soyo-select w-full px-2.5 py-1.5 text-xs"
          >
            <option value="markdown" className="bg-[var(--surface-overlay)]">
              Markdown (.md)
            </option>
            <option value="json" className="bg-[var(--surface-overlay)]">
              JSON v1 (.json)
            </option>
          </select>
        </div>

        <div className="space-y-1">
          <label
            htmlFor="export-scope-select"
            className="block text-[12px] text-[var(--text-secondary)]"
          >
            {t.exportDialog.scope}
          </label>
          <select
            id="export-scope-select"
            value={scope}
            onChange={(e) => setScope(e.target.value as ExportScopeDto)}
            className="soyo-select w-full px-2.5 py-1.5 text-xs"
          >
            <option value="transcript" className="bg-[var(--surface-overlay)]">
              {t.exportDialog.scopeTranscript}
            </option>
            <option value="full" className="bg-[var(--surface-overlay)]">
              {t.exportDialog.scopeFull}
            </option>
          </select>
        </div>
      </div>

      <label className="inline-flex cursor-pointer items-center gap-2 text-[var(--text-secondary)]">
        <input
          type="checkbox"
          checked={includeTools}
          onChange={(e) => setIncludeTools(e.target.checked)}
          className="soyo-checkbox"
        />
        {t.exportDialog.includeTools}
      </label>

      {embedded && (
        <div className="flex justify-end pt-2">
          <Button
            variant="primary"
            size="md"
            disabled={busy || !sessionId || !targetPath.trim()}
            onClick={() => void handleExport()}
          >
            {busy && <SquaresLoader size={12} />}
            {busy ? t.exportDialog.exporting : t.exportDialog.publishBtn}
          </Button>
        </div>
      )}
    </div>
  );

  if (embedded) {
    return <div data-testid="export-embedded-panel">{body}</div>;
  }

  return (
    <DialogShell
      open={open}
      onClose={onClose}
      ariaLabel={t.exportDialog.title}
      testId="export-dialog"
      size="sm"
      panelClassName="max-w-lg"
    >
      <div className="flex items-center justify-between border-b border-[var(--border-subtle)] px-4 py-3">
        <div className="flex items-center gap-2">
          <Download className="h-4 w-4 text-[var(--accent-text)]" strokeWidth={1.75} />
          <h2 className="text-[14px] font-semibold text-[var(--text-primary)]">
            {t.exportDialog.title}
          </h2>
        </div>
        <button
          type="button"
          onClick={onClose}
          aria-label={t.a11y.closeExport}
          className="rounded-[var(--radius-sm)] p-1 text-[var(--text-tertiary)] transition-colors hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]"
        >
          <X className="h-4 w-4" strokeWidth={1.75} />
        </button>
      </div>

      <div className="p-4">{body}</div>

      <div className="flex justify-end gap-2 border-t border-[var(--border-subtle)] px-4 py-3 text-xs">
        <Button variant="secondary" size="sm" onClick={onClose}>
          {t.exportDialog.close}
        </Button>
        <Button
          variant="primary"
          size="md"
          disabled={busy || !sessionId || !targetPath.trim()}
          onClick={() => void handleExport()}
        >
          {busy && <SquaresLoader size={12} />}
          {busy ? t.exportDialog.exporting : t.exportDialog.publishBtn}
        </Button>
      </div>
    </DialogShell>
  );
}
