// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { cancelComponentUpdate, checkComponentUpdates, fetchAboutRuntimeTarget } from "../../lib/tauri";
import type { AboutRuntimeStatus, BundleChannel, ComponentUpdateCatalog } from "../../lib/types";
import { RuntimeTargets } from "./RuntimeTargets";

vi.mock("../../lib/tauri", () => ({
  cancelComponentUpdate: vi.fn(), checkComponentUpdates: vi.fn(), fetchAboutRuntimeTarget: vi.fn(),
}));
const windows = "x86_64-pc-windows-msvc";
const linux = "aarch64-unknown-linux-musl";
const build = { version: "0.1.5", source_revision: "a".repeat(40), source_fingerprint: "b".repeat(64), dirty: false };
const inventory: AboutRuntimeStatus = {
  active: null, build_label: null, origin: null, missing: false,
  installed_targets: [{ target_triple: linux, current_publications: { nightly: "nightly.publication" }, builds: [{ channels: ["nightly", "stable"], build }] }],
};
function catalog(channel: BundleChannel = "nightly"): ComponentUpdateCatalog {
  return {
    default_channel: channel, local_development: false, error: null,
    catalog: { channel, app_version: build.version, git_revision: build.source_revision, bundle_id: "fixture-v0.1.5", publication_id: channel + ".publication",
      targets: [linux, windows, "aarch64-apple-darwin"].map((target_triple) => ({
        target_triple, compatible: target_triple !== "aarch64-apple-darwin", components: { "ctl-agent": { build, protocols: [] } },
      })) },
  };
}
const on_refresh = vi.fn();
function show() {
  return render(<RuntimeTargets inventory={inventory} default_channel="nightly" loading={false} error={null} on_refresh={on_refresh} />);
}
beforeEach(() => {
  vi.resetAllMocks();
  vi.mocked(checkComponentUpdates).mockImplementation(async (channel) => catalog(channel));
  vi.mocked(fetchAboutRuntimeTarget).mockResolvedValue(undefined);
  vi.mocked(cancelComponentUpdate).mockResolvedValue(undefined);
});
afterEach(cleanup);

it("groups exact versions newest first, including prereleases and retained imported builds", () => {
  const versions = ["0.9.0", "local-snapshot", "0.10.0-rc.2", "0.10.0", "0.10.0-rc.10"];
  const installed_targets = [linux, windows].map((target_triple) => ({
    target_triple, builds: versions.map((version) => ({
      channels: ["stable"], build: { ...build, version }, current: version !== "local-snapshot",
    })),
  }));
  render(<RuntimeTargets inventory={{ ...inventory, installed_targets }} loading={false} error={null} on_refresh={on_refresh} />);
  const headings = screen.getAllByRole("heading", { level: 3 });
  expect(headings.map((item) => item.textContent)).toEqual(["0.10.0", "0.10.0-rc.10", "0.10.0-rc.2", "0.9.0", "local-snapshot"]);
  for (const heading of headings) {
    const version = within(heading.closest("li")!);
    expect(version.getByText(linux)).toBeTruthy();
    expect(version.getByText(windows)).toBeTruthy();
    expect(version.getAllByText("stable")).toHaveLength(1);
    expect(!!version.queryByText("Retained")).toBe(heading.textContent === "local-snapshot");
  }
});

