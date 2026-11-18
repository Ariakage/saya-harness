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
      className="soyo-backdrop-enter fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4 backdrop-blur-xs"
    >
      <div className="soyo-dialog-enter flex max-h-[85vh] w-full max-w-[660px] flex-col overflow-hidden rounded-[14px] border border-[#2b2b2b] bg-[#181818] shadow-[0_24px_60px_rgba(0,0,0,0.65)]">
        <div className="flex items-center justify-between border-b border-[#232323] px-4 py-3">
          <div className="flex items-center gap-2">
            <KeyRound className="h-4 w-4 text-[#d9a08e]" />
            <h2 className="text-[14px] font-semibold text-[#e8e8e8]">
              MCP Servers & Credentials (Redacted Metadata Only)
            </h2>
          </div>
          <button
            type="button"
            onClick={onClose}
            aria-label="Close MCP and credentials dialog"
            className="rounded-[7px] p-1 text-[#8a8a8a] transition-colors hover:bg-[#222222] hover:text-[#e8e8e8]"
          >
            <X className="h-4 w-4" />
          </button>
        </div>

        <div className="flex-1 space-y-5 overflow-y-auto p-4 text-xs">
          {errorMsg && (
            <div className="rounded-[8px] border border-[#f87171]/35 bg-[#1c1515] px-3 py-2 text-[#f87171]">
              {errorMsg}
            </div>
          )}

          <section className="space-y-2">
            <div className="flex items-center gap-1.5 font-medium text-[#e8e8e8]">
              <Plug className="h-3.5 w-3.5 text-[#7aa2f7]" />
              <span>Configured MCP Servers</span>
            </div>
            {servers.length === 0 ? (
              <div className="rounded-[9px] border border-[#262626] bg-[#1a1a1a] p-3 text-[#6e6e6e]">
                No MCP servers configured.
              </div>
            ) : (
              <ul className="space-y-2">
                {servers.map((srv) => (
                  <li
                    key={srv.name}
                    className="flex items-center justify-between gap-3 rounded-[9px] border border-[#262626] bg-[#1a1a1a] p-2.5"
                  >
                    <div className="min-w-0 space-y-0.5">
                      <div className="flex items-center gap-2">
                        <span className="font-mono font-semibold text-[#e8e8e8]">
                          {sanitizeDisplayText(srv.name)}
                        </span>
                        <span className="devin-pill py-0 text-[10.5px] text-[#7aa2f7]">
                          {srv.transport}
                        </span>
                      </div>
                      <div className="truncate font-mono text-[11px] text-[#7a7a7a]">
                        {sanitizeDisplayText(srv.target)}
                      </div>
                    </div>
                    <button
                      type="button"
                      disabled={runActive}
                      onClick={() => void handleToggleServer(srv)}
                      className={`devin-pill-btn shrink-0 ${
                        srv.enabled ? "text-[#4ade80]" : "text-[#8a8a8a]"
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
            <div className="flex items-center gap-1.5 font-medium text-[#e8e8e8]">
              <ShieldCheck className="h-3.5 w-3.5 text-[#4ade80]" />
              <span>Stored Credential References</span>
            </div>
            {credentials.length === 0 ? (
              <div className="rounded-[9px] border border-[#262626] bg-[#1a1a1a] p-3 text-[#6e6e6e]">
                No stored host credential references.
              </div>
            ) : (
              <ul className="space-y-2">
                {credentials.map((cred) => (
                  <li
                    key={cred.reference}
                    className="flex items-center justify-between gap-3 rounded-[9px] border border-[#262626] bg-[#1a1a1a] p-2.5"
                  >
                    <div className="min-w-0 space-y-0.5">
                      <div className="flex items-center gap-2">
                        <span className="font-mono text-[#e8e8e8]">
                          {cred.server} ({cred.kind})
                        </span>
                        <span className="text-[10.5px] text-[#6e6e6e]">
                          rev {cred.revision}
                        </span>
                        {cred.revoked && (
                          <span className="devin-pill py-0 text-[10px] text-[#f87171]">
                            revoked
                          </span>
                        )}
                      </div>
                      <div className="truncate font-mono text-[10.5px] text-[#7a7a7a]">
                        {cred.reference} · {sanitizeDisplayText(cred.audience)}
                      </div>
                    </div>
                    {!cred.revoked && (
                      <button
                        type="button"
                        onClick={() => void handleRevoke(cred.reference)}
                        className="devin-pill-btn shrink-0 text-[#f87171]"
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

          <section className="space-y-2 rounded-[10px] border border-[#262626] bg-[#141414] p-3">
            <div className="font-medium text-[#e8e8e8]">
              Import Bearer Secret from Environment Variable
            </div>
            <p className="text-[11px] text-[#7a7a7a]">
              Plaintext tokens stay in host memory and private storage; enter
              only the environment variable name.
            </p>
            <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
              <input
                type="text"
                aria-label="MCP server name"
                value={importServer}
                onChange={(e) => setImportServer(e.target.value)}
                placeholder="HTTP MCP server name"
                className="devin-input px-2.5 py-1.5 text-xs"
              />
              <input
                type="text"
                aria-label="Secret environment variable"
                value={envVar}
                onChange={(e) => setEnvVar(e.target.value)}
                placeholder="Env var name (e.g. MCP_TOKEN)"
                className="devin-input px-2.5 py-1.5 font-mono text-xs"
              />
            </div>
            <input
              type="text"
              aria-label="Credential scopes"
              value={scopesInput}
              onChange={(e) => setScopesInput(e.target.value)}
              placeholder="Optional comma-separated scopes"
              className="devin-input w-full px-2.5 py-1.5 text-xs"
            />
            <div className="flex items-center justify-between pt-1">
              <label className="inline-flex cursor-pointer items-center gap-2 text-[11px] text-[#c9c9c9]">
                <input
                  type="checkbox"
                  checked={approveScopes}
                  onChange={(e) => setApproveScopes(e.target.checked)}
                  className="devin-checkbox"
                />
                Explicitly approve endpoint & scopes
              </label>
              <button
                type="button"
                disabled={
                  busy || !importServer.trim() || !envVar.trim() || !approveScopes
                }
                onClick={() => void handleImport()}
                className="rounded-[8px] bg-[#d9a08e] px-3 py-1.5 text-xs font-semibold text-[#1c120e] transition-colors hover:bg-[#e2a68f] disabled:opacity-40"
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
