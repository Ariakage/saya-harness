import { Check, Database, Search, X } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import {
  commands,
  type MemoryQueryResultDto,
  type PreparedMemoryReviewDto,
} from "../bindings";
import { getMessages } from "../lib/i18n";
import { useAppStore } from "../store/useAppStore";
import { SquaresLoader } from "./SquaresLoader";
import { Button } from "./ui/Button";
import { DialogShell } from "./ui/DialogShell";

export interface MemoryDialogProps {
  open: boolean;
  embedded?: boolean;
  runActive: boolean;
  onClose: () => void;
}

export function MemoryDialog({
  open,
  embedded = false,
  runActive,
  onClose,
}: MemoryDialogProps) {
  const locale = useAppStore((s) => s.locale);
  const t = getMessages(locale);
  const [queryCmd, setQueryCmd] = useState("list");
  const [queryResult, setQueryResult] = useState<MemoryQueryResultDto | null>(
    null
  );
  const [actionCmd, setActionCmd] = useState("");
  const [prepared, setPrepared] = useState<PreparedMemoryReviewDto | null>(null);
  const [confirmMsg, setConfirmMsg] = useState<string | null>(null);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const runQuery = useCallback(async (cmd: string) => {
    setErrorMsg(null);
    const res = await commands.queryMemory(cmd);
    if (res.status === "ok") {
      setQueryResult(res.data);
    } else {
      setErrorMsg(`${res.error.code}: ${res.error.message}`);
    }
  }, []);

  useEffect(() => {
    if (!open) return;
    setPrepared(null);
    setConfirmMsg(null);
    void runQuery("list");
  }, [open, runQuery]);

  if (!open) {
    return null;
  }

  const handlePrepare = async () => {
    const trimmed = actionCmd.trim();
    if (!trimmed) return;
    setErrorMsg(null);
    setConfirmMsg(null);
    setBusy(true);
    try {
      const res = await commands.prepareMemoryAction(trimmed);
      if (res.status === "ok") {
        setPrepared(res.data);
      } else {
        setErrorMsg(`${res.error.code}: ${res.error.message}`);
      }
    } finally {
      setBusy(false);
    }
  };

  const handleConfirm = async () => {
    if (!prepared) return;
    setErrorMsg(null);
    setBusy(true);
    try {
      const res = await commands.confirmMemoryAction(prepared.action_id);
      if (res.status === "ok") {
        setConfirmMsg(t.memoryDialog.appliedMsg(prepared.operation_id));
        setPrepared(null);
        setActionCmd("");
        await runQuery(queryCmd);
      } else {
        setErrorMsg(`${res.error.code}: ${res.error.message}`);
      }
    } finally {
      setBusy(false);
    }
  };

  const body = (
    <div className="space-y-4 text-xs">
      {errorMsg && (
        <div className="rounded-[var(--radius-md)] border border-[var(--danger)] bg-[var(--danger-surface)] px-3 py-2 text-[var(--danger)]">
          {errorMsg}
        </div>
      )}
      {confirmMsg && (
        <div className="rounded-[var(--radius-md)] border border-[var(--success)] bg-[var(--success-surface)] px-3 py-2 text-[var(--success)]">
          {confirmMsg}
        </div>
      )}

      <div className="space-y-2">
        <div className="flex gap-2">
          <input
            type="text"
            aria-label={t.a11y.memoryQueryInput}
            value={queryCmd}
            onChange={(e) => setQueryCmd(e.target.value)}
            placeholder={t.memoryDialog.queryPlaceholder}
            className="soyo-input flex-1 px-2.5 py-1.5 font-mono text-xs"
          />
          <Button variant="secondary" size="sm" onClick={() => void runQuery(queryCmd)}>
            <Search className="h-3.5 w-3.5 text-[var(--text-tertiary)]" strokeWidth={1.75} />
            {t.memoryDialog.queryBtn}
          </Button>
        </div>
        {queryResult && (
          <pre className="max-h-56 overflow-auto rounded-[var(--radius-lg)] border border-[var(--border-subtle)] bg-[var(--surface-main)] p-3 font-mono text-[11px] text-[var(--text-secondary)]">
            {JSON.stringify(queryResult.data, null, 2)}
          </pre>
        )}
      </div>

      <div className="space-y-2 rounded-[var(--radius-lg)] border border-[var(--border-subtle)] bg-[var(--surface-main)] p-3">
        <div className="font-medium text-[var(--text-primary)]">
          {t.memoryDialog.prepareTitle}
        </div>
        <div className="flex gap-2">
          <input
            type="text"
            aria-label={t.a11y.memoryActionInput}
            value={actionCmd}
            disabled={runActive || busy}
            onChange={(e) => setActionCmd(e.target.value)}
            placeholder={t.memoryDialog.preparePlaceholder}
            className="soyo-input flex-1 px-2.5 py-1.5 font-mono text-xs"
          />
          <Button
            variant="primary"
            size="md"
            disabled={runActive || busy || !actionCmd.trim()}
            onClick={() => void handlePrepare()}
          >
            {busy && <SquaresLoader size={12} />}
            {t.memoryDialog.prepareBtn}
          </Button>
        </div>

        {prepared && (
          <div className="soyo-panel-enter mt-2 space-y-2 rounded-[var(--radius-md)] border border-[var(--warning)] bg-[var(--warning-surface)] p-3">
            <div className="flex items-center justify-between text-[11px]">
              <span className="font-mono text-[var(--warning)]">
                {t.memoryDialog.operationSummary(
                  prepared.operation_id,
                  prepared.expected_revision
                )}
              </span>
            </div>
            <pre className="max-h-40 overflow-auto rounded-[var(--radius-md)] border border-[var(--border)] bg-[var(--surface-code)] p-2 font-mono text-[11px] text-[var(--text-primary)]">
              {JSON.stringify(prepared.review, null, 2)}
            </pre>
            <div className="flex justify-end gap-2">
              <Button variant="secondary" size="sm" onClick={() => setPrepared(null)}>
                {t.memoryDialog.discardBtn}
              </Button>
              <Button
                variant="secondary"
                size="sm"
                disabled={busy}
                onClick={() => void handleConfirm()}
                className="font-semibold text-[var(--success)]"
              >
                {busy ? (
                  <SquaresLoader size={12} />
                ) : (
                  <Check className="h-3.5 w-3.5" strokeWidth={1.75} />
                )}
                {t.memoryDialog.confirmBtn}
              </Button>
            </div>
          </div>
        )}
      </div>
    </div>
  );

  if (embedded) {
    return <div data-testid="memory-embedded-panel">{body}</div>;
  }

  return (
    <DialogShell
      open={open}
      onClose={onClose}
      ariaLabel={t.memoryDialog.title}
      testId="memory-dialog"
      size="md"
      panelClassName="flex max-h-[85vh] max-w-[700px] flex-col"
    >
      <div className="flex items-center justify-between border-b border-[var(--border-subtle)] px-4 py-3">
        <div className="flex items-center gap-2">
          <Database className="h-4 w-4 text-[var(--accent-text)]" strokeWidth={1.75} />
          <h2 className="text-[14px] font-semibold text-[var(--text-primary)]">
            {t.memoryDialog.title}
          </h2>
        </div>
        <button
          type="button"
          onClick={onClose}
          aria-label={t.a11y.closeMemoryDialog}
          className="rounded-[var(--radius-sm)] p-1 text-[var(--text-tertiary)] transition-colors hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]"
        >
          <X className="h-4 w-4" strokeWidth={1.75} />
        </button>
      </div>

      <div className="flex-1 overflow-y-auto p-4">{body}</div>
    </DialogShell>
  );
}
