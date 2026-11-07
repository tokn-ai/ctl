// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ComponentBundle, ComponentBundlePhase, ComponentBundleSelectionResult } from "../../lib/types";
import { ComponentBundlePanel } from "./ComponentBundlePanel";

const api = vi.hoisted(() => ({ list: vi.fn(), select: vi.fn() }));
vi.mock("../../lib/tauri", () => ({ getComponentBundles: api.list, selectComponentBundle: api.select }));
const bundle: ComponentBundle = {
  bundle_id: "a".repeat(64), target_triple: "aarch64-apple-darwin", source: "local", app_version: "0.1.0",
  git_revision: "abcdef123456abcdef", dirty: true, compatible: true, included: false,
  local_use: "available", upload_use: "available", local_unavailable_reason: null, upload_unavailable_reason: null,
};
beforeEach(() => {
  vi.resetAllMocks();
  api.list.mockResolvedValue({ bundles: [bundle], errors: [] });
});
afterEach(cleanup);

it("lists only on opening and exposes native-approved uses of each complete build", async () => {
  const page = render(<ComponentBundlePanel visible={false} on_selected={vi.fn()} />);
  expect(api.list).not.toHaveBeenCalled();
  api.list.mockResolvedValue({ bundles: [bundle, { ...bundle, bundle_id: "b".repeat(64), compatible: false, local_use: "unavailable", upload_use: "unavailable", source: "ci" }], errors: ["One stored target failed verification."] });
  page.rerender(<ComponentBundlePanel visible on_selected={vi.fn()} />);
  expect(await screen.findByText("Local build")).toBeTruthy();
  expect(screen.getByText("CI")).toBeTruthy();
  const incompatible = screen.getByText("Incompatible with this app").closest("tr")!;
  expect(within(incompatible).queryByRole("button")).toBeNull();
  expect(screen.getByRole("alert").textContent).toContain("failed verification");
  expect(api.select).not.toHaveBeenCalled();
});

it("verifies before selecting, serializes actions and observes completion while hidden", async () => {
  let complete!: (result: ComponentBundleSelectionResult) => void;
  let progress!: (phase: ComponentBundlePhase) => void;
  api.select.mockImplementation((_request, on_progress) => {
    progress = on_progress;
    return new Promise((resolve) => { complete = resolve; });
  });
  const on_selected = vi.fn();
  const page = render(<ComponentBundlePanel visible on_selected={on_selected} />);
  fireEvent.click(await screen.findByRole("button", { name: "Install" }));
  expect(api.select.mock.calls[0][0]).toEqual({ bundle_id: bundle.bundle_id, target_triple: bundle.target_triple, purpose: "local" });
  expect(screen.getByText("Verifying the complete build…")).toBeTruthy();
  expect((screen.getByRole("button", { name: "Install" }) as HTMLButtonElement).disabled).toBe(true);
  act(() => progress("selecting"));
  expect(screen.getByText("Activating the verified build…")).toBeTruthy();
  page.rerender(<ComponentBundlePanel visible={false} on_selected={on_selected} />);
  api.list.mockResolvedValue({ bundles: [{ ...bundle, local_use: "selected" }, { ...bundle, bundle_id: "b".repeat(64) }], errors: [] });
  await act(async () => complete({ bundle_id: bundle.bundle_id, target_triple: bundle.target_triple, purpose: "local", services_preserved: true }));
  expect(on_selected).toHaveBeenCalledOnce();
  expect(screen.getByText("Active")).toBeTruthy();
  expect(screen.getByText(/Running services and sessions were preserved/)).toBeTruthy();
});

it("keeps the prior selection and reports verification failure without claiming success", async () => {
  api.list.mockResolvedValue({ bundles: [{ ...bundle, local_use: "selected" }, { ...bundle, bundle_id: "b".repeat(64) }], errors: [] });
  api.select.mockRejectedValue({ message: "Bundle checksum changed." });
  const on_selected = vi.fn();
  render(<ComponentBundlePanel visible on_selected={on_selected} />);
  fireEvent.click(await screen.findByRole("button", { name: "Install" }));
  await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("Bundle checksum changed."));
  expect(screen.getByText("Active")).toBeTruthy();
  expect(on_selected).not.toHaveBeenCalled();
  expect(screen.queryByText(/were preserved/)).toBeNull();
});

it("shows included deployment builds without upload selections", async () => {
  api.list.mockResolvedValue({ bundles: [{ ...bundle, included: true, source: "ci", local_use: "unavailable", local_unavailable_reason: "Requires a signed macOS helper package" }], errors: [] });
  render(<ComponentBundlePanel visible on_selected={vi.fn()} />);
  const table = within(await screen.findByRole("table", { name: "Component bundles" }));
  expect(table.getByText("Included with app")).toBeTruthy();
  expect(table.getByRole("columnheader", { name: "Installation" })).toBeTruthy();
  expect(table.queryByRole("columnheader", { name: "Remote uploads" })).toBeNull();
  expect(table.getByText("macOS · Apple silicon").title).toBe(bundle.target_triple);
  expect(table.getByText("Requires a signed macOS helper package")).toBeTruthy();
  expect(table.queryByRole("button", { name: "Install" })).toBeNull();
  expect(api.select).not.toHaveBeenCalled();
});

it("does not report an empty store when the first check failed", async () => {
  api.list.mockRejectedValue(new Error("Could not read included archives."));
  render(<ComponentBundlePanel visible on_selected={vi.fn()} />);
  expect(await screen.findByRole("alert")).toBeTruthy();
  expect(screen.getByText("Bundle availability could not be checked.")).toBeTruthy();
  expect(screen.queryByText(/No included or stored/)).toBeNull();
});
