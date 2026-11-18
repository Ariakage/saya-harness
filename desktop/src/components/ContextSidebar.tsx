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

// Color rules (§3.2 & Soyo Dark Frame 02):
// Messages primary segment uses #d9a08e, followed by orange/green/ochre/neutral free space.
const CONTEXT_BAR_COLORS = [
  "#d9a08e",
  "#e07b3f",
  "#3fbf7f",
  "#bf9f3f",
  "#8aa8d8",
  "#6b6b6b",
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
  const totalContextTokens = contextEntries.reduce(
    (acc, item) => acc + item.tokens,
    0
  );
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
      className="flex w-[308px] shrink-0 flex-col gap-3.5 overflow-y-auto border-l border-[#1f1f1f] bg-[#0f0f0f] p-3.5 text-xs"
      data-testid="context-sidebar"
    >
      {/* Runtime */}
      <section className="space-y-1.5">
        <div className="flex items-center justify-between text-[12px] font-medium text-[#8a8a8a]">
          <span>{t.inspector.runtime}</span>
          <span
            className="text-[10px]"
            style={{ color: modeStyle.color }}
            title={`Active mode: ${modeStyle.label}`}
          >
            ●
          </span>
        </div>
        <div className="space-y-1 text-[12px]">
          <div className="flex items-center justify-between gap-2 py-0.5">
            <span className="text-[#7a7a7a]">{t.inspector.profile}</span>
            <span className="truncate font-mono text-[11.5px] text-[#c9c9c9]">
              {bootstrap?.selected_profile ?? t.inspector.unset}
            </span>
          </div>
          <div className="flex items-center justify-between gap-2 py-0.5">
            <span className="text-[#7a7a7a]">{t.inspector.model}</span>
            <span className="truncate font-mono text-[11.5px] text-[#c9c9c9]">
              {bootstrap?.selected_model ?? t.inspector.unset}
            </span>
          </div>
          <div className="flex items-center justify-between gap-2 py-0.5">
            <span className="text-[#7a7a7a]">{t.inspector.sandbox}</span>
            <span className="truncate font-mono text-[11.5px] text-[#c9c9c9]">
              {bootstrap?.sandbox_backend ?? "default"}
            </span>
          </div>
          <div className="flex items-center justify-between gap-2 py-0.5">
            <span className="text-[#7a7a7a]">{t.inspector.workspace}</span>
            <span
              className="truncate font-mono text-[11px] text-[#c9c9c9]"
              title={bootstrap?.workspace}
            >
              {basenameFromWorkspace(bootstrap?.workspace) || t.inspector.unset}
            </span>
          </div>
        </div>
      </section>

      <div className="h-px bg-[#1f1f1f]" />

      {/* Context window */}
      <section className="space-y-2">
        <div className="flex items-center justify-between text-[12px] font-medium text-[#8a8a8a]">
          <span>{t.inspector.contextWindow}</span>
          <span className="font-mono text-[11.5px] text-[#6e6e6e]">
            {contextEstimated ? "~" : ""}
            {formatTokenCount(totalContextTokens)}
            {windowTokens
              ? ` / ${formatTokenCount(windowTokens)} (${usagePercent}%)`
              : ""}
          </span>
        </div>

        {contextEntries.length === 0 ? (
          <div className="text-[11.5px] text-[#6e6e6e]">
            {t.inspector.contextEmpty}
          </div>
        ) : (
          <div className="space-y-2.5">
            <div className="flex h-[6px] w-full overflow-hidden rounded-[3px] bg-[#2a2a2a]">
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
                    backgroundColor: "#3a3a3a",
                  }}
                  title={`Free space: ${freeTokens} tokens`}
                />
              )}
            </div>

            <ul className="space-y-1 text-[11.5px]">
              {contextEntries.map((entry, idx) => {
                const denom =
                  windowTokens && windowTokens > 0
                    ? windowTokens
                    : Math.max(1, totalContextTokens);
                const pct = Math.round((entry.tokens / denom) * 100);
                return (
                  <li
                    key={entry.label}
                    className="flex items-center justify-between text-[#a3a3a3]"
                  >
                    <span className="inline-flex items-center gap-1.5">
                      <span
                        className="h-2 w-2 rounded-[2px]"
                        style={{
                          backgroundColor:
                            CONTEXT_BAR_COLORS[idx % CONTEXT_BAR_COLORS.length],
                        }}
                      />
                      <span>{entry.label}</span>
                    </span>
                    <span className="font-mono text-[#7a7a7a]">
                      {formatTokenCount(entry.tokens)}
                      {windowTokens ? ` · ${pct}%` : ""}
                    </span>
                  </li>
                );
              })}
              {freeTokens > 0 && windowTokens && (
                <li className="flex items-center justify-between text-[#a3a3a3]">
                  <span className="inline-flex items-center gap-1.5">
                    <span className="h-2 w-2 rounded-[2px] bg-[#3a3a3a]" />
                    <span>{t.inspector.freeSpace}</span>
                  </span>
                  <span className="font-mono text-[#7a7a7a]">
                    {formatTokenCount(freeTokens)} · {Math.max(0, 100 - usagePercent)}%
                  </span>
                </li>
              )}
            </ul>

            {contextSkillsCount != null && (
              <div className="text-[11px] text-[#6e6e6e]">
                {contextSkillsCount} active skill(s)
              </div>
            )}
          </div>
        )}
      </section>

      <div className="h-px bg-[#1f1f1f]" />

      {/* Tasks */}
      <section className="space-y-1.5">
        <div className="flex items-center justify-between text-[12px] font-medium text-[#8a8a8a]">
          <span>{t.inspector.tasks}</span>
          <span className="font-mono text-[11px] text-[#6e6e6e]">
            {todos.filter((t) => t.status === "completed").length}/{todos.length}
          </span>
        </div>
        {todos.length === 0 ? (
          <div className="text-[11.5px] text-[#6e6e6e]">{t.inspector.noActiveTasks}</div>
        ) : (
          <ul className="space-y-1.5 pt-0.5 text-[12px]">
            {todos.map((item) => (
              <li key={item.id} className="flex items-start gap-2">
                {item.status === "completed" ? (
                  <CheckCircle2 className="mt-0.5 h-3.5 w-3.5 shrink-0 text-[#4ade80]" />
                ) : item.status === "in_progress" ? (
                  <span className="mt-1.5 h-2 w-2 shrink-0 rounded-full bg-[#fac454]" />
                ) : (
                  <Circle className="mt-0.5 h-3.5 w-3.5 shrink-0 text-[#6e6e6e]" />
                )}
                <span
                  className={
                    item.status === "completed"
                      ? "text-[#6e6e6e] line-through"
                      : item.status === "in_progress"
                        ? "text-[#fac454]"
                        : "text-[#a3a3a3]"
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
          <div className="h-px bg-[#1f1f1f]" />
          <section className="space-y-1.5">
            <div className="flex items-center justify-between text-[12px] font-medium text-[#8a8a8a]">
              <span className="inline-flex items-center gap-1.5">
                <Plug className="h-3 w-3 text-[#8a8a8a]" />
                <span>MCP</span>
              </span>
              <span className="font-mono text-[11px] text-[#6e6e6e]">
                {mcpServers.length}
              </span>
            </div>
            <ul className="space-y-1 text-[11.5px]">
              {mcpServers.map((srv) => (
                <li
                  key={srv.server}
                  className="flex items-center justify-between gap-2 text-[#a3a3a3]"
                >
                  <span className="truncate">{srv.server}</span>
                  <span className="shrink-0 font-mono text-[10.5px] text-[#6e6e6e]">
                    {srv.state} · {srv.tools}t
                  </span>
                </li>
              ))}
            </ul>
          </section>
        </>
      )}

      <div className="h-px bg-[#1f1f1f]" />

      {/* Outputs & Recent */}
      <section className="space-y-1.5">
        <div className="flex items-center justify-between text-[12px] font-medium text-[#8a8a8a]">
          <span>{outputPaths.length > 0 ? t.inspector.outputs : t.inspector.outputsAndRecent}</span>
          <span className="font-mono text-[11px] text-[#6e6e6e]">
            {outputPaths.length > 0 ? outputPaths.length : recentSessions.length}
          </span>
        </div>
        {outputPaths.length > 0 ? (
          <ul className="space-y-1">
            {outputPaths.map((filePath) => (
              <li
                key={filePath}
                title={filePath}
                className="flex items-center gap-2 rounded-[7px] px-2 py-1 text-[11.5px] text-[#a3a3a3] transition-colors hover:bg-[#1a1a1a] hover:text-[#e8e8e8]"
              >
                <FileText className="h-3 w-3 shrink-0 text-[#6e6e6e]" />
                <span className="truncate font-mono text-[11px]">{filePath}</span>
              </li>
            ))}
          </ul>
        ) : recentSessions.length === 0 ? (
          <div className="text-[11.5px] text-[#6e6e6e]">{t.inspector.noSavedSessions}</div>
        ) : (
          <ul className="space-y-1">
            {recentSessions.slice(0, 5).map((item) => {
              const isCurrent = item.id === activeSessionId;
              return (
                <li
                  key={item.id}
                  className={`flex items-start gap-2 rounded-[7px] px-2 py-1.5 text-[11.5px] transition-colors ${
                    isCurrent
                      ? "bg-[#1c1c1c] text-[#e8e8e8]"
                      : "text-[#a3a3a3]"
                  }`}
                >
                  <FileText className="mt-0.5 h-3 w-3 shrink-0 text-[#6e6e6e]" />
                  <div className="min-w-0 flex-1">
                    <div className="truncate font-medium">
                      {item.title ?? "Untitled session"}
                    </div>
                    <div className="truncate text-[10.5px] text-[#6e6e6e]">
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
