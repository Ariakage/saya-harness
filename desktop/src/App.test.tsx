import { render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import App from "./App";

const invokeMock = vi.fn();

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

describe("App scaffold", () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it("loads and renders paginated sessions from list_sessions", async () => {
    invokeMock.mockResolvedValueOnce({
      page: 1,
      page_size: 20,
      items: [
        {
          id: "01930000-0000-7000-8000-000000000001",
          workspace: "/workspace/demo",
          work_mode: "work",
          profile: "default",
          title: "Inspect workspace",
          updated_at: 1700000000,
          last_status: "completed",
        },
      ],
    });

    render(<App />);

    await waitFor(() => {
      expect(screen.getByText("Inspect workspace")).toBeTruthy();
    });
    expect(invokeMock).toHaveBeenCalledWith("list_sessions", { page: 1 });
  });
});
