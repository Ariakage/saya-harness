import { createCodePlugin } from "@streamdown/code";
import { createMathPlugin } from "@streamdown/math";
import { createMermaidPlugin } from "@streamdown/mermaid";
import { AlertTriangle, Code2, Eye, GitBranch } from "lucide-react";
import { type ComponentProps, type ReactNode, useMemo } from "react";
import { Streamdown, type MermaidErrorComponentProps } from "streamdown";
import { sanitizeDisplayText } from "../lib/sanitize";
import type { MarkdownDisplayMode } from "../store/useAppStore";

const MAX_MERMAID_BYTES = 8192;
const MAX_MERMAID_LINES = 128;
const MAX_MATH_BYTES = 16384;
const MAX_CODE_BYTES = 32768;
const MAX_CODE_LINE_BYTES = 2048;

const ALLOWED_MERMAID_STARTERS = [
  "flowchart",
  "graph",
  "sequencediagram",
  "statediagram",
  "statediagram-v2",
  "classdiagram",
  "erdiagram",
];

const codePlugin = createCodePlugin({
  themes: ["github-dark", "github-dark"],
});

const mathPlugin = createMathPlugin({
  singleDollarTextMath: true,
});

const mermaidPlugin = createMermaidPlugin({
  config: {
    theme: "dark",
    securityLevel: "strict",
  },
});