it("retains per-target channels and distinguishes rebuilds from the same commit", () => {
  const other = { ...build, source_fingerprint: build.source_fingerprint.slice(0, 8) + "c".repeat(56) };
  render(<RuntimeTargets loading={false} error={null} on_refresh={on_refresh} inventory={{ ...inventory, installed_targets: [
    { target_triple: windows, builds: [{ build, channels: ["local"] }, { build, channels: ["local"] }] },
    { target_triple: linux, builds: [
      { build, channels: ["nightly"], current: false },
      { build: other, channels: ["nightly"], current: false },
    ] },
  ] }} />);
  expect(screen.getAllByRole("heading", { level: 3 })).toHaveLength(1);
  const platform = screen.getByText(windows).parentElement!;
  expect(within(platform).getByText("local")).toBeTruthy();
  expect(within(platform).queryByText("nightly")).toBeNull();
  expect(within(platform).queryByText("Retained")).toBeNull();
  const retained = screen.getByText("Retained").parentElement!;
  expect(within(retained).getByText(linux)).toBeTruthy();
  expect(within(retained).getByText("nightly")).toBeTruthy();
  expect(screen.getByText("1 earlier build")).toBeTruthy();
  expect(screen.getAllByTitle(build.source_revision).map((sha) => sha.textContent)).toEqual(["aaaaaaaa", "aaaaaaaa"]);
  expect(screen.getByTitle(build.source_fingerprint).textContent).not.toBe(screen.getByTitle(other.source_fingerprint).textContent);
});

it("lists installed builds offline and fetches only a compatible published target on the chosen channel", async () => {
  show();
  expect(screen.getByText(linux)).toBeTruthy();
  expect(screen.getByText("aaaaaaaa")).toBeTruthy();
  expect(checkComponentUpdates).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Add target" }));
  const select = await screen.findByRole<HTMLSelectElement>("combobox", { name: "Target" });
  await waitFor(() => expect(Array.from(select.options, (o) => o.value)).toEqual([windows]));
  expect(fetchAboutRuntimeTarget).not.toHaveBeenCalled();
  fireEvent.change(screen.getByRole("combobox", { name: "Channel" }), { target: { value: "stable" } });
  await waitFor(() => expect(Array.from(select.options, (o) => o.value)).toEqual([linux, windows]));
  fireEvent.change(select, { target: { value: windows } });
  fireEvent.click(screen.getByRole("button", { name: "Fetch target" }));
  await screen.findByText("Stored Windows · x64 on this computer.");
  expect(fetchAboutRuntimeTarget).toHaveBeenCalledWith(windows, "stable", expect.any(String));
  expect(on_refresh).toHaveBeenCalledTimes(1);
});

it("stops the active fetch by attempt ID and refreshes the inventory after interruption", async () => {
  let reject!: (error: Error) => void;
  vi.mocked(fetchAboutRuntimeTarget).mockImplementation(() => new Promise((_, fail) => { reject = fail; }));
  show();
  fireEvent.click(screen.getByRole("button", { name: "Add target" }));
  await screen.findByRole("option", { name: "Windows · x64" });
  fireEvent.click(screen.getByRole("button", { name: "Fetch target" }));
  fireEvent.click(screen.getByRole("button", { name: "Stop update" }));
  expect(cancelComponentUpdate).toHaveBeenCalledWith(vi.mocked(fetchAboutRuntimeTarget).mock.calls[0][2]);
  await act(async () => reject(Object.assign(new Error("Fetch interrupted."), { code: "runtime_operation_cancelled" })));
  expect(await screen.findByText("Cancelled")).toBeTruthy();
  expect(screen.queryByRole("alert")).toBeNull();
  expect(on_refresh).toHaveBeenCalled();
});

it("allows fetching a republished build with the same version and revision", async () => {
  const republished = catalog();
  republished.catalog!.publication_id = "nightly.rebuilt";
  vi.mocked(checkComponentUpdates).mockResolvedValue(republished);
  show();
  fireEvent.click(screen.getByRole("button", { name: "Add target" }));
  expect(await screen.findByRole("option", { name: "Linux · ARM64 · musl" })).toBeTruthy();
});

it("keeps a catalog error away from the installed list and permits retry", async () => {
  vi.mocked(checkComponentUpdates).mockResolvedValueOnce({ ...catalog(), catalog: null, error: "Offline" });
  show();
  fireEvent.click(screen.getByRole("button", { name: "Add target" }));
  expect(await screen.findByText("Offline")).toBeTruthy();
  expect(screen.getByText(linux)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
  await screen.findByRole("option", { name: "Windows · x64" });
  expect(checkComponentUpdates).toHaveBeenLastCalledWith("nightly", true);
});
