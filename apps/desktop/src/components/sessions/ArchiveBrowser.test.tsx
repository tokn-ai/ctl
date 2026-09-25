// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { ArchiveBrowser } from "./ArchiveBrowser";
const mocks = vi.hoisted(() => ({ request: vi.fn(), restore: vi.fn().mockResolvedValue(undefined), dispose: vi.fn() }));
vi.mock("../../lib/tauri", () => ({ sessionArchive: mocks.request }));
vi.mock("../../features/terminal/XtermRenderer", () => ({ XtermRenderer: class {
  restoreCheckpoint = mocks.restore;
  dispose = mocks.dispose;
} }));
beforeEach(() => { HTMLDialogElement.prototype.showModal = function () { this.setAttribute("open", ""); }; });
afterEach(() => { cleanup(); vi.clearAllMocks(); });
it("lists retention dates and opens an archived checkpoint without attaching to a PTY", async () => {
  mocks.request.mockResolvedValueOnce({ kind: "list", archives: [{ session_id: "root", name: "old shell", archived_at_ms: 1000, expires_at_ms: 604801000, terminals: [{ terminal_id: "pane", reason: "exited", exit_code: 7 }] }] })
    .mockResolvedValueOnce({ kind: "terminal", checkpoint: { terminal_size: { columns: 80, rows: 24 }, payload_base64: btoa("final output"), input_prefix_base64: "", sequence: "12" }, history: { lines: ["previous output"] } });
  render(<ArchiveBrowser targets={[{ kind: "local" }]} on_close={vi.fn()} />);
  fireEvent.click(await screen.findByRole("button", { name: "Pane 1: exited (7)" }));
  await waitFor(() => expect(mocks.restore).toHaveBeenCalledOnce());
  const [size, history, payload, prefix, sequence] = mocks.restore.mock.calls[0];
  expect(size).toEqual({ columns: 80, rows: 24 });
  expect(history).toEqual(["previous output"]);
  expect(Array.from(payload)).toEqual(Array.from(new TextEncoder().encode("final output")));
  expect(Array.from(prefix)).toEqual([]);
  expect(sequence).toBe("12");
  expect(mocks.request).toHaveBeenLastCalledWith({ kind: "local" }, { kind: "read", session_id: "root", terminal_id: "pane" });
  expect(screen.getByText(/Retained until/)).toBeTruthy();
});
it("shows archive lookup failures without pretending the inventory is empty", async () => {
  mocks.request.mockRejectedValue(new Error("Host unavailable"));
  render(<ArchiveBrowser targets={[{ kind: "local" }]} on_close={vi.fn()} />);
  expect((await screen.findByRole("alert")).textContent).toContain("Host unavailable");
  expect(screen.queryByText("No retained archives on this host.")).toBeNull();
});
