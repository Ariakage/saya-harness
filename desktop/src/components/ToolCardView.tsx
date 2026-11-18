import { Bot, ChevronRight, Globe, Pencil, Terminal } from "lucide-react";
import { useMemo, useState } from "react";
import type { DesktopMessage } from "../store/useAppStore";
import { SessionMediaPreview } from "./AttachmentPreview";

export interface ToolCardViewProps {
  message: DesktopMessage;
  sessionId?: string | null;
}

interface ParsedDiffLine {
  kind: "hunk" | "add" | "del" | "ctx";
  lineNumber: number | null;
  sign: string;
  code: string;
}

interface ParsedDiff {
  filePath: string;
  addCount: number;
  delCount: number;
  hunkCount: number;
  lines: ParsedDiffLine[];
}

interface ParsedSubagentStep {
  tool: string;
  detail: string;
  status: string | null;
}

const DEFAULT_VISIBLE_DIFF_LINES = 12;

function isEditToolName(name: string): boolean {
  return /(edit|write|patch|replace|modify|create_file)/i.test(name);
}

function isBrowserToolName(name: string): boolean {
  return /(browser|navigate|webpage|url|playwright|fetch)/i.test(name);
}

function stripDiffPathPrefix(raw: string): string {
  const cleaned = raw.trim().replace(/^["']|["']$/g, "");
  return cleaned.replace(/^[ab]\//, "");
}

function parseUnifiedDiff(
  rawText: string | undefined | null,
  fallbackPath?: string | null
): ParsedDiff | null {
  if (!rawText) return null;
  const rawLines = rawText.split(/\r?\n/);

  let detectedPath = fallbackPath ? fallbackPath.split("·")[0]?.trim() ?? "" : "";
  let hasDiffStructure = false;
  let hunkCount = 0;
  let addCount = 0;
  let delCount = 0;
  let oldLine = 1;
  let newLine = 1;
  const parsedLines: ParsedDiffLine[] = [];

  for (const line of rawLines) {
    if (line.startsWith("--- ")) {
      const candidate = stripDiffPathPrefix(line.slice(4));
      if (candidate && candidate !== "/dev/null" && !detectedPath) {
        detectedPath = candidate;
      }
      hasDiffStructure = true;
      continue;
    }
    if (line.startsWith("+++ ")) {
      const candidate = stripDiffPathPrefix(line.slice(4));
      if (candidate && candidate !== "/dev/null") {
        detectedPath = candidate;
      }
      hasDiffStructure = true;
      continue;
    }
    if (line.startsWith("diff --git ")) {
      const parts = line.trim().split(/\s+/);
      const lastPart = parts[parts.length - 1];
      if (lastPart) {
        detectedPath = stripDiffPathPrefix(lastPart);
      }
      hasDiffStructure = true;
      continue;
    }
    if (line.startsWith("index ") || line.startsWith("new file ") || line.startsWith("deleted file ")) {
      continue;
    }

    const hunkMatch = line.match(/^@@\s+-(\d+)(?:,\d+)?\s+\+(\d+)(?:,\d+)?\s+@@/);
    if (hunkMatch) {
      hasDiffStructure = true;
      hunkCount += 1;
      oldLine = Number.parseInt(hunkMatch[1], 10);
      newLine = Number.parseInt(hunkMatch[2], 10);
      parsedLines.push({
        kind: "hunk",
        lineNumber: null,
        sign: "",
        code: line,
      });
      continue;
    }

    if (hasDiffStructure && line.startsWith("+") && !line.startsWith("+++")) {
      addCount += 1;
      parsedLines.push({
        kind: "add",
        lineNumber: newLine,
        sign: "+",
        code: line.slice(1),
      });
      newLine += 1;
      continue;
    }

    if (hasDiffStructure && line.startsWith("-") && !line.startsWith("---")) {
      delCount += 1;
      parsedLines.push({
        kind: "del",
        lineNumber: oldLine,
        sign: "-",
        code: line.slice(1),
      });
      oldLine += 1;
      continue;
    }

    if (hasDiffStructure && line.startsWith(" ")) {
      parsedLines.push({
        kind: "ctx",
        lineNumber: newLine,
        sign: " ",
        code: line.slice(1),
      });
      oldLine += 1;
      newLine += 1;
    }
  }

  if (!hasDiffStructure || (addCount === 0 && delCount === 0 && hunkCount === 0)) {
    return null;
  }

  return {
    filePath: detectedPath || "patch",
    addCount,
    delCount,
    hunkCount: Math.max(1, hunkCount),
    lines: parsedLines,
  };
}

function parseSubagentSteps(rawText: string | undefined | null): ParsedSubagentStep[] | null {
  if (!rawText) return null;
  const lines = rawText
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean);
  if (lines.length === 0) return null;

  const steps: ParsedSubagentStep[] = [];
  for (const line of lines) {
    const match = line.match(/^([A-Za-z0-9_.-]+)\s+(.*?)\s*->\s*(.+)$/);
    if (!match) {
      return null;
    }
    steps.push({
      tool: match[1],
      detail: match[2].trim(),
      status: match[3].trim(),
    });
  }
  return steps;
}

function renderStepStatusBadge(status: string | null) {
  if (!status) return null;
  const normalized = status.toLowerCase();
  if (normalized === "done" || normalized === "ok" || normalized === "completed") {
    return <span className="soyo-pill py-0 text-[10.5px] text-[#4ade80]">{status}</span>;
  }
  if (normalized === "running" || normalized === "in_progress") {
    return (
      <span className="soyo-pill py-0 text-[10.5px] text-[#fac454]">
        <span className="h-1.5 w-1.5 rounded-full bg-[#fac454] soyo-pulse-dot" />
        {status}
      </span>
    );
  }
  if (normalized === "error" || normalized === "failed") {
    return <span className="soyo-pill py-0 text-[10.5px] text-[#f87171]">{status}</span>;
  }
  return (
    <span className="ml-auto shrink-0 font-mono text-[11px] text-[#6e6e6e]">
      {status}
    </span>
  );
}

export function ToolCardView({ message, sessionId = null }: ToolCardViewProps) {
  const toolName = message.toolName ?? message.label;
  const toolNameLower = toolName.toLowerCase();
  const isSubagent =
    toolNameLower.includes("subagent") ||
    toolNameLower.includes("delegate") ||
    toolNameLower.includes("agent_");

  const parsedDiff = useMemo(
    () =>
      parseUnifiedDiff(message.text, message.toolSummary) ??
      parseUnifiedDiff(message.toolRequestFormatted, message.toolSummary),
    [message.text, message.toolRequestFormatted, message.toolSummary]
  );

  const subagentSteps = useMemo(
    () => (isSubagent ? parseSubagentSteps(message.text) : null),
    [isSubagent, message.text]
  );

  const [expanded, setExpanded] = useState(
    message.toolOk === false ||
      message.mediaRefs.length > 0 ||
      parsedDiff !== null ||
      (isSubagent && (message.toolState === "running" || Boolean(subagentSteps)))
  );
  const [showAllDiffLines, setShowAllDiffLines] = useState(false);

  const statusPill = (() => {
    switch (message.toolState) {
      case "requested":
        return <span className="soyo-pill text-[#7aa2f7]">requested</span>;
      case "running":
        return (
          <span className="soyo-pill text-[#fac454]">
            <span className="h-1.5 w-1.5 rounded-full bg-[#fac454] soyo-pulse-dot" />
            running
          </span>
        );
      case "error":
        return <span className="soyo-pill text-[#f87171]">error</span>;
      case "ok":
        return <span className="soyo-pill text-[#4ade80]">done</span>;
      default:
        return message.toolOk === true ? (
          <span className="soyo-pill text-[#4ade80]">done</span>
        ) : message.toolOk === false ? (
          <span className="soyo-pill text-[#f87171]">error</span>
        ) : (
          <span className="soyo-pill text-[#a3a3a3]">tool</span>
        );
    }
  })();

  if (isSubagent) {
    return (
      <div
        data-testid="tool-card"
        className="soyo-msg-enter rounded-[10px] border border-[#2c2c2c] border-l-2 border-l-[#d9a08e] bg-[#171717] px-3 py-2.5 text-[12.5px] transition-colors hover:border-[#383838] hover:border-l-[#d9a08e]"
      >
        <button
          type="button"
          onClick={() => setExpanded((prev) => !prev)}
          aria-expanded={expanded}
          className="flex w-full items-center gap-2 text-left"
        >
          <Bot className="h-3.5 w-3.5 shrink-0 text-[#d9a08e]" />
          <span className="font-medium text-[#e8e8e8]">
            {message.toolName ?? "Subagent"}
          </span>
          <span className="soyo-pill py-0 text-[11px] text-[#7aa2f7]">subagent</span>
          {statusPill}
          <span className="flex-1" />
          {message.toolSummary && (
            <span className="max-w-xs truncate font-mono text-[11.5px] text-[#6e6e6e]">
              {message.toolSummary}
            </span>
          )}
          <ChevronRight
            className={`h-3.5 w-3.5 shrink-0 text-[#6e6e6e] transition-transform duration-200 ease-out ${
              expanded ? "rotate-90" : ""
            }`}
          />
        </button>

        {expanded && (
          <div className="soyo-panel-enter ml-[15px] mt-2 space-y-2 border-l border-[#2c2c2c] pl-3 text-xs text-[#8a8a8a]">
            {message.toolRequestFormatted && !subagentSteps && (
              <pre className="max-h-40 overflow-auto whitespace-pre-wrap rounded-[8px] border border-[#262626] bg-[#121212] p-2 font-mono text-[11px] text-[#c9c9c9]">
                {message.toolRequestFormatted}
              </pre>
            )}
            {message.toolError && (
              <div className="rounded-[8px] border border-[#f87171]/35 bg-[#1c1515] px-2.5 py-1.5 text-[11px] text-[#f87171]">
                {message.toolError.code}: {message.toolError.message}
              </div>
            )}
            {subagentSteps ? (
              <div className="space-y-1.5">
                {subagentSteps.map((step, idx) => (
                  <div
                    key={`${step.tool}-${idx}`}
                    className="flex items-center gap-2 text-[12px] text-[#8a8a8a]"
                  >
                    <Terminal className="h-3 w-3 shrink-0 text-[#6e6e6e]" />
                    <span className="font-mono text-[#c9c9c9]">{step.tool}</span>
                    {step.detail && (
                      <span className="truncate font-mono text-[11.5px] text-[#8a8a8a]">
                        {step.detail}
                      </span>
                    )}
                    {renderStepStatusBadge(step.status)}
                  </div>
                ))}
              </div>
            ) : (
              message.text && (
                <pre className="max-h-56 overflow-auto whitespace-pre-wrap font-mono text-[11.5px] text-[#c9c9c9]">
                  {message.text}
                </pre>
              )
            )}
            <SessionMediaPreview
              sessionId={sessionId}
              mediaRefs={message.mediaRefs}
            />
          </div>
        )}
      </div>
    );
  }

  const RowIcon = isEditToolName(toolNameLower)
    ? Pencil
    : isBrowserToolName(toolNameLower)
      ? Globe
      : Terminal;

  const visibleDiffLines = parsedDiff
    ? showAllDiffLines
      ? parsedDiff.lines
      : parsedDiff.lines.slice(0, DEFAULT_VISIBLE_DIFF_LINES)
    : [];
  const hiddenDiffCount = parsedDiff
    ? Math.max(0, parsedDiff.lines.length - visibleDiffLines.length)
    : 0;

  return (
    <div className="soyo-msg-enter text-[12.5px]" data-testid="tool-card">
      <button
        type="button"
        onClick={() => setExpanded((prev) => !prev)}
        aria-expanded={expanded}
        className="group flex w-full items-center gap-2.5 rounded-[8px] px-1 py-[6px] text-left text-[#9a9a9a] transition-colors hover:bg-[#1a1a1a]/60 hover:text-[#e8e8e8]"
      >
        <RowIcon className="h-3.5 w-3.5 shrink-0 text-[#8a8a8a] transition-colors group-hover:text-[#c9c9c9]" />
        <span className="truncate font-mono text-[#e8e8e8]">
          {message.toolName ?? message.label}
        </span>
        {statusPill}
        {message.truncated && (
          <span className="soyo-pill text-[#fac454]">truncated</span>
        )}
        <span className="flex-1" />
        {parsedDiff ? (
          <span className="flex items-center gap-2 font-mono text-[11.5px]">
            <span className="max-w-[220px] truncate text-[#6e6e6e]">
              {parsedDiff.filePath}
            </span>
            {parsedDiff.addCount > 0 && (
              <span className="font-semibold text-[#4ade80]">
                +{parsedDiff.addCount}
              </span>
            )}
            {parsedDiff.delCount > 0 && (
              <span className="font-semibold text-[#f87171]">
                -{parsedDiff.delCount}
              </span>
            )}
          </span>
        ) : (
          message.toolSummary && (
            <span className="max-w-sm truncate font-mono text-[11.5px] text-[#6e6e6e]">
              {message.toolSummary}
            </span>
          )
        )}
        <ChevronRight
          className={`h-3.5 w-3.5 shrink-0 text-[#6e6e6e] transition-transform duration-200 ease-out ${
            expanded ? "rotate-90" : ""
          }`}
        />
      </button>

      {expanded && (
        <div className="soyo-panel-enter ml-[7px] mt-1 space-y-2 border-l-2 border-[#2c2c2c] pb-1.5 pl-3.5 text-xs">
          {message.toolReviewNote && (
            <div className="rounded-[8px] border border-[#262626] bg-[#1a1a1a] px-2.5 py-1.5 text-[11px] text-[#a3a3a3]">
              Review: {message.toolReviewNote}
            </div>
          )}

          {message.toolError && (
            <div className="rounded-[8px] border border-[#f87171]/35 bg-[#1c1515] px-2.5 py-1.5 text-[11px] text-[#f87171]">
              {message.toolError.code}: {message.toolError.message}
            </div>
          )}

          {parsedDiff ? (
            <div className="soyo-diff" data-testid="inline-diff-card">
              <div className="soyo-diff-header">
                <span className="truncate text-[#c9c9c9]">{parsedDiff.filePath}</span>
                {parsedDiff.addCount > 0 && (
                  <span className="font-semibold text-[#4ade80]">
                    +{parsedDiff.addCount}
                  </span>
                )}
                {parsedDiff.delCount > 0 && (
                  <span className="font-semibold text-[#f87171]">
                    -{parsedDiff.delCount}
                  </span>
                )}
                <span className="flex-1" />
                <span className="text-[#5e5e5e]">
                  {parsedDiff.hunkCount} {parsedDiff.hunkCount === 1 ? "hunk" : "hunks"} ›
                </span>
              </div>
              <div className="soyo-diff-body">
                {visibleDiffLines.map((line, idx) => (
                  <div
                    key={`${idx}-${line.kind}`}
                    className={`soyo-diff-line ${
                      line.kind === "add"
                        ? "is-add"
                        : line.kind === "del"
                          ? "is-del"
                          : line.kind === "hunk"
                            ? "is-hunk"
                            : ""
                    }`}
                  >
                    <span className="soyo-diff-line__ln">
                      {line.lineNumber ?? ""}
                    </span>
                    <span className="soyo-diff-line__sg">{line.sign}</span>
                    <span className="soyo-diff-line__code">{line.code}</span>
                  </div>
                ))}
              </div>
              {(hiddenDiffCount > 0 || showAllDiffLines) &&
                parsedDiff.lines.length > DEFAULT_VISIBLE_DIFF_LINES && (
                  <button
                    type="button"
                    onClick={() => setShowAllDiffLines((prev) => !prev)}
                    className="w-full border-t border-[#1f1f1f] px-3 py-1.5 text-center text-[11px] text-[#6e6e6e] transition-colors hover:bg-[#181818] hover:text-[#c9c9c9]"
                  >
                    {showAllDiffLines
                      ? "Collapse diff preview ‹"
                      : `${hiddenDiffCount} lines hidden · expand ›`}
                  </button>
                )}
            </div>
          ) : (
            <>
              {message.toolRequestFormatted &&
                message.toolRequestFormatted !== message.text && (
                  <div>
                    <div className="mb-1 text-[10px] uppercase tracking-wider text-[#6e6e6e]">
                      Request
                    </div>
                    <pre className="max-h-40 overflow-auto whitespace-pre-wrap rounded-[8px] border border-[#262626] bg-[#161616] p-2 font-mono text-[11px] text-[#c9d1d9]">
                      {message.toolRequestFormatted}
                    </pre>
                  </div>
                )}
              {message.text && (
                <div>
                  <div className="mb-1 text-[10px] uppercase tracking-wider text-[#6e6e6e]">
                    Output
                  </div>
                  <pre className="max-h-64 overflow-auto whitespace-pre-wrap rounded-[8px] border border-[#262626] bg-[#161616] p-2 font-mono text-[11px] text-[#c9d1d9]">
                    {message.text}
                  </pre>
                </div>
              )}
            </>
          )}

          <SessionMediaPreview
            sessionId={sessionId}
            mediaRefs={message.mediaRefs}
          />
        </div>
      )}
    </div>
  );
}
