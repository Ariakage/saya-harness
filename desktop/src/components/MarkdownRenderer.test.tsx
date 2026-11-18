import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import {
  MarkdownRenderer,
  preprocessMarkdownForDisplay,
} from "./MarkdownRenderer";

vi.mock("@streamdown/mermaid", () => ({
  createMermaidPlugin: () => ({
    name: "mermaid",
    type: "diagram",
    language: "mermaid",
    getMermaid: () => ({
      initialize: () => {},
      render: async () => ({ svg: "<svg></svg>" }),
    }),
  }),
}));

describe("MarkdownRenderer", () => {
  it("strips ANSI and C0 control characters before rendering", () => {
    const dirty = "Hello \u001b[31mred\u001b[0m\u0007 world";
    render(<MarkdownRenderer content={dirty} mode="render" />);
    expect(screen.getByTestId("markdown-render-view").textContent).toContain(
      "Hello red world"
    );
  });

  it("switches cleanly between render and source views", () => {
    const content = "# Title\n\n**bold**";
    const { rerender } = render(
      <MarkdownRenderer content={content} mode="source" />
    );
    const sourceEl = screen.getByTestId("markdown-source-view");
    expect(sourceEl.textContent).toContain("# Title");
    expect(sourceEl.textContent).toContain("**bold**");

    rerender(<MarkdownRenderer content={content} mode="render" />);
    const renderEl = screen.getByTestId("markdown-render-view");
    expect(renderEl.querySelector("h1")?.textContent).toBe("Title");
  });

  it("degrades unsupported or over-budget Mermaid blocks into source fallback cards", () => {
    const disallowed = "```mermaid\nmindmap\n  root((test))\n```";
    render(<MarkdownRenderer content={disallowed} mode="render" />);
    expect(screen.getByText(/source fallback/i)).toBeTruthy();
    expect(screen.getByText(/mindmap/)).toBeTruthy();

    const tooManyLines = [
      "```mermaid",
      "flowchart LR",
      ...Array.from({ length: 130 }, (_, i) => `  N${i} --> N${i + 1}`),
      "```",
    ].join("\n");
    const preprocessed = preprocessMarkdownForDisplay(tooManyLines);
    expect(preprocessed).toContain("```mermaid-fallback");
  });

  it("does not execute raw script tags inside Markdown", () => {
    const md = "Safe text\n\n<script>window.__xss = true;</script>\n";
    const { container } = render(
      <MarkdownRenderer content={md} mode="render" />
    );
    expect(container.querySelector("script")).toBeNull();
    expect((window as unknown as { __xss?: boolean }).__xss).toBeUndefined();
  });
});
