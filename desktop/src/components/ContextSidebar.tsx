import { CheckCircle2, Circle, FileText, Plug } from "lucide-react";
import type {
  BootstrapStateDto_Serialize,
  ContextEntryDto,
  McpStatusBadgeDto,
  SessionItemDto_Serialize,
  TodoItemDto,
  WorkModeDto,
} from "../bindings";
import { getMessages } from "../lib/i18n";
import { WORK_MODE_STYLES, formatTokenCount } from "../lib/sanitize";
import { type DesktopMessage, useAppStore } from "../store/useAppStore";

export interface ContextSidebarProps {
  bootstrap: BootstrapStateDto_Serialize | null;
  workMode: WorkModeDto;
  contextEntries: ContextEntryDto[];
  contextSkillsCount: number | null;
  contextEstimated: boolean;
  /** Total tokens filling the window — the same figure the composer ring
   * shows. When omitted it is derived from `contextEntries`. */
  contextUsedTokens?: number | null;
  todos: TodoItemDto[];
  mcpServers: McpStatusBadgeDto[];
  recentSessions: SessionItemDto_Serialize[];
  activeSessionId: string | null;
  messages?: DesktopMessage[];
}

function extractOutputPaths(messages: DesktopMessage[]): string[] {
  const seen = new Set<string>();
  const results: string[] = [];

  const pushCandidate = (raw: string | null | undefined) => {
    if (!raw) return;
    const candidate = raw.split("·")[0]?.trim().replace(/^[ab]\//, "") ?? "";
    if (!candidate || candidate.startsWith("$") || candidate.includes(" ")) {
      return;
    }
    if (
      (candidate.includes("/") || /\.[a-z0-9]{1,8}$/i.test(candidate)) &&
      !seen.has(candidate)
    ) {
      seen.add(candidate);
      results.push(candidate);
    }
  };

  for (const msg of messages) {
    if (msg.role !== "tool" || msg.toolOk === false) continue;
    pushCandidate(msg.toolSummary);
    if (msg.text) {
      for (const line of msg.text.split(/\r?\n/)) {
        if (line.startsWith("+++ ")) {
          const target = line.slice(4).trim();
          if (target !== "/dev/null") {
            pushCandidate(target);
          }
        }
      }
    }
  }

  return results.slice(0, 6);
}

// Color rules (desktop-ui.md §8 / Soyo theme tokens):
const CONTEXT_BAR_COLORS = [
  "var(--accent)",
  "var(--mode-build)",
  "var(--mode-code)",
  "var(--warning)",
  "var(--info)",
  "var(--border-strong)",
];

function basenameFromWorkspace(path: string | undefined): string {
  if (!path) return "";
  const cleaned = path.replace(/\/+$/, "");
  const parts = cleaned.split("/");
  return parts.length > 2 ? `~/${parts.slice(-1)[0]}` : cleaned;
}

export function ContextSidebar({
  bootstrap,
  workMode,
  contextEntries,
  contextSkillsCount,
  contextEstimated,
  contextUsedTokens = null,
  todos,
  mcpServers,
  recentSessions,
  activeSessionId,
  messages = [],
}: ContextSidebarProps) {
  const locale = useAppStore((s) => s.locale);
  const t = getMessages(locale);
  const modeStyle = WORK_MODE_STYLES[workMode];
  const outputPaths = extractOutputPaths(messages);
  const windowTokens = bootstrap?.context_window_tokens ?? null;
  const totalContextTokens =
    contextUsedTokens ??
    contextEntries.reduce((acc, item) => acc + item.tokens, 0);
  const freeTokens =
    windowTokens && windowTokens > totalContextTokens
      ? windowTokens - totalContextTokens
      : 0;
  const usagePercent =
    windowTokens && windowTokens > 0
      ? Math.min(100, Math.round((totalContextTokens / windowTokens) * 100))
      : 0;

  return (
    <aside
      className="flex w-[308px] shrink-0 flex-col gap-3.5 overflow-y-auto border-l border-[var(--border-subtle)] bg-[var(--surface-code)] p-3.5 text-xs"
      data-testid="context-sidebar"
    >
      {/* Runtime */}
      <section className="space-y-1.5">
        <div className="flex items-center justify-between text-[12px] font-medium text-[var(--text-tertiary)]">
          <span>{t.inspector.runtime}</span>
          <span
            className="inline-block h-2 w-2 rounded-full"
            style={{ backgroundColor: modeStyle.color }}
            title={`${t.inspector.runtime}: ${modeStyle.label}`}
            aria-hidden="true"
          />
        </div>
        <div className="space-y-1 text-[12px]">
          <div className="flex items-center justify-between gap-2 py-0.5">
            <span className="text-[var(--text-tertiary)]">{t.inspector.profile}</span>
            <span className="truncate font-mono text-[12px] text-[var(--text-secondary)]">
              {bootstrap?.selected_profile ?? t.inspector.unset}
            </span>
          </div>
          <div className="flex items-center justify-between gap-2 py-0.5">
            <span className="text-[var(--text-tertiary)]">{t.inspector.model}</span>
            <span className="truncate font-mono text-[12px] text-[var(--text-secondary)]">
              {bootstrap?.selected_model ?? t.inspector.unset}
            </span>
          </div>
          <div className="flex items-center justify-between gap-2 py-0.5">
            <span className="text-[var(--text-tertiary)]">{t.inspector.sandbox}</span>
            <span className="truncate font-mono text-[12px] text-[var(--text-secondary)]">
              {bootstrap?.sandbox_backend ?? "default"}
            </span>
          </div>
          <div className="flex items-center justify-between gap-2 py-0.5">
            <span className="text-[var(--text-tertiary)]">{t.inspector.workspace}</span>
            <span
              className="truncate font-mono text-[11px] text-[var(--text-secondary)]"
              title={bootstrap?.workspace}
            >
              {basenameFromWorkspace(bootstrap?.workspace) || t.inspector.unset}
            </span>
          </div>
        </div>
      </section>

      <div className="h-px bg-[var(--surface-raised)]" />

      {/* Context window */}
      <section className="space-y-2">
        <div className="flex items-center justify-between text-[12px] font-medium text-[var(--text-tertiary)]">
          <span>{t.inspector.contextWindow}</span>
          <span className="font-mono text-[12px] text-[var(--text-tertiary)]">
            {contextEstimated ? "~" : ""}
            {formatTokenCount(totalContextTokens)}
            {windowTokens
              ? ` / ${formatTokenCount(windowTokens)} (${usagePercent}%)`
              : ""}
          </span>
        </div>

        {contextEntries.length === 0 ? (
          <div className="text-[12px] text-[var(--text-tertiary)]">
            {t.inspector.contextEmpty}
          </div>
        ) : (
          <div className="space-y-2.5">
            <div className="flex h-[6px] w-full overflow-hidden rounded-[var(--radius-sm)] bg-[var(--surface-hover)]">
              {contextEntries.map((entry, idx) => {
                const denom =
                  windowTokens && windowTokens > 0
                    ? windowTokens
                    : Math.max(1, totalContextTokens);
                const widthPct = Math.max(
                  2,
                  Math.round((entry.tokens / denom) * 100)
                );
                return (
                  <div
                    key={entry.label}
                    className="transition-[width] duration-300 ease-out"
                    style={{
                      width: `${widthPct}%`,
                      backgroundColor:
                        CONTEXT_BAR_COLORS[idx % CONTEXT_BAR_COLORS.length],
                    }}
                    title={`${entry.label}: ${entry.tokens} tokens`}
                  />
                );
              })}
              {freeTokens > 0 && windowTokens && (
                <div
                  className="transition-[width] duration-300 ease-out"
                  style={{
                    width: `${Math.max(0, 100 - usagePercent)}%`,
                    backgroundColor: "var(--border-strong)",
                  }}
                  title={`${t.inspector.freeSpace}: ${freeTokens}`}
                />
              )}
            </div>

            <ul className="space-y-1 text-[12px]">
              {contextEntries.map((entry, idx) => {
                const denom =
                  windowTokens && windowTokens > 0
                    ? windowTokens
                    : Math.max(1, totalContextTokens);
                const pct = Math.round((entry.tokens / denom) * 100);
                return (
                  <li
                    key={entry.label}
                    className="flex items-center justify-between text-[var(--text-secondary)]"
                  >
                    <span className="inline-flex items-center gap-1.5">
                      <span
                        className="h-2 w-2 rounded-[var(--radius-sm)]"
                        style={{
                          backgroundColor:
                            CONTEXT_BAR_COLORS[idx % CONTEXT_BAR_COLORS.length],
                        }}
                      />
                      <span>{entry.label}</span>
                    </span>
                    <span className="font-mono text-[var(--text-tertiary)]">
                      {formatTokenCount(entry.tokens)}
                      {windowTokens ? ` · ${pct}%` : ""}
                    </span>
                  </li>
                );
              })}
              {freeTokens > 0 && windowTokens && (
                <li className="flex items-center justify-between text-[var(--text-secondary)]">
                  <span className="inline-flex items-center gap-1.5">
                    <span className="h-2 w-2 rounded-[var(--radius-sm)] bg-[var(--surface-hover)]" />
                    <span>{t.inspector.freeSpace}</span>
                  </span>
                  <span className="font-mono text-[var(--text-tertiary)]">
                    {formatTokenCount(freeTokens)} · {Math.max(0, 100 - usagePercent)}%
                  </span>
                </li>
              )}
            </ul>

            {contextSkillsCount != null && (
              <div className="text-[11px] text-[var(--text-tertiary)]">
                {t.inspector.activeSkills(contextSkillsCount)}
              </div>
            )}
          </div>
        )}
      </section>

      <div className="h-px bg-[var(--surface-raised)]" />

      {/* Tasks */}
      <section className="space-y-1.5">
        <div className="flex items-center justify-between text-[12px] font-medium text-[var(--text-tertiary)]">
          <span>{t.inspector.tasks}</span>
          <span className="font-mono text-[11px] text-[var(--text-tertiary)]">
            {todos.filter((t) => t.status === "completed").length}/{todos.length}
          </span>
        </div>
        {todos.length === 0 ? (
          <div className="text-[12px] text-[var(--text-tertiary)]">{t.inspector.noActiveTasks}</div>
        ) : (
          <ul className="space-y-1.5 pt-0.5 text-[12px]">
            {todos.map((item) => (
              <li key={item.id} className="flex items-start gap-2">
                {item.status === "completed" ? (
                  <CheckCircle2 className="mt-0.5 h-3.5 w-3.5 shrink-0 text-[var(--success)]" />
                ) : item.status === "in_progress" ? (
                  <span className="mt-1.5 h-2 w-2 shrink-0 rounded-full bg-[var(--warning)]" />
                ) : (
                  <Circle className="mt-0.5 h-3.5 w-3.5 shrink-0 text-[var(--text-tertiary)]" />
                )}
                <span
                  className={
                    item.status === "completed"
                      ? "text-[var(--text-tertiary)] line-through"
                      : item.status === "in_progress"
                        ? "text-[var(--warning)]"
                        : "text-[var(--text-secondary)]"
                  }
                >
                  {item.content}
                </span>
              </li>
            ))}
          </ul>
        )}
      </section>

      {mcpServers.length > 0 && (
        <>
          <div className="h-px bg-[var(--surface-raised)]" />
          <section className="space-y-1.5">
            <div className="flex items-center justify-between text-[12px] font-medium text-[var(--text-tertiary)]">
              <span className="inline-flex items-center gap-1.5">
                <Plug className="h-3 w-3 text-[var(--text-tertiary)]" />
                <span>MCP</span>
              </span>
              <span className="font-mono text-[11px] text-[var(--text-tertiary)]">
                {mcpServers.length}
              </span>
            </div>
            <ul className="space-y-1 text-[12px]">
              {mcpServers.map((srv) => (
                <li
                  key={srv.server}
                  className="flex items-center justify-between gap-2 text-[var(--text-secondary)]"
                >
                  <span className="truncate">{srv.server}</span>
                  <span className="shrink-0 font-mono text-[11px] text-[var(--text-tertiary)]">
                    {srv.state} · {srv.tools}t
                  </span>
                </li>
              ))}
            </ul>
          </section>
        </>
      )}

      <div className="h-px bg-[var(--surface-raised)]" />

      {/* Outputs & Recent */}
      <section className="space-y-1.5">
        <div className="flex items-center justify-between text-[12px] font-medium text-[var(--text-tertiary)]">
          <span>{outputPaths.length > 0 ? t.inspector.outputs : t.inspector.outputsAndRecent}</span>
          <span className="font-mono text-[11px] text-[var(--text-tertiary)]">
            {outputPaths.length > 0 ? outputPaths.length : recentSessions.length}
          </span>
        </div>
        {outputPaths.length > 0 ? (
          <ul className="space-y-1">
            {outputPaths.map((filePath) => (
              <li
                key={filePath}
                title={filePath}
                className="flex items-center gap-2 rounded-[var(--radius-md)] px-2 py-1 text-[12px] text-[var(--text-secondary)] transition-colors hover:bg-[var(--surface-raised)] hover:text-[var(--text-primary)]"
              >
                <FileText className="h-3 w-3 shrink-0 text-[var(--text-tertiary)]" />
                <span className="truncate font-mono text-[11px]">{filePath}</span>
              </li>
            ))}
          </ul>
        ) : recentSessions.length === 0 ? (
          <div className="text-[12px] text-[var(--text-tertiary)]">{t.inspector.noSavedSessions}</div>
        ) : (
          <ul className="space-y-1">
            {recentSessions.slice(0, 5).map((item) => {
              const isCurrent = item.id === activeSessionId;
              return (
                <li
                  key={item.id}
                  className={`flex items-start gap-2 rounded-[var(--radius-md)] px-2 py-1.5 text-[12px] transition-colors ${
                    isCurrent
                      ? "bg-[var(--surface-raised)] text-[var(--text-primary)]"
                      : "text-[var(--text-secondary)]"
                  }`}
                >
                  <FileText className="mt-0.5 h-3 w-3 shrink-0 text-[var(--text-tertiary)]" />
                  <div className="min-w-0 flex-1">
                    <div className="truncate font-medium">
                      {item.title ?? "Untitled session"}
                    </div>
                    <div className="truncate text-[11px] text-[var(--text-tertiary)]">
                      {item.work_mode.toUpperCase()} · {item.profile}
                    </div>
                  </div>
                </li>
              );
            })}
          </ul>
        )}
      </section>
    </aside>
  );
}
