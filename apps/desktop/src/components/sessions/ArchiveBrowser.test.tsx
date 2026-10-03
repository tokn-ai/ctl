// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { ArchiveBrowser } from "./ArchiveBrowser";
import type { ArchiveResponse, SessionArchive } from "../../lib/types";
const mocks = vi.hoisted(() => ({ request: vi.fn() }));
vi.mock("../../lib/tauri", () => ({ sessionArchive: mocks.request }));
function retainedArchive(lines: string[]): SessionArchive {
  return { session_id: "root", host_key: "local", name: "retained shell", archived_at_ms: 1, expires_at_ms: 0, terminals: [{ terminal_id: "pane", reason: "Closed", lines }] };
}
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
    .mockResolvedValueOnce({ kind: "output", lines: ["first history"], next_offset: "42", history_gap: false })
    .mockResolvedValueOnce({ kind: "output", lines: ["later history", "current screen"], next_offset: null, history_gap: false });
  render(<ArchiveBrowser targets={[]} on_close={vi.fn()} />);
  fireEvent.click(await screen.findByRole("button", { name: /durable/ }));
  fireEvent.click(await screen.findByRole("button", { name: "Load more output" }));
  await waitFor(() => expect(screen.getByLabelText("Archived terminal output").textContent).toBe("first history\nlater history\ncurrent screen"));
  expect(mocks.request).toHaveBeenLastCalledWith({ kind: "read", host_key: "local", session_id: "root", terminal_id: "pane", offset: "42" });
  expect(screen.queryByRole("button", { name: "Load more output" })).toBeNull();
});

it("shows a retained-history gap across archive pages and clears it for another archive", async () => {
  const other = { ...retainedArchive(["complete inline output"]), session_id: "other", name: "complete shell" };
  mocks.request.mockResolvedValueOnce({ kind: "list", archives: [retainedArchive([]), other] })
    .mockResolvedValueOnce({ kind: "output", lines: ["retained tail"], next_offset: "42", history_gap: true })
    .mockResolvedValueOnce({ kind: "output", lines: ["final screen"], next_offset: null, history_gap: false });
  render(<ArchiveBrowser targets={[]} on_close={vi.fn()} />);
  fireEvent.click(await screen.findByRole("button", { name: /retained shell/ }));
  expect(await screen.findByText(/Earlier output is unavailable/)).toBeTruthy();
  fireEvent.click(await screen.findByRole("button", { name: "Load more output" }));
  await waitFor(() => expect(screen.getByLabelText("Archived terminal output").textContent).toBe("retained tail\nfinal screen"));
  expect(screen.getByText(/Earlier output is unavailable/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: /complete shell/ }));
  expect(screen.queryByText(/Earlier output is unavailable/)).toBeNull();
});

it("shows a retained-history gap for inline archived output", async () => {
  const archive = retainedArchive(["retained output"]);
  archive.terminals[0].history_gap = true;
  mocks.request.mockResolvedValueOnce({ kind: "list", archives: [archive] });
  render(<ArchiveBrowser targets={[]} on_close={vi.fn()} />);
  fireEvent.click(await screen.findByRole("button", { name: /retained shell/ }));
  expect(screen.getByText(/Earlier output is unavailable/)).toBeTruthy();
  expect(screen.getByLabelText("Archived terminal output").textContent).toBe("retained output");
  expect(mocks.request).toHaveBeenCalledExactlyOnceWith({ kind: "list" });
});

it.each([{ lines: [""] }, { lines: ["", " \t", ""] }])("shows an empty message for inline output with only whitespace: $lines", async ({ lines }) => {
  mocks.request.mockResolvedValueOnce({ kind: "list", archives: [retainedArchive(lines)] });
  render(<ArchiveBrowser targets={[]} on_close={vi.fn()} />);
  fireEvent.click(await screen.findByRole("button", { name: /retained shell/ }));
  expect(screen.getByLabelText("Archived terminal output").textContent).toBe("No readable retained output is available.");
  expect(mocks.request).toHaveBeenCalledExactlyOnceWith({ kind: "list" });
});

it("preserves blank lines and spaces around readable inline output", async () => {
  const lines = ["", " \t", "  retained output  ", "", " "];
  mocks.request.mockResolvedValueOnce({ kind: "list", archives: [retainedArchive(lines)] });
  render(<ArchiveBrowser targets={[]} on_close={vi.fn()} />);
  fireEvent.click(await screen.findByRole("button", { name: /retained shell/ }));
  expect(screen.getByLabelText("Archived terminal output").textContent).toBe(lines.join("\n"));
});

it("shows loading until a blank-only durable read has completed", async () => {
  let resolve_read!: (response: ArchiveResponse) => void;
  const pending_read = new Promise<ArchiveResponse>((resolve) => { resolve_read = resolve; });
  mocks.request.mockResolvedValueOnce({ kind: "list", archives: [retainedArchive([])] })
    .mockReturnValueOnce(pending_read);
  render(<ArchiveBrowser targets={[]} on_close={vi.fn()} />);
  fireEvent.click(await screen.findByRole("button", { name: /retained shell/ }));
  expect(screen.getByLabelText("Archived terminal output").textContent).toBe("Loading retained output…");
  await act(async () => { resolve_read({ kind: "output", lines: ["", " \t", ""], next_offset: null, history_gap: false }); });
  expect(screen.getByLabelText("Archived terminal output").textContent).toBe("No readable retained output is available.");
  expect(screen.queryByRole("button", { name: "Load more output" })).toBeNull();
});

it.each([
  { final_lines: ["retained output", ""], expected: "\n \t\n\nretained output\n" },
  { final_lines: ["", " \t"], expected: "No readable retained output is available." },
])("distinguishes blank loaded pages from a completed archive: $final_lines", async ({ final_lines, expected }) => {
  let resolve_read!: (response: ArchiveResponse) => void;
  const pending_read = new Promise<ArchiveResponse>((resolve) => { resolve_read = resolve; });
  mocks.request.mockResolvedValueOnce({ kind: "list", archives: [retainedArchive([])] })
    .mockResolvedValueOnce({ kind: "output", lines: ["", " \t", ""], next_offset: "42", history_gap: false })
    .mockReturnValueOnce(pending_read);
  render(<ArchiveBrowser targets={[]} on_close={vi.fn()} />);
  fireEvent.click(await screen.findByRole("button", { name: /retained shell/ }));
  const load_more = await screen.findByRole("button", { name: "Load more output" });
  expect(screen.getByLabelText("Archived terminal output").textContent).toBe("No readable text in the loaded output.");
  fireEvent.click(load_more);
  expect(screen.getByLabelText("Archived terminal output").textContent).toBe("Loading retained output…");
  expect(screen.getByRole("button", { name: "Loading…" }).hasAttribute("disabled")).toBe(true);
  await act(async () => { resolve_read({ kind: "output", lines: final_lines, next_offset: null, history_gap: false }); });
  expect(screen.getByLabelText("Archived terminal output").textContent).toBe(expected);
  expect(screen.queryByRole("button", { name: "Load more output" })).toBeNull();
});
