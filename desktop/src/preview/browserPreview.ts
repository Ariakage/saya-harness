import { mockIPC } from "@tauri-apps/api/mocks";
import type { SessionPageDto } from "../bindings";

let installed = false;

export function isBrowserPreview(): boolean {
  return (
    typeof window !== "undefined" &&
    import.meta.env.DEV &&
    (!("__TAURI_INTERNALS__" in window) ||
      new URLSearchParams(window.location.search).get("preview") === "1")
  );
}

export function installBrowserPreviewMock(): boolean {
  if (!isBrowserPreview() || installed) {
    return installed;
  }
  installed = true;

  const samplePage: SessionPageDto = {
    page: 1,
    page_size: 20,
    items: [
      {
        id: "01930000-0000-7000-8000-000000000001",
        workspace: "/workspace/soyo-harness",
        work_mode: "code",
        profile: "anthropic-official",
        title: "Implement desktop UI preview with Vite HMR",
        updated_at: Math.floor(Date.now() / 1000) - 180,
        last_status: "completed",
      },
      {
        id: "01930000-0000-7000-8000-000000000002",
        workspace: "/workspace/soyo-harness",
        work_mode: "plan",
        profile: "openai-responses",
        title: "Audit sandbox isolation and credential redaction",
        updated_at: Math.floor(Date.now() / 1000) - 3600,
        last_status: "completed",
      },
    ],
  };

  mockIPC((cmd, args) => {
    if (cmd === "list_sessions") {
      const page = (args as { page?: number } | undefined)?.page ?? 1;
      return {
        ...samplePage,
        page: page ?? 1,
      };
    }
    throw {
      code: "preview_unimplemented",
      message: `Command '${cmd}' is not mocked in browser preview yet.`,
    };
  });

  return true;
}
