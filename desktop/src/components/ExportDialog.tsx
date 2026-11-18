import { Download, X } from "lucide-react";
import { useState } from "react";
import {
  commands,
  type ExportFormatDto,
  type ExportResultDto,
  type ExportScopeDto,
} from "../bindings";

export interface ExportDialogProps {
  open: boolean;
  sessionId: string | null;
  workspace: string;
  onClose: () => void;
}

export function ExportDialog({
  open,
  sessionId,
  workspace,
  onClose,
}: ExportDialogProps) {
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

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label="Export Session"
      data-testid="export-dialog"
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-4 backdrop-blur-xs"
    >
      <div className="w-full max-w-lg rounded-xl border border-[#303038] bg-[#141416] shadow-2xl">
        <div className="flex items-center justify-between border-b border-[#303038] px-4 py-3">
          <div className="flex items-center gap-2">
            <Download className="h-4 w-4 text-[#58A6FF]" />
            <h2 className="text-sm font-semibold text-[#E8E8ED]">
              Export Session Transcript
            </h2>
          </div>
          <button
            type="button"
            onClick={onClose}
            aria-label="Close export dialog"
            className="rounded p-1 text-[#81818B] hover:bg-[#202024] hover:text-[#E8E8ED]"
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        <div className="space-y-3 p-4 text-xs">
          {errorMsg && (
            <div className="rounded border border-[#F87171]/40 bg-[#F87171]/10 px-3 py-2 text-[#F87171]">
              {errorMsg}
            </div>
          )}

          {result && (
            <div className="space-y-1 rounded border border-[#7DCB9B]/40 bg-[#7DCB9B]/10 p-2.5 text-[#7DCB9B]">
              <div className="font-medium">Export published</div>
              <div className="truncate font-mono text-[11px]">
                {result.path} ({result.bytes} bytes · {result.format})
              </div>
            </div>
          )}

          <div className="space-y-1">
            <label
              htmlFor="export-target-path"
              className="block font-medium text-[#E8E8ED]"
            >
              Output File Path (relative to {workspace || "workspace"} or absolute)
            </label>
            <input
              id="export-target-path"
              type="text"
              value={targetPath}
              onChange={(e) => setTargetPath(e.target.value)}
              className="w-full rounded-md border border-[#303038] bg-[#202024] px-2.5 py-1.5 font-mono text-xs text-[#E8E8ED]"
            />
          </div>

          <div className="grid grid-cols-2 gap-3">
            <div className="space-y-1">
              <label
                htmlFor="export-format-select"
                className="block font-medium text-[#E8E8ED]"
              >
                Format
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
                className="w-full rounded-md border border-[#303038] bg-[#202024] px-2 py-1.5 text-xs text-[#E8E8ED]"
              >
                <option value="markdown">Markdown (.md)</option>
                <option value="json">JSON v1 (.json)</option>
              </select>
            </div>

            <div className="space-y-1">
              <label
                htmlFor="export-scope-select"
                className="block font-medium text-[#E8E8ED]"
              >
                Scope
              </label>
              <select
                id="export-scope-select"
                value={scope}
                onChange={(e) => setScope(e.target.value as ExportScopeDto)}
                className="w-full rounded-md border border-[#303038] bg-[#202024] px-2 py-1.5 text-xs text-[#E8E8ED]"
              >
                <option value="transcript">Transcript</option>
                <option value="full">Full (with events/metrics)</option>
              </select>
            </div>
          </div>

          <label className="inline-flex items-center gap-2 text-[#D4D4D8]">
            <input
              type="checkbox"
              checked={includeTools}
              onChange={(e) => setIncludeTools(e.target.checked)}
            />
            Include tool arguments and result bodies
          </label>
        </div>

        <div className="flex justify-end gap-2 border-t border-[#303038] px-4 py-3 text-xs">
          <button
            type="button"
            onClick={onClose}
            className="rounded-md border border-[#303038] bg-[#202024] px-3 py-1.5 text-[#D4D4D8]"
          >
            Close
          </button>
          <button
            type="button"
            disabled={busy || !sessionId || !targetPath.trim()}
            onClick={() => void handleExport()}
            className="rounded-md bg-[#58A6FF] px-3.5 py-1.5 font-semibold text-[#09090B] disabled:opacity-40"
          >
            {busy ? "Exporting..." : "Publish Export"}
          </button>
        </div>
      </div>
    </div>
  );
}