export function validateMermaidBudget(source: string): string | null {
  const bytes = new TextEncoder().encode(source).length;
  if (bytes > MAX_MERMAID_BYTES) {
    return `exceeds 8KiB limit (${bytes} bytes)`;
  }
  const lines = source.split(/\r?\n/);
  if (lines.length > MAX_MERMAID_LINES) {
    return `exceeds 128 lines (${lines.length} lines)`;
  }
  const firstMeaningful = lines
    .map((line) => line.trim())
    .find((line) => line.length > 0 && !line.startsWith("%%"));
  if (!firstMeaningful) {
    return "empty diagram";
  }
  const headerToken = firstMeaningful.split(/[\s{(:]/, 1)[0]?.toLowerCase() ?? "";
  if (!ALLOWED_MERMAID_STARTERS.includes(headerToken)) {
    return `unsupported diagram type '${headerToken || "unknown"}'`;
  }
  if (/&#[0-9a-fx]+;|<script|<iframe|javascript:/i.test(source)) {
    return "disallowed HTML/entity content";
  }
  return null;
}

export function preprocessMarkdownForDisplay(raw: string): string {
  const clean = sanitizeDisplayText(raw);
  const lines = clean.split("\n");
  const out: string[] = [];

  let i = 0;
  while (i < lines.length) {
    const fenceMatch = lines[i].match(/^(\s*)(`{3,}|~{3,})([^`~]*)$/);
    if (fenceMatch) {
      const indent = fenceMatch[1];
      const fence = fenceMatch[2];
      const info = fenceMatch[3].trim();
      const lang = info.split(/\s+/, 1)[0]?.toLowerCase() ?? "";
      const closeRe = new RegExp(`^\\s*${fence[0]}{${fence.length},}\\s*$`);
      const bodyLines: string[] = [];
      i += 1;
      while (i < lines.length && !closeRe.test(lines[i])) {
        bodyLines.push(lines[i]);
        i += 1;
      }
      const closed = i < lines.length;
      if (closed) {
        i += 1;
      }
      const body = bodyLines.join("\n");

      if (lang === "mermaid") {
        const budgetReason = validateMermaidBudget(body);
        if (budgetReason) {
          out.push(`${indent}${fence}mermaid-fallback ${budgetReason}`);
          out.push(...bodyLines);
          out.push(`${indent}${fence}`);
          continue;
        }
      } else if (lang === "math" || lang === "latex" || lang === "tex") {
        const bytes = new TextEncoder().encode(body).length;
        if (bytes <= MAX_MATH_BYTES && closed) {
          out.push(`${indent}$$`);
          out.push(...bodyLines);
          out.push(`${indent}$$`);
          continue;
        }
        out.push(`${indent}${fence}math-fallback exceeds math budget`);
        out.push(...bodyLines);
        out.push(`${indent}${fence}`);
        continue;
      } else if (
        new TextEncoder().encode(body).length > MAX_CODE_BYTES ||
        bodyLines.some((l) => new TextEncoder().encode(l).length > MAX_CODE_LINE_BYTES)
      ) {
        out.push(`${indent}${fence}code-fallback`);
        out.push(...bodyLines);
        if (closed) {
          out.push(`${indent}${fence}`);
        }
        continue;
      }

      out.push(`${indent}${fence}${info ? info : ""}`);
      out.push(...bodyLines);
      if (closed) {
        out.push(`${indent}${fence}`);
      }
      continue;
    }

    // Outside code fences: normalize \[...\] and \(...\) for remark-math.
    const converted = lines[i]
      .replace(/\\\[((?:.|\s)*?)\\\]/g, (_m, inner: string) => `$$${inner}$$`)
      .replace(/\\\(((?:.|\s)*?)\\\)/g, (_m, inner: string) => `$${inner}$`);
    out.push(converted);
    i += 1;
  }

  return out.join("\n");
}

function MermaidFallbackCard({ chart, error }: MermaidErrorComponentProps) {
  return (
    <div className="my-2 rounded-[10px] border border-[#4a3f2a] bg-[#1c1a15] p-3 text-xs">
      <div className="mb-2 flex items-center justify-between gap-2 text-[#fac454]">
        <span className="inline-flex items-center gap-1.5 font-medium">
          <GitBranch className="h-3.5 w-3.5" />
          Mermaid
        </span>
        <span className="devin-pill py-0 text-[10px] uppercase tracking-wide text-[#fac454]">
          source fallback
        </span>
      </div>
      {error && (
        <div className="mb-2 flex items-center gap-1 text-[11px] text-[#a3a3a3]">
          <AlertTriangle className="h-3 w-3 shrink-0 text-[#fac454]" />
          <span>{sanitizeDisplayText(error)}</span>
        </div>
      )}
      <pre className="overflow-x-auto rounded-[8px] border border-[#2e2820] bg-[#121110] p-2.5 text-xs text-[#c9c9c9]">
        <code>{sanitizeDisplayText(chart)}</code>
      </pre>
    </div>
  );
}

function extractTextContent(node: ReactNode): string {
  if (typeof node === "string" || typeof node === "number") {
    return String(node);
  }
  if (Array.isArray(node)) {
    return node.map(extractTextContent).join("");
  }
  if (node && typeof node === "object" && "props" in node) {
    const element = node as { props?: { children?: ReactNode } };
    return extractTextContent(element.props?.children);
  }
  return "";
}

type CodeProps = ComponentProps<"code"> & { node?: unknown };
type AnchorProps = ComponentProps<"a"> & { node?: unknown };

export interface MarkdownRendererProps {
  content: string;
  mode: MarkdownDisplayMode;
  streaming?: boolean;
  onToggleMode?: () => void;
  showModeToggle?: boolean;
}

export function MarkdownRenderer({
  content,
  mode,
  streaming = false,
  onToggleMode,
  showModeToggle = false,
}: MarkdownRendererProps) {
  const sanitized = useMemo(() => sanitizeDisplayText(content), [content]);
  const prepared = useMemo(
    () => (mode === "render" ? preprocessMarkdownForDisplay(content) : sanitized),
    [content, mode, sanitized]
  );

  if (mode === "source") {
    return (
      <div className="space-y-1.5" data-testid="markdown-source-view">
        {showModeToggle && onToggleMode && (
          <div className="flex justify-end">
            <button
              type="button"
              onClick={onToggleMode}
              className="devin-pill-btn text-[11px] text-[#c9c9c9] hover:text-[#e8e8e8]"
            >
              <Eye className="h-3 w-3" />
              Render
            </button>
          </div>
        )}
        <pre className="overflow-x-auto whitespace-pre-wrap rounded-[10px] border border-[#262626] bg-[#161616] p-3 text-xs leading-relaxed text-[#c9d1d9]">
          <code>{sanitized}</code>
        </pre>
      </div>
    );
  }

  return (
    <div className="soyo-markdown space-y-1.5" data-testid="markdown-render-view">
      {showModeToggle && onToggleMode && (
        <div className="flex justify-end">
          <button
            type="button"
            onClick={onToggleMode}
            className="devin-pill-btn text-[11px] text-[#a3a3a3] hover:text-[#e8e8e8]"
          >
            <Code2 className="h-3 w-3" />
            Source
          </button>
        </div>
      )}
      <Streamdown
        mode={streaming ? "streaming" : "static"}
        isAnimating={streaming}
        plugins={{
          code: codePlugin,
          mermaid: mermaidPlugin,
          math: mathPlugin,
        }}
        shikiTheme={["github-dark", "github-dark"]}
        mermaid={{
          config: { theme: "dark", securityLevel: "strict" },
          errorComponent: MermaidFallbackCard,
        }}
        components={{
          code: ({ className, children, ...props }: CodeProps) => {
            if (
              className?.includes("language-mermaid-fallback") ||
              className?.includes("language-math-fallback") ||
              className?.includes("language-code-fallback")
            ) {
              const rawText = extractTextContent(children);
              const kindLabel = className.includes("mermaid-fallback")
                ? "Mermaid"
                : className.includes("math-fallback")
                  ? "Math"
                  : "Code";
              return (
                <div className="my-2 rounded-[10px] border border-[#4a3f2a] bg-[#1c1a15] p-3 text-xs">
                  <div className="mb-1.5 flex items-center justify-between text-[#fac454]">
                    <span className="font-medium">{kindLabel}</span>
                    <span className="devin-pill text-[10px] uppercase tracking-wide text-[#fac454]">
                      source fallback
                    </span>
                  </div>
                  <pre className="overflow-x-auto rounded-[8px] border border-[#2e2820] bg-[#121110] p-2 text-xs text-[#c9c9c9]">
                    <code>{rawText}</code>
                  </pre>
                </div>
              );
            }
            return (
              <code className={className} {...props}>
                {children}
              </code>
            );
          },
          a: ({ href, children, onClick, ...props }: AnchorProps) => {
            const safeHref = href ?? "";
            if (safeHref.startsWith("#")) {
              return (
                <a
                  href={safeHref}
                  onClick={(e) => {
                    onClick?.(e);
                    const targetId = safeHref.slice(1);
                    const targetEl = document.getElementById(targetId);
                    if (targetEl && typeof targetEl.scrollIntoView === "function") {
                      e.preventDefault();
                      targetEl.scrollIntoView({
                        behavior: "smooth",
                        block: "nearest",
                      });
                    }
                  }}
                  {...props}
                >
                  {children}
                </a>
              );
            }
            return (
              <a
                href={safeHref}
                target="_blank"
                rel="noreferrer noopener"
                onClick={onClick}
                {...props}
              >
                {children}
              </a>
            );
          },
        }}
      >
        {prepared}
      </Streamdown>
    </div>
  );
}
