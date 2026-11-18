import { KeyRound, Plug, ShieldCheck, Trash2, X } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import {
  commands,
  type CredentialRecordDto_Serialize,
  type McpServerDto_Serialize,
} from "../bindings";
import { sanitizeDisplayText } from "../lib/sanitize";

export interface McpCredentialsDialogProps {
  open: boolean;
  runActive: boolean;
  onClose: () => void;
}

export function McpCredentialsDialog({
  open,
  runActive,
  onClose,
}: McpCredentialsDialogProps) {
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

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label="MCP Servers and Credentials"
      data-testid="mcp-credentials-dialog"
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-4 backdrop-blur-xs"
    >
      <div className="flex max-h-[85vh] w-full max-w-2xl flex-col rounded-xl border border-[#303038] bg-[#141416] shadow-2xl">
        <div className="flex items-center justify-between border-b border-[#303038] px-4 py-3">
          <div className="flex items-center gap-2">
            <KeyRound className="h-4 w-4 text-[#7DCB9B]" />
            <h2 className="text-sm font-semibold text-[#E8E8ED]">
              MCP Servers & Credentials (Redacted Metadata Only)
            </h2>
          </div>
          <button
            type="button"
            onClick={onClose}
            aria-label="Close MCP and credentials dialog"
            className="rounded p-1 text-[#81818B] hover:bg-[#202024] hover:text-[#E8E8ED]"
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        <div className="flex-1 space-y-5 overflow-y-auto p-4 text-xs">
          {errorMsg && (
            <div className="rounded border border-[#F87171]/40 bg-[#F87171]/10 px-3 py-2 text-[#F87171]">
              {errorMsg}
            </div>
          )}

          <section className="space-y-2">
            <div className="flex items-center gap-1.5 font-medium text-[#E8E8ED]">
              <Plug className="h-3.5 w-3.5 text-[#58A6FF]" />
              <span>Configured MCP Servers</span>
            </div>
            {servers.length === 0 ? (
              <div className="rounded border border-[#303038] bg-[#09090B] p-3 text-[#81818B]">
                No MCP servers configured.
              </div>
            ) : (
              <ul className="space-y-2">
                {servers.map((srv) => (
                  <li
                    key={srv.name}
                    className="flex items-center justify-between gap-3 rounded-md border border-[#303038] bg-[#09090B] p-2.5"
                  >
                    <div className="min-w-0 space-y-0.5">
                      <div className="flex items-center gap-2">
                        <span className="font-mono font-semibold text-[#E8E8ED]">
                          {sanitizeDisplayText(srv.name)}
                        </span>
                        <span className="rounded bg-[#202024] px-1.5 py-0.5 text-[10px] text-[#81818B]">
                          {srv.transport}
                        </span>
                      </div>
                      <div className="truncate font-mono text-[11px] text-[#81818B]">
                        {sanitizeDisplayText(srv.target)}
                      </div>
                    </div>
                    <button
                      type="button"
                      disabled={runActive}
                      onClick={() => void handleToggleServer(srv)}
                      className={`shrink-0 rounded px-2.5 py-1 text-[11px] font-semibold ${
                        srv.enabled
                          ? "border border-[#7DCB9B]/40 bg-[#7DCB9B]/20 text-[#7DCB9B]"
                          : "border border-[#303038] bg-[#202024] text-[#81818B]"
                      } disabled:opacity-40`}
                    >
                      {srv.enabled ? "Enabled" : "Disabled"}
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </section>

          <section className="space-y-2">
            <div className="flex items-center gap-1.5 font-medium text-[#E8E8ED]">
              <ShieldCheck className="h-3.5 w-3.5 text-[#7DCB9B]" />
              <span>Stored Credential References</span>
            </div>
            {credentials.length === 0 ? (
              <div className="rounded border border-[#303038] bg-[#09090B] p-3 text-[#81818B]">
                No stored host credential references.
              </div>
            ) : (
              <ul className="space-y-2">
                {credentials.map((cred) => (
                  <li
                    key={cred.reference}
                    className="flex items-center justify-between gap-3 rounded-md border border-[#303038] bg-[#09090B] p-2.5"
                  >
                    <div className="min-w-0 space-y-0.5">
                      <div className="flex items-center gap-2">
                        <span className="font-mono text-[#E8E8ED]">
                          {cred.server} ({cred.kind})
                        </span>
                        <span className="text-[10px] text-[#81818B]">
                          rev {cred.revision}
                        </span>
                        {cred.revoked && (
                          <span className="rounded bg-[#F87171]/15 px-1.5 py-0.5 text-[10px] text-[#F87171]">
                            revoked
                          </span>
                        )}
                      </div>
                      <div className="truncate font-mono text-[10px] text-[#81818B]">
                        {cred.reference} · {sanitizeDisplayText(cred.audience)}
                      </div>
                    </div>
                    {!cred.revoked && (
                      <button
                        type="button"
                        onClick={() => void handleRevoke(cred.reference)}
                        className="inline-flex shrink-0 items-center gap-1 rounded border border-[#F87171]/40 bg-[#F87171]/10 px-2.5 py-1 text-[11px] text-[#F87171]"
                      >
                        <Trash2 className="h-3 w-3" />
                        Revoke
                      </button>
                    )}
                  </li>
                ))}
              </ul>
            )}
          </section>

          <section className="space-y-2 rounded-md border border-[#303038] bg-[#09090B] p-3">
            <div className="font-medium text-[#E8E8ED]">
              Import Bearer Secret from Environment Variable
            </div>
            <p className="text-[11px] text-[#81818B]">
              Plaintext tokens stay in host memory and private storage; enter only the environment variable name.
            </p>
            <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
              <input
                type="text"
                aria-label="MCP server name"
                value={importServer}
                onChange={(e) => setImportServer(e.target.value)}
                placeholder="HTTP MCP server name"
                className="rounded border border-[#303038] bg-[#202024] px-2.5 py-1.5 text-xs text-[#E8E8ED]"
              />
              <input
                type="text"
                aria-label="Secret environment variable"
                value={envVar}
                onChange={(e) => setEnvVar(e.target.value)}
                placeholder="Env var name (e.g. MCP_TOKEN)"
                className="rounded border border-[#303038] bg-[#202024] px-2.5 py-1.5 font-mono text-xs text-[#E8E8ED]"
              />
            </div>
            <input
              type="text"
              aria-label="Credential scopes"
              value={scopesInput}
              onChange={(e) => setScopesInput(e.target.value)}
              placeholder="Optional comma-separated scopes"
              className="w-full rounded border border-[#303038] bg-[#202024] px-2.5 py-1.5 text-xs text-[#E8E8ED]"
            />
            <div className="flex items-center justify-between pt-1">
              <label className="inline-flex items-center gap-2 text-[11px] text-[#D4D4D8]">
                <input
                  type="checkbox"
                  checked={approveScopes}
                  onChange={(e) => setApproveScopes(e.target.checked)}
                />
                Explicitly approve endpoint & scopes
              </label>
              <button
                type="button"
                disabled={busy || !importServer.trim() || !envVar.trim() || !approveScopes}
                onClick={() => void handleImport()}
                className="rounded-md bg-[#7DCB9B] px-3 py-1.5 text-xs font-semibold text-[#09090B] disabled:opacity-40"
              >
                Import Reference
              </button>
            </div>
          </section>
        </div>
      </div>
    </div>
  );
}
