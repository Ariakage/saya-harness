import { KeyRound, Plug, ShieldCheck, Trash2, X } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import {
  commands,
  type CredentialRecordDto_Serialize,
  type McpServerDto_Serialize,
} from "../bindings";
import { getMessages } from "../lib/i18n";
import { sanitizeDisplayText } from "../lib/sanitize";
import { useAppStore } from "../store/useAppStore";
import { SquaresLoader } from "./SquaresLoader";
import { Button } from "./ui/Button";
import { DialogShell } from "./ui/DialogShell";
import { Pill } from "./ui/Pill";

export interface McpCredentialsDialogProps {
  open: boolean;
  embedded?: boolean;
  runActive: boolean;
  onClose: () => void;
}

export function McpCredentialsDialog({
  open,
  embedded = false,
  runActive,
  onClose,
}: McpCredentialsDialogProps) {
  const locale = useAppStore((s) => s.locale);
  const t = getMessages(locale);
  const [servers, setServers] = useState<McpServerDto_Serialize[]>([]);
  const [credentials, setCredentials] = useState<CredentialRecordDto_Serialize[]>([]);
  const [importServer, setImportServer] = useState("");
  const [envVar, setEnvVar] = useState("");
  const [scopesInput, setScopesInput] = useState("");
  const [approveScopes, setApproveScopes] = useState(false);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const loadData = useCallback(async () => {
    const [srvRes, credRes] = await Promise.all([
      commands.listMcpServers(),
      commands.listCredentials(),
    ]);
    if (srvRes.status === "ok") {
      setServers(srvRes.data);
      const firstHttp = srvRes.data.find((s) => s.transport === "http");
      if (firstHttp && !importServer) {
        setImportServer(firstHttp.name);
      }
    }
    if (credRes.status === "ok") {
      setCredentials(credRes.data);
    }
  }, [importServer]);

  useEffect(() => {
    if (!open) return;
    void loadData();
  }, [open, loadData]);

  if (!open) {
    return null;
  }

  const handleToggleServer = async (srv: McpServerDto_Serialize) => {
    setErrorMsg(null);
    const res = await commands.toggleMcpServer(srv.name, !srv.enabled);
    if (res.status === "ok") {
      setServers(res.data);
    } else {
      setErrorMsg(`${res.error.code}: ${res.error.message}`);
    }
  };

  const handleImport = async () => {
    if (!importServer.trim() || !envVar.trim()) return;
    setErrorMsg(null);
    setBusy(true);
    try {
      const scopes = scopesInput
        .split(",")
        .map((s) => s.trim())
        .filter(Boolean);
      const res = await commands.importCredential(
        importServer.trim(),
        envVar.trim(),
        scopes,
        approveScopes
      );
      if (res.status === "ok") {
        setEnvVar("");
        setScopesInput("");
        setApproveScopes(false);
        await loadData();
      } else {
        setErrorMsg(`${res.error.code}: ${res.error.message}`);
      }
    } finally {
      setBusy(false);
    }
  };

  const handleRevoke = async (reference: string) => {
    setErrorMsg(null);
    const res = await commands.revokeCredential(reference);
    if (res.status === "ok") {
      await loadData();
    } else {
      setErrorMsg(`${res.error.code}: ${res.error.message}`);
    }
  };

  const body = (
    <div className="space-y-5 text-xs">
      {errorMsg && (
        <div className="rounded-[var(--radius-md)] border border-[var(--danger)] bg-[var(--danger-surface)] px-3 py-2 text-[var(--danger)]">
          {errorMsg}
        </div>
      )}

      <section className="space-y-2">
        <div className="flex items-center gap-1.5 font-medium text-[var(--text-primary)]">
          <Plug className="h-3.5 w-3.5 text-[var(--info)]" strokeWidth={1.75} />
          <span>{t.mcpDialog.configuredServers}</span>
        </div>
        {servers.length === 0 ? (
          <div className="rounded-[var(--radius-md)] border border-[var(--border-subtle)] bg-[var(--surface-raised)] p-3 text-[var(--text-tertiary)]">
            {t.mcpDialog.noServers}
          </div>
        ) : (
          <ul className="space-y-2">
            {servers.map((srv) => (
              <li
                key={srv.name}
                className="flex items-center justify-between gap-3 rounded-[var(--radius-md)] border border-[var(--border-subtle)] bg-[var(--surface-raised)] p-2.5"
              >
                <div className="min-w-0 space-y-0.5">
                  <div className="flex items-center gap-2">
                    <span className="font-mono font-semibold text-[var(--text-primary)]">
                      {sanitizeDisplayText(srv.name)}
                    </span>
                    <Pill tone="info">{srv.transport}</Pill>
                  </div>
                  <div className="truncate font-mono text-[11px] text-[var(--text-tertiary)]">
                    {sanitizeDisplayText(srv.target)}
                  </div>
                </div>
                <Button
                  variant="secondary"
                  size="sm"
                  disabled={runActive}
                  onClick={() => void handleToggleServer(srv)}
                  className={
                    srv.enabled ? "text-[var(--success)]" : "text-[var(--text-tertiary)]"
                  }
                >
                  {srv.enabled ? t.mcpDialog.enabled : t.mcpDialog.disabled}
                </Button>
              </li>
            ))}
          </ul>
        )}
      </section>

      <section className="space-y-2">
        <div className="flex items-center gap-1.5 font-medium text-[var(--text-primary)]">
          <ShieldCheck className="h-3.5 w-3.5 text-[var(--success)]" strokeWidth={1.75} />
          <span>{t.mcpDialog.storedCredentials}</span>
        </div>
        {credentials.length === 0 ? (
          <div className="rounded-[var(--radius-md)] border border-[var(--border-subtle)] bg-[var(--surface-raised)] p-3 text-[var(--text-tertiary)]">
            {t.mcpDialog.noCredentials}
          </div>
        ) : (
          <ul className="space-y-2">
            {credentials.map((cred) => (
              <li
                key={cred.reference}
                className="flex items-center justify-between gap-3 rounded-[var(--radius-md)] border border-[var(--border-subtle)] bg-[var(--surface-raised)] p-2.5"
              >
                <div className="min-w-0 space-y-0.5">
                  <div className="flex items-center gap-2">
                    <span className="font-mono text-[var(--text-primary)]">
                      {cred.server} ({cred.kind})
                    </span>
                    <span className="text-[11px] text-[var(--text-tertiary)]">
                      {t.mcpDialog.revisionLabel(String(cred.revision))}
                    </span>
                    {cred.revoked && (
                      <Pill tone="danger">{t.mcpDialog.revoked}</Pill>
                    )}
                  </div>
                  <div className="truncate font-mono text-[11px] text-[var(--text-tertiary)]">
                    {cred.reference} · {sanitizeDisplayText(cred.audience)}
                  </div>
                </div>
                {!cred.revoked && (
                  <Button
                    variant="danger"
                    size="sm"
                    onClick={() => void handleRevoke(cred.reference)}
                  >
                    <Trash2 className="h-3 w-3" strokeWidth={1.75} />
                    {t.mcpDialog.revoke}
                  </Button>
                )}
              </li>
            ))}
          </ul>
        )}
      </section>

      <section className="space-y-2 rounded-[var(--radius-lg)] border border-[var(--border-subtle)] bg-[var(--surface-app)] p-3">
        <div className="font-medium text-[var(--text-primary)]">
          {t.mcpDialog.importTitle}
        </div>
        <p className="text-[11px] text-[var(--text-tertiary)]">
          {t.mcpDialog.importDesc}
        </p>
        <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
          <input
            type="text"
            aria-label={t.a11y.mcpServerInput}
            value={importServer}
            onChange={(e) => setImportServer(e.target.value)}
            placeholder={t.mcpDialog.serverPlaceholder}
            className="soyo-input px-2.5 py-1.5 text-xs"
          />
          <input
            type="text"
            aria-label={t.a11y.mcpEnvInput}
            value={envVar}
            onChange={(e) => setEnvVar(e.target.value)}
            placeholder={t.mcpDialog.envPlaceholder}
            className="soyo-input px-2.5 py-1.5 font-mono text-xs"
          />
        </div>
        <input
          type="text"
          aria-label={t.a11y.mcpScopesInput}
          value={scopesInput}
          onChange={(e) => setScopesInput(e.target.value)}
          placeholder={t.mcpDialog.scopesPlaceholder}
          className="soyo-input w-full px-2.5 py-1.5 text-xs"
        />
        <div className="flex items-center justify-between pt-1">
          <label className="inline-flex cursor-pointer items-center gap-2 text-[11px] text-[var(--text-secondary)]">
            <input
              type="checkbox"
              checked={approveScopes}
              onChange={(e) => setApproveScopes(e.target.checked)}
              className="soyo-checkbox"
            />
            {t.mcpDialog.approveScopesLabel}
          </label>
          <Button
            variant="primary"
            size="md"
            disabled={
              busy || !importServer.trim() || !envVar.trim() || !approveScopes
            }
            onClick={() => void handleImport()}
          >
            {busy && <SquaresLoader size={12} />}
            {t.mcpDialog.importBtn}
          </Button>
        </div>
      </section>
    </div>
  );

  if (embedded) {
    return <div data-testid="mcp-embedded-panel">{body}</div>;
  }

  return (
    <DialogShell
      open={open}
      onClose={onClose}
      ariaLabel={t.mcpDialog.title}
      testId="mcp-credentials-dialog"
      size="md"
      panelClassName="flex max-h-[85vh] max-w-[660px] flex-col"
    >
      <div className="flex items-center justify-between border-b border-[var(--border-subtle)] px-4 py-3">
        <div className="flex items-center gap-2">
          <KeyRound className="h-4 w-4 text-[var(--accent-text)]" strokeWidth={1.75} />
          <h2 className="text-[14px] font-semibold text-[var(--text-primary)]">
            {t.mcpDialog.title}
          </h2>
        </div>
        <button
          type="button"
          onClick={onClose}
          aria-label={t.a11y.closeMcpDialog}
          className="rounded-[var(--radius-sm)] p-1 text-[var(--text-tertiary)] transition-colors hover:bg-[var(--surface-hover)] hover:text-[var(--text-primary)]"
        >
          <X className="h-4 w-4" strokeWidth={1.75} />
        </button>
      </div>

      <div className="flex-1 overflow-y-auto p-4">{body}</div>
    </DialogShell>
  );
}
