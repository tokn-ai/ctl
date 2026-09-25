// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { ArchiveBrowser } from "./ArchiveBrowser";
const mocks = vi.hoisted(() => ({ request: vi.fn() }));
vi.mock("../../lib/tauri", () => ({ sessionArchive: mocks.request }));
beforeEach(() => { HTMLDialogElement.prototype.showModal = function () { this.setAttribute("open", ""); }; });
afterEach(() => { cleanup(); vi.clearAllMocks(); });
it("reads local retained output even without any configured or reachable hosts", async () => {
  mocks.request.mockResolvedValue({ kind: "list", archives: [{ session_id: "root", host_key: "removed-host", name: "old shell", archived_at_ms: 1000, expires_at_ms: 604801000, terminals: [{ terminal_id: "pane", reason: "Exited (code 7)", lines: ["previous output", "final output"] }] }] });
  render(<ArchiveBrowser targets={[]} on_close={vi.fn()} />);
  fireEvent.click(await screen.findByRole("button", { name: "Pane 1: Exited (code 7)" }));
  expect(screen.getByLabelText("Archived terminal output").textContent).toBe("previous output\nfinal output");
  expect(mocks.request).toHaveBeenCalledExactlyOnceWith({ kind: "list" });
  expect(screen.getByText(/Retained until/)).toBeTruthy();
});
it("shows local storage failures without pretending the inventory is empty", async () => {
  mocks.request.mockRejectedValue(new Error("Storage unavailable"));
  render(<ArchiveBrowser targets={[]} on_close={vi.fn()} />);
  expect((await screen.findByRole("alert")).textContent).toContain("Storage unavailable");
  expect(screen.queryByText("No retained archives on this device.")).toBeNull();
});
