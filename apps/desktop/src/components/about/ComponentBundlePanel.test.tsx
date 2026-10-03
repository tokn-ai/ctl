// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ComponentBundle, ComponentBundlePhase, ComponentBundleSelectionResult } from "../../lib/types";
import { ComponentBundlePanel } from "./ComponentBundlePanel";

const api = vi.hoisted(() => ({ list: vi.fn(), select: vi.fn() }));
vi.mock("../../lib/tauri", () => ({ getComponentBundles: api.list, selectComponentBundle: api.select }));
const bundle: ComponentBundle = {
  bundle_id: "a".repeat(64), target_triple: "aarch64-apple-darwin", source: "local", app_version: "0.1.0",
  git_revision: "abcdef123456abcdef", dirty: true, compatible: true, local_use: "available", upload_use: "available",
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
  fireEvent.click(await screen.findByRole("button", { name: "Use for uploads" }));
  expect(api.select.mock.calls[0][0]).toEqual({ bundle_id: bundle.bundle_id, target_triple: bundle.target_triple, purpose: "upload" });
  expect(screen.getByText("Verifying the complete build…")).toBeTruthy();
  expect((screen.getByRole("button", { name: "Use locally" }) as HTMLButtonElement).disabled).toBe(true);
  act(() => progress("selecting"));
  expect(screen.getByText("Selecting the verified bundle…")).toBeTruthy();
  page.rerender(<ComponentBundlePanel visible={false} on_selected={on_selected} />);
  api.list.mockResolvedValue({ bundles: [{ ...bundle, upload_use: "selected" }], errors: [] });
  await act(async () => complete({ bundle_id: bundle.bundle_id, target_triple: bundle.target_triple, purpose: "upload", services_preserved: true }));
  expect(on_selected).toHaveBeenCalledOnce();
  expect(screen.getByText("Selected for uploads")).toBeTruthy();
  expect(screen.getByText(/Running services and sessions were preserved/)).toBeTruthy();
});

it("keeps the prior selection and reports verification failure without claiming success", async () => {
  api.list.mockResolvedValue({ bundles: [{ ...bundle, local_use: "selected" }], errors: [] });
  api.select.mockRejectedValue({ message: "Bundle checksum changed." });
  const on_selected = vi.fn();
  render(<ComponentBundlePanel visible on_selected={on_selected} />);
  fireEvent.click(await screen.findByRole("button", { name: "Use for uploads" }));
  await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("Bundle checksum changed."));
  expect(screen.getByText("Selected locally")).toBeTruthy();
  expect(on_selected).not.toHaveBeenCalled();
  expect(screen.queryByText(/were preserved/)).toBeNull();
});
