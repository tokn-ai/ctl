// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { ArchiveBrowser } from "./ArchiveBrowser";
const mocks = vi.hoisted(() => ({ request: vi.fn() }));
vi.mock("../../lib/tauri", () => ({ sessionArchive: mocks.request }));
beforeEach(() => { HTMLDialogElement.prototype.showModal = function () { this.setAttribute("open", ""); }; });
afterEach(() => { cleanup(); vi.clearAllMocks(); });
it("reads local retained output even without any configured or reachable hosts", async () => {
  mocks.request.mockResolvedValue({ kind: "list", archives: [{ session_id: "root", host_key: "removed-host", name: "old shell", archived_at_ms: 1000, expires_at_ms: 604801000, terminals: [{ terminal_id: "pane", reason: "Exited (code 7)", lines: ["previous output", "final output"] }] }] });
  render(<ArchiveBrowser targets={[]} on_close={vi.fn()} />);
  fireEvent.click(await screen.findByRole("button", { name: /old shell/ }));
  expect(screen.getByLabelText("Archived terminal output").textContent).toBe("previous output\nfinal output");
  expect(mocks.request).toHaveBeenCalledExactlyOnceWith({ kind: "list" });
  expect(screen.getByText(/until deleted/)).toBeTruthy();
  mocks.request.mockResolvedValue({ kind: "deleted" });
  fireEvent.click(screen.getByRole("button", { name: "Delete archive" }));
  await waitFor(() => expect(screen.queryByRole("button", { name: /old shell/ })).toBeNull());
  expect(mocks.request).toHaveBeenLastCalledWith({ kind: "delete", host_key: "removed-host", session_id: "root" });
});
it("shows local storage failures without pretending the inventory is empty", async () => {
  mocks.request.mockRejectedValue(new Error("Storage unavailable"));
  render(<ArchiveBrowser targets={[]} on_close={vi.fn()} />);
  expect((await screen.findByRole("alert")).textContent).toContain("Storage unavailable");
  expect(screen.queryByText("No retained archives on this device.")).toBeNull();
});

it("loads durable history in pages and appends the final current screen", async () => {
  mocks.request.mockResolvedValueOnce({ kind: "list", archives: [{ session_id: "root", host_key: "local", name: "durable", archived_at_ms: 1, expires_at_ms: 0, terminals: [{ terminal_id: "pane", reason: "Closed", lines: [] }] }] })
    .mockResolvedValueOnce({ kind: "output", lines: ["first history"], next_offset: "42" })
    .mockResolvedValueOnce({ kind: "output", lines: ["later history", "current screen"], next_offset: null });
  render(<ArchiveBrowser targets={[]} on_close={vi.fn()} />);
  fireEvent.click(await screen.findByRole("button", { name: /durable/ }));
  fireEvent.click(await screen.findByRole("button", { name: "Load more output" }));
  await waitFor(() => expect(screen.getByLabelText("Archived terminal output").textContent).toBe("first history\nlater history\ncurrent screen"));
  expect(mocks.request).toHaveBeenLastCalledWith({ kind: "read", host_key: "local", session_id: "root", terminal_id: "pane", offset: "42" });
  expect(screen.queryByRole("button", { name: "Load more output" })).toBeNull();
});
