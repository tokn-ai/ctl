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

it("gives every stored build its own heading, including retained builds with the same version", () => {
  const stable = "stable-v0.10.0-aaaaaaa";
  const nightly = "nightly-20261123T080000Z-v0.10.0-aaaaaaa";
  const local = "dev-20261122T180000Z-v0.10.0-aaaaaaa-local";
  const pullRequest = "dev-20261123T100000Z-v0.10.0-aaaaaaa-pr-12";
  const collision = local + "-123456789abc";
  const previous = "stable-v0.9.0-aaaaaaa";
  const rc2 = "release-undated-v0.10.0-rc.2-aaaaaaa";
  const rc10 = "release-undated-v0.10.0-rc.10-aaaaaaa";
  const imported = "provided-undated-vunknown-aaaaaaa";
  const entries = [
    [previous, "0.9.0", "stable"],
    [local, "0.10.0", "local"], [stable, "0.10.0", "stable"], [collision, "0.10.0", "local"],
    [nightly, "0.10.0", "nightly"], [rc2, "0.10.0-rc.2", "release"],
    [rc10, "0.10.0-rc.10", "release"], [imported, "snapshot", "provided"],
    [pullRequest, "0.10.0", "pr-12"],
  ];
  const builds = entries.map(([build_label, version, channel]) => ({
    build_label, channels: [channel], build: { ...build, version }, current: build_label !== local,
  }));
  render(<RuntimeTargets inventory={{ ...inventory, installed_targets: [{ target_triple: linux, builds }] }}
    loading={false} error={null} on_refresh={on_refresh} />);
  const section = within(screen.getByRole("region", { name: "Runtime targets on this computer" }));
  expect(section.getAllByRole("heading", { level: 3 }).map((header) => header.textContent)).toEqual([
    pullRequest, nightly, local, collision, stable, rc10, rc2, previous, imported,
  ]);
  const retained = within(section.getByRole("listitem", { name: local }));
  expect(retained.getByText("Retained")).toBeTruthy();
  expect(retained.getByText(linux)).toBeTruthy();
  expect(within(section.getByRole("listitem", { name: imported })).getByText("snapshot")).toBeTruthy();
  expect(screen.queryByRole("heading", { name: "0.10.0" })).toBeNull();
});

it("lists all targets for the same stored label with their own channels, retention and build identities", () => {
  const label = "dev-20261123T183000Z-v0.1.5-aaaaaaa-local";
  const other = { ...build, source_revision: "a".repeat(9) + "c".repeat(31), source_fingerprint: "b".repeat(9) + "c".repeat(55) };
  render(<RuntimeTargets loading={false} error={null} on_refresh={on_refresh} inventory={{ ...inventory, installed_targets: [
    { target_triple: windows, builds: [{ build_label: label, build, channels: ["local"] }] },
    { target_triple: linux, builds: [{ build_label: label, build: other, channels: ["nightly", "stable"], current: false }] },
  ] }} />);
  const group = within(screen.getByRole("listitem", { name: label }));
  expect(group.getByRole("heading", { level: 3, name: label })).toBeTruthy();
  const targets = within(group.getByRole("list")).getAllByRole("listitem");
  expect(targets).toHaveLength(2);
  const [linuxEntry, windowsEntry] = targets.map((target) => within(target));
  expect(windowsEntry.getByText(windows)).toBeTruthy();
  expect(linuxEntry.getByText(linux)).toBeTruthy();
  expect(windowsEntry.getByText("local")).toBeTruthy();
  expect(windowsEntry.queryByText("Retained")).toBeNull();
  expect(linuxEntry.getByText("Retained")).toBeTruthy();
  expect(linuxEntry.getByText("nightly")).toBeTruthy();
  expect(linuxEntry.getByText("stable")).toBeTruthy();
  expect(windowsEntry.getByTitle(build.source_revision).textContent).not.toBe(linuxEntry.getByTitle(other.source_revision).textContent);
  expect(windowsEntry.getByTitle(build.source_fingerprint).textContent).not.toBe(linuxEntry.getByTitle(other.source_fingerprint).textContent);
});

it("keeps legacy records without store labels visible as separate target builds", () => {
  const older = { ...build, source_fingerprint: "c".repeat(64) };
  const firstId = "d".repeat(64);
  const secondId = "d".repeat(12) + "e".repeat(52);
  render(<RuntimeTargets loading={false} error={null} on_refresh={on_refresh} inventory={{ ...inventory, installed_targets: [
    { target_triple: windows, builds: [
      { build, build_label: null, bundle_id: firstId, channels: ["local"] },
      { build, build_label: null, bundle_id: secondId, channels: ["local"] },
    ] },
    { target_triple: linux, builds: [{ build, channels: ["stable"] }, { build: older, channels: ["nightly"], current: false }] },
  ] }} />);
  expect(screen.getAllByRole("heading", { level: 3 })).toHaveLength(4);
  expect(screen.getByTitle(firstId).textContent).toBe("build " + firstId.slice(0, 13));
  expect(screen.getByTitle(secondId).textContent).toBe("build " + secondId.slice(0, 13));
  const retained = within(screen.getByRole("listitem", { name: /0\.1\.5 build cccccccccccc/ }));
  expect(retained.getByText("Retained")).toBeTruthy();
  expect(retained.getByText("aaaaaaaa")).toBeTruthy();
  expect(retained.getByText(linux)).toBeTruthy();
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
