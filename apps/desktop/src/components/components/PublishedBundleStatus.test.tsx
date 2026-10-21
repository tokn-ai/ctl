// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { checkComponentUpdates } from "../../lib/tauri";
import type { ComponentUpdateCatalog } from "../../lib/types";
import { PublishedBundleStatus } from "./PublishedBundleStatus";

vi.mock("../../lib/tauri", () => ({ checkComponentUpdates: vi.fn() }));
const available: ComponentUpdateCatalog = {
  default_channel: "dev", local_development: false, error: null,
  catalog: { channel: "dev", app_version: "0.1.0", bundle_id: "0.1.0-dev.abcdef123456", git_revision: "abcdef123456", targets: [{ target_triple: "x86_64-unknown-linux-musl", compatible: true, components: {} }] },
};
beforeEach(() => { vi.resetAllMocks(); vi.mocked(checkComponentUpdates).mockResolvedValue(available); });
afterEach(cleanup);

it("checks only when visible and refreshes publication metadata explicitly", async () => {
  const on_update = vi.fn();
  const page = render(<PublishedBundleStatus visible={false} on_update={on_update} />);
  expect(checkComponentUpdates).not.toHaveBeenCalled();
  page.rerender(<PublishedBundleStatus visible on_update={on_update} />);
  expect(await screen.findByText("0.1.0-dev.abcdef123456")).toBeTruthy();
  expect(checkComponentUpdates).toHaveBeenCalledWith(undefined, false);
  fireEvent.click(screen.getByRole("button", { name: "Check for bundle updates" }));
  await waitFor(() => expect(checkComponentUpdates).toHaveBeenLastCalledWith(undefined, true));
  fireEvent.click(await screen.findByRole("button", { name: "Install published bundle…" }));
  expect(on_update).toHaveBeenCalledWith("dev");
});

it("keeps a failed check distinct from an available publication", async () => {
  vi.mocked(checkComponentUpdates).mockResolvedValue({ ...available, catalog: null, error: "Publication is unavailable." });
  render(<PublishedBundleStatus on_update={vi.fn()} />);
  expect((await screen.findByRole("alert")).textContent).toContain("Publication is unavailable.");
  expect(screen.queryByRole("button", { name: "Install published bundle…" })).toBeNull();
  expect(screen.queryByText("0.1.0-dev.abcdef123456")).toBeNull();
});

it("ignores an old channel response after the channel changes", async () => {
  let finish!: (value: ComponentUpdateCatalog) => void;
  vi.mocked(checkComponentUpdates).mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
  const stable: ComponentUpdateCatalog = { ...available, catalog: { ...available.catalog!, channel: "stable", bundle_id: "0.2.0", app_version: "0.2.0" } };
  vi.mocked(checkComponentUpdates).mockResolvedValueOnce(stable);
  const page = render(<PublishedBundleStatus channel="dev" />);
  page.rerender(<PublishedBundleStatus channel="stable" />);
  expect(await screen.findByText("0.2.0")).toBeTruthy();
  await act(async () => finish(available));
  expect(screen.queryByText("0.1.0-dev.abcdef123456")).toBeNull();
  expect(screen.getByText("0.2.0")).toBeTruthy();
});

it("explains local development and reports published incompatibility", async () => {
  vi.mocked(checkComponentUpdates).mockResolvedValue({ ...available, local_development: true, catalog: { ...available.catalog!, targets: [{ ...available.catalog!.targets[0], compatible: false }] } });
  render(<PublishedBundleStatus />);
  expect(await screen.findByText(/Linux x64 uploads build this working tree locally/)).toBeTruthy();
  expect(screen.getByText(/Some published targets require a newer client/)).toBeTruthy();
});
