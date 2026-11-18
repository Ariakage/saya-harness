import { createRequire } from "node:module";
import path from "node:path";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

function resolveKatexCssPath(): string | undefined {
  try {
    const require = createRequire(import.meta.url);
    const mathEntry = require.resolve("@streamdown/math");
    const mathRequire = createRequire(mathEntry);
    const rehypeKatexEntry = mathRequire.resolve("rehype-katex");
    const rehypeKatexRequire = createRequire(rehypeKatexEntry);
    return rehypeKatexRequire.resolve("katex/dist/katex.min.css");
  } catch {
    return undefined;
  }
}

const katexCssPath = resolveKatexCssPath();

export default defineConfig({
  plugins: [react(), tailwindcss()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: "127.0.0.1",
  },
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
      ...(katexCssPath ? { "katex/dist/katex.min.css": katexCssPath } : {}),
    },
  },
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["./src/test/setup.ts"],
  },
});
