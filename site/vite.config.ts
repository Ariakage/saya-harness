import path from "node:path";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  clearScreen: false,
  appType: "mpa",
  build: {
    rollupOptions: {
      input: {
        main: path.resolve(__dirname, "index.html"),
        download: path.resolve(__dirname, "download.html"),
      },
    },
  },
  server: {
    port: 1421,
    strictPort: true,
    host: "127.0.0.1",
  },
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
});
