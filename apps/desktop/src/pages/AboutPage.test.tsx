// @vitest-environment jsdom
import type { ReactElement } from "react";
import { NotificationProvider } from "../features/notifications/NotificationContext";
import { LocaleProvider } from "../features/i18n/LocaleContext";
import { formatText } from "../features/i18n/messages";
import { NotificationStore } from "../features/notifications/NotificationStore";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ComponentVersionInfo, ComponentVersionsSnapshot, ComponentActionPreflight } from "../lib/types";
import { AboutPage } from "./AboutPage";

const api = vi.hoisted(() => ({
  desktop: vi.fn(), updater: vi.fn(), fetch: vi.fn(), runtime: vi.fn(), sync: vi.fn(), versions: vi.fn(), preflight: vi.fn(), restart: vi.fn(),
  bundles: vi.fn(), select: vi.fn(), published: vi.fn(), update: vi.fn(), cancel_update: vi.fn(),
}));
vi.mock("../lib/tauri", () => ({
  getDesktopBuild: api.desktop,
  getUpdaterVersion: api.updater,
  fetchAboutRuntimeTarget: api.fetch,
  getAboutRuntimeStatus: api.runtime,
  syncAboutRuntime: api.sync,
  getComponentVersions: api.versions,
  preflightComponentAction: api.preflight,
  executeComponentAction: api.restart,
  getComponentBundles: api.bundles,
  selectComponentBundle: api.select,
  checkComponentUpdates: api.published,
  componentUpdateCheckoutAvailable: vi.fn(async () => false),
  updateComponents: api.update,
  cancelComponentUpdate: api.cancel_update,
}));

const current: ComponentVersionInfo = { version: "0.1.0", source_revision: "abcdef123456", source_fingerprint: "current", dirty: false, protocols: [{ name: "ctld", build: 12, version: "1.0.12", supported_versions: ["1.0.12"] }] };
const snapshot: ComponentVersionsSnapshot = { components: [
  { component_id: "app", component: "ctmux", label: "ctmux", location: "local", host_id: null, observation: "bundled", status: "current", running: current, available: current, restart_supported: false, action: null, detail: null, error: null },
  { component_id: "owner-1", component: "ctld", label: "ctld (SSH)", location: "local", host_id: null, observation: "running", status: "different_build", restart_required: true, running: { ...current, source_revision: "112233445566" }, available: current, restart_supported: true, action: "restart", detail: "SSH and VPN broker", error: null },
  { component_id: "remote-1", component: "ctl_agent", label: "ctl-agent — Development", location: "remote", host_id: "dev", host_name: "Development", observation: "last_observed", status: "unknown", running: { ...current, source_revision: null, source_fingerprint: null }, available: current, restart_supported: false, action: null, detail: null, error: null },
] };
const preflight: ComponentActionPreflight = { action_token: "native-owner-token", component_id: "owner-1", component: "ctld", location: "local", host_id: null, action: "restart", label: "ctld (SSH)", running: snapshot.components[1].running, available: current, impact: { ssh_connections: null, port_forwards: null, vpn_connections: 2, terminal_sessions: null, description: "Stops 2 managed VPN connections and may interrupt SSH connections and port forwards." } };
function openDetails(toggle: HTMLElement) {
  fireEvent.click(toggle);
  return within(document.getElementById(toggle.getAttribute("aria-controls")!)!);
}

async function servicesReady() {
  await screen.findByRole("table", { name: "This computer component versions" });
}
async function selectMachine(name: string) {
  fireEvent.click(await screen.findByRole("button", { name }));
  return within(await screen.findByRole("region", { name }));
}

const saved_target = { kind: "ssh" as const, host_id: "dev", host_name: "Development", destination: "dev" };
const props = () => ({ visible: true, remote_targets: [saved_target], on_close: vi.fn(), on_restarted: vi.fn(), on_dialog_change: vi.fn() });

let notifications: NotificationStore;

function firstNotification() {
  const entry = notifications.snapshot().entries[0];
  return entry && { severity: entry.severity, source: formatText("en", entry.source), message: formatText("en", entry.message) };
}
function renderPage(ui: ReactElement) {
  return render(ui, { wrapper: ({ children }) => <NotificationProvider store={notifications}>{children}</NotificationProvider> });
}

beforeEach(() => {
  notifications = new NotificationStore();
  vi.resetAllMocks();
  api.desktop.mockResolvedValue({ version: "0.1.5", channel: "local", source_revision: "12121212".repeat(5) });
  api.updater.mockResolvedValue("0.1.3");
  api.runtime.mockResolvedValue({ active: null, build_label: null, missing: false, origin: null });
  api.sync.mockResolvedValue({ state: "installed" });
  api.bundles.mockResolvedValue({ bundles: [], errors: [] });
  api.published.mockResolvedValue({ default_channel: "nightly", local_development: false, catalog: null, error: null });
  api.versions.mockResolvedValue(structuredClone(snapshot));
  api.preflight.mockResolvedValue(structuredClone(preflight));
  api.restart.mockResolvedValue({ component_id: "owner-1", running: current });
  api.cancel_update.mockResolvedValue(undefined);
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); localStorage.removeItem("ctmux.desktop.language"); });

describe("About page", () => {
  it("fetches a target locally while viewing a host and keeps that host's deployment separate", async () => {
    const target_triple = "aarch64-unknown-linux-musl";
    const build = { version: "0.1.4", source_revision: "abcd0123".repeat(5), source_fingerprint: "a".repeat(64), dirty: false };
    const build_label = "stable-v0.1.4-abcd012";
    const local = { active: "local", build_label: "local build", origin: { channel: "local" }, missing: false, installed_targets: [] };
    api.runtime.mockResolvedValueOnce(local).mockResolvedValue({
      ...local, installed_targets: [{ target_triple, builds: [{ build, build_label, bundle_id: "d".repeat(64), channels: ["stable"] }] }],
    });
    api.published.mockResolvedValue({ default_channel: "stable", catalog: {
      channel: "stable", publication_id: "v0.1.4", app_version: build.version, git_revision: build.source_revision,
      targets: [{ target_triple, compatible: true, components: {} }],
    } });
    renderPage(<AboutPage {...props()} />);
    const remote = await selectMachine("Development");
    const inventory = screen.getByRole("region", { name: /runtime targets/i });
    const updater = await screen.findByText(/\b0\.1\.3\b/);
    expect(updater.compareDocumentPosition(inventory) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    const store = within(inventory);
    expect(api.published).not.toHaveBeenCalled();
    fireEvent.click(store.getByRole("button", { name: "Add target" }));
    await waitFor(() => expect(store.getByLabelText("Target")).toHaveProperty("value", target_triple));
    fireEvent.click(store.getByRole("button", { name: "Fetch target" }));
    const installed = await store.findByRole("rowheader", { name: build_label });
    expect(installed.closest("tr")?.textContent).toContain(target_triple);
    expect(api.fetch).toHaveBeenCalledExactlyOnceWith(target_triple, "stable", expect.any(String));
    expect(api.runtime.mock.calls).toEqual([[{ kind: "local" }], [{ kind: "local" }]]);
    expect(api.sync).not.toHaveBeenCalled();
    expect(api.update).not.toHaveBeenCalled();
    expect(remote.getByRole("button", { name: "Check installation" })).toBeTruthy();
    expect(screen.getByRole("group", { name: "Desktop application" }).textContent).toContain("SHA 12121212");
  });

  it("shows the recorded channels of the running and on-disk builds independently", async () => {
    const identity = { version: "0.1.5", source_revision: "a".repeat(40), source_fingerprint: "b".repeat(64), dirty: false };
    const replacement = { ...identity, source_fingerprint: "b".repeat(8) + "c".repeat(56) };
    const observations = structuredClone(snapshot);
    observations.components[1].running = { ...current, ...identity };
    observations.components[1].installed = { ...current, ...replacement };
    api.versions.mockResolvedValue(observations);
    api.runtime.mockResolvedValue({
      active: "selected", build_label: "selected-build", origin: { channel: "local" }, missing: false,
      build_channels: [
        { build: { ...identity, dirty: true }, channels: ["pr-123"] },
        { build: identity, channels: ["nightly", "stable"] },
        { build: replacement, channels: ["local"] },
      ],
    });
    renderPage(<AboutPage {...props()} />);
    await within(await screen.findByRole("region", { name: "This computer" })).findByText("selected-build");
    await servicesReady();
    const table = within(screen.getByRole("table", { name: "This computer component versions" }));
    const owner = table.getAllByText("Running").find((label) => label.parentElement?.classList.contains("about-build-summary"))!.parentElement!;
    const disk = table.getByText("On disk").parentElement!;
    expect(owner.textContent).toContain(identity.source_revision.slice(0, 7));
    expect(disk.textContent).toContain(replacement.source_revision.slice(0, 7));
    expect(owner.textContent).toContain(identity.source_fingerprint.slice(0, 9));
    expect(disk.textContent).toContain(replacement.source_fingerprint.slice(0, 9));
    expect(owner.textContent).toMatch(/nightly/i);
    expect(owner.textContent).toMatch(/stable/i);
    expect(owner.textContent).not.toMatch(/local|pr-123/i);
    expect(disk.textContent).toMatch(/local/i);
    expect(disk.textContent).not.toMatch(/nightly|stable|pr-123/i);
    const details = openDetails(table.getByRole("button", { name: "Show details for ctld (SSH)" }));
    const running = within(details.getByRole("region", { name: "Running" }));
    const installed = within(details.getByRole("region", { name: "On disk" }));
    expect(running.getByText(identity.source_revision)).toBeTruthy();
    expect(installed.getByText(replacement.source_revision)).toBeTruthy();
    expect(running.getByText("Channel").parentElement!.textContent).toMatch(/nightly/i);
    expect(installed.getByText("Channel").parentElement!.textContent).toMatch(/local/i);
  });

  it.each(["nightly", "local", "stable"])("summarizes matching %s builds with the appropriate commit detail", async (channel) => {
    const identity = { version: "0.1.5", source_revision: "0123456789abcdef".repeat(2) + "01234567", source_fingerprint: "f".repeat(64), dirty: false };
    const observations = structuredClone(snapshot);
    observations.components[1].running = { ...current, ...identity };
    observations.components[1].installed = { ...current, ...identity };
    observations.components[1].status = "current";
    api.versions.mockResolvedValue(observations);
    api.runtime.mockResolvedValue({
      active: "selected", build_label: "selected-build", origin: { channel }, missing: false,
      build_channels: [{ build: identity, channels: [channel] }],
    });
    renderPage(<AboutPage {...props()} />);
    await within(await screen.findByRole("region", { name: "This computer" })).findByText("selected-build");
    await servicesReady();
    const table = within(screen.getByRole("table", { name: "This computer component versions" }));
    const summary = table.getByText("Running + on disk").parentElement!;
    expect(summary.textContent).toContain(identity.version);
    expect(summary.textContent).toContain(channel);
    const short_sha = identity.source_revision.slice(0, 7);
    if (channel === "stable") expect(summary.textContent).not.toContain(short_sha);
    else expect(summary.textContent).toContain(short_sha);
    const details = openDetails(table.getByRole("button", { name: "Show details for ctld (SSH)" }));
    expect(details.getByText(identity.source_revision)).toBeTruthy();
  });

  it("checks the remote inventory on demand and leaves an unrecognized owner's channel unknown", async () => {
    const identity = { version: "0.1.5", source_revision: "a".repeat(40), source_fingerprint: "b".repeat(64), dirty: false };
    const observations = structuredClone(snapshot);
    observations.components[2].running = { ...current, ...identity, source_fingerprint: null };
    observations.components[2].installed = { ...current, ...identity };
    api.versions.mockResolvedValue(observations);
    const inventory = { active: "selected", missing: false, build_label: "runtime", origin: { channel: "stable" } };
    api.runtime.mockResolvedValueOnce({ ...inventory, build_channels: [{ build: identity, channels: ["local"] }] })
      .mockResolvedValueOnce({ ...inventory, build_channels: [{ build: identity, channels: ["nightly"] }] });
    renderPage(<AboutPage {...props()} />);
    await servicesReady();
    const host = await selectMachine("Development");
    const table = within(host.getByRole("table"));
    const details = openDetails(table.getByRole("button", { name: "Show details for ctl-agent — Development" }));
    const running = within(details.getByRole("region", { name: "Running · last observed" }));
    const installed = within(details.getByRole("region", { name: "On disk" }));
    expect(installed.getByText("Channel").parentElement!.textContent).toMatch(/not checked/i);
    expect(api.runtime).toHaveBeenCalledExactlyOnceWith({ kind: "local" });
    fireEvent.click(host.getByRole("button", { name: "Check installation" }));
    await waitFor(() => expect(installed.getByText("Channel").parentElement!.textContent).toMatch(/nightly/i));
    const owner_channel = running.getByText("Channel").parentElement!.textContent;
    expect(owner_channel).toMatch(/unknown/i);
    expect(owner_channel).not.toMatch(/stable|nightly|local/i);
    expect(api.runtime).toHaveBeenLastCalledWith(saved_target);
  });

  it("shows the Windows x64 install command only for a confirmed empty local runtime", async () => {
    vi.spyOn(navigator, "platform", "get").mockReturnValue("Win32");
    api.runtime.mockRejectedValueOnce(new Error("Cannot read the local runtime."));
    renderPage(<AboutPage {...props()} />);
    const local = within(await screen.findByRole("region", { name: "This computer" }));
    const command = "ctlup install --channel stable";
    await local.findByText("Cannot read the local runtime.");
    expect(local.queryByText(command)).toBeNull();
    expect(local.queryByRole("button", { name: "Install from…" })).toBeNull();

    fireEvent.click(local.getByRole("button", { name: "Check installation" }));
    await local.findByText(command);
    expect(local.getByText(/On Windows x64/)).toBeTruthy();
    const remote = await selectMachine("Development");
    fireEvent.click(remote.getByRole("button", { name: "Check installation" }));
    await remote.findByText("No runtime installed");
    expect(remote.queryByText(command)).toBeNull();

    api.runtime.mockResolvedValueOnce({ active: "stable-id", build_label: "installed-build", missing: false, origin: { channel: "stable" } });
    const currentLocal = await selectMachine("This computer");
    fireEvent.click(currentLocal.getByRole("button", { name: "Check installation" }));
    await currentLocal.findByText("installed-build");
    expect(currentLocal.queryByText(command)).toBeNull();
  });

  it("inspects a saved SSH installation on demand and syncs its selected route", async () => {
    const route = { ...saved_target, method_id: "gateway", remote_info: { remote_id: "account", agent_version: "test" } };
    const local = { kind: "local" };
    const installed = { active: "old-id", build_label: "stable-v0.1.4-build", missing: false, origin: { channel: "stable", publication_id: "0.1.4" } };
    api.runtime.mockResolvedValueOnce({ ...installed, build_label: "dev-local-build", origin: { channel: "dev" } })
      .mockResolvedValueOnce(installed)
      .mockResolvedValue({ ...installed, active: "new-id", build_label: "stable-v0.1.5-build", origin: { channel: "stable", publication_id: "0.1.5" } });
    renderPage(<AboutPage {...props()} remote_targets={[route]} />);
    await within(await screen.findByRole("region", { name: "This computer" })).findByText("dev-local-build");
    const remote = await selectMachine("Development");
    expect(api.runtime).toHaveBeenCalledExactlyOnceWith(local);
    expect(api.published).not.toHaveBeenCalled();
    fireEvent.click(remote.getByRole("button", { name: "Check installation" }));
    await remote.findByText("stable-v0.1.4-build");
    expect(api.runtime).toHaveBeenLastCalledWith(route);

    fireEvent.click(remote.getByRole("button", { name: "Sync" }));
    await remote.findByText("stable-v0.1.5-build");
    expect(api.sync).toHaveBeenCalledExactlyOnceWith(route, expect.any(String));
    expect(api.runtime).toHaveBeenLastCalledWith(route);
    expect(api.update).not.toHaveBeenCalled();
    expect(api.restart).not.toHaveBeenCalled();
  });

  it("discards a pending inspection when the selected saved route changes", async () => {
    const first = { ...saved_target, method_id: "one", destination: "first-alias" };
    const second = { ...saved_target, method_id: "two", destination: "second-alias" };
    const record = { active: "runtime", origin: { channel: "stable" }, missing: false };
    const options = props();
    const page = renderPage(<AboutPage {...options} remote_targets={[first]} />);
    await waitFor(() => expect(api.runtime).toHaveBeenCalledExactlyOnceWith({ kind: "local" }));
    let finish!: (value: unknown) => void;
    api.runtime.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    await selectMachine("Development");
    const remote = () => within(screen.getByRole("region", { name: "Development" }));
    fireEvent.click(remote().getByRole("button", { name: "Check installation" }));
    await waitFor(() => expect(api.runtime).toHaveBeenLastCalledWith(first));
    page.rerender(<AboutPage {...options} remote_targets={[second]} />);
    api.runtime.mockResolvedValueOnce({ ...record, build_label: "second-route-build" });
    fireEvent.click(remote().getByRole("button", { name: "Check installation" }));
    await remote().findByText("second-route-build");
    expect(api.runtime).toHaveBeenLastCalledWith(second);
    await act(async () => { finish({ ...record, build_label: "first-route-build" }); });
    expect(remote().queryByText("first-route-build")).toBeNull();
    expect(remote().getByText("second-route-build")).toBeTruthy();
  });

  it("keeps a failed sync scoped to the machine without installing another source", async () => {
    api.runtime.mockResolvedValue({
      active: "pinned-id", build_label: "dev-pinned-build",
      origin: { channel: "dev" }, missing: true,
    });
    api.sync.mockRejectedValueOnce(new Error("Exact build missing from cache."));
    renderPage(<AboutPage {...props()} />);
    const local = within(await screen.findByRole("region", { name: "This computer" }));
    await local.findByText("dev-pinned-build");
    fireEvent.click(local.getByRole("button", { name: "Sync" }));
    expect(await local.findByText(/Exact build missing from cache/)).toBeTruthy();
    expect(local.getByRole("button", { name: "Install from…" })).toBeTruthy();
    expect(api.sync).toHaveBeenCalledExactlyOnceWith({ kind: "local" }, expect.any(String));
    expect(api.update).not.toHaveBeenCalled();
  });

  it("stops only the selected machine's sync and checks its final installation", async () => {
    api.runtime.mockResolvedValue({ active: "active", build_label: "installed-build", missing: false, origin: { channel: "stable" } });
    let interrupt!: (error: Error) => void;
    api.sync.mockImplementationOnce(() => new Promise((_resolve, reject) => { interrupt = reject; }));
    renderPage(<AboutPage {...props()} />);
    const local = within(await screen.findByRole("region", { name: "This computer" }));
    await local.findByText("installed-build");
    fireEvent.click(local.getByRole("button", { name: "Sync" }));
    const id = api.sync.mock.calls[0][1];
    expect(id).toEqual(expect.any(String));
    fireEvent.click(await local.findByRole("button", { name: "Stop update" }));
    expect(api.cancel_update).toHaveBeenCalledExactlyOnceWith(id);
    expect(local.getByRole("button", { name: "Stopping update…" })).toHaveProperty("disabled", true);
    await act(async () => { interrupt(new Error("Sync cancelled.")); });
    expect(await local.findByText("Sync cancelled.")).toBeTruthy();
    expect(local.getByRole("button", { name: "Sync" })).toHaveProperty("disabled", false);
    expect(api.runtime).toHaveBeenCalledTimes(2);
    expect(api.runtime).toHaveBeenLastCalledWith({ kind: "local" });
  });

  it("renders native maintenance details and result counts in the selected language", async () => {
    localStorage.setItem("ctmux.desktop.language", "zh-Hans");
    const component = { ...snapshot.components[1], component_id: "ctmuxd", component: "ctmuxd", label: "ctmuxd" };
    api.versions.mockResolvedValue({ components: [snapshot.components[0], component] });
    api.preflight.mockResolvedValue({ ...preflight, component_id: "ctmuxd", component: "ctmuxd", label: "ctmuxd",
      impact: { ...preflight.impact, kind: "local_terminal" } });
    api.restart.mockResolvedValue({
      component_id: "ctmuxd",
      running: current,
      summary: { kind: "local_terminal_restart", ended_sessions: 2 },
    });
    renderPage(<LocaleProvider><AboutPage {...props()} /></LocaleProvider>);
    fireEvent.click(await screen.findByRole("button", { name: "重启 ctmuxd" }));
    const dialog = await screen.findByRole("dialog", { name: "重启 ctmuxd" });
    expect(within(dialog).getByText(/结束所有本地终端会话/)).toBeTruthy();
    fireEvent.click(within(dialog).getByRole("button", { name: "重启 ctmuxd" }));
    await waitFor(() => expect(notifications.snapshot().entries).toHaveLength(1));
    expect(formatText("zh-Hans", notifications.snapshot().entries[0].message)).toContain("已结束 2 个终端会话");
  });

  it("shows installed remote builds without treating this app's reference as an installation", async () => {
    const next = structuredClone(snapshot);
    next.components[2] = { ...next.components[2], component: "ctmuxd", label: "ctmuxd — Saved host", installed: current, running: null,
      legacy_protocols: [{ name: "ctmux_control", version: 1 }, { name: "ctmux", version: 13 }], restart_required: true, status: "incompatible" };
    api.versions.mockResolvedValue(next);
    renderPage(<AboutPage {...props()} />);
    await selectMachine("Development");
    const toggle = await screen.findByRole("button", { name: "Show details for ctmuxd — Saved host" });
    const row = within(toggle.closest("tr")!);
    expect(row.getByText("Restart required")).toBeTruthy();
    expect(row.getByText("Protocol mismatch")).toBeTruthy();
    const details = openDetails(toggle);
    const observed = within(details.getByRole("region", { name: "Running · last observed" }));
    expect(observed.getByLabelText("Session legacy 13: Unverified")).toBeTruthy();
    expect(observed.queryByText(current.source_revision!)).toBeNull();
    const installed = within(details.getByRole("region", { name: "On disk" }));
    expect(installed.getByRole("list", { name: "On disk protocols" })).toBeTruthy();
    expect(installed.getByText(current.source_revision!)).toBeTruthy();
    expect(details.queryByText(/1\.0\.13/)).toBeNull();
  });

  it("keeps failed saved hosts manageable without an active terminal transport", async () => {
    const next = structuredClone(snapshot);
    next.components[2] = { ...next.components[2], component_id: "remote:saved:dev:ctl_agent", observation: "not_checked", running: null, installed: null, status: "unavailable", error: "SSH account is not connected." };
    api.versions.mockResolvedValue(next);
    renderPage(<AboutPage {...props()} remote_targets={[{ kind: "ssh", host_id: "dev", destination: "dev" }]} />);
    await servicesReady();
    const machine = await selectMachine("Development");
    expect(machine.getByRole("button", { name: "Install from…" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Check host Development" })).toBeTruthy();
    expect(screen.getAllByText("Not checked").length).toBeGreaterThan(0);
    expect(api.preflight).not.toHaveBeenCalled();
    expect(api.restart).not.toHaveBeenCalled();
  });

  it("updates a saved Windows host through its pinned route without offering inspection", async () => {
    const target = { kind: "ssh" as const, host_id: "dev", host_name: "Development", destination: "win",
      remote_platform: "windows" as const, port: 2022, method_id: "tunnel",
      remote_info: { remote_id: "saved-account", agent_version: "test" } };
    api.update.mockResolvedValueOnce([{ host_index: 0, state: "failed", result: null, error: "SSH account identity changed." }]);
    renderPage(<AboutPage {...props()} remote_targets={[target]} />);
    const machine = await selectMachine("Development");
    fireEvent.click(machine.getByRole("button", { name: "Install from…" }));
    expect(screen.queryByRole("button", { name: "Check host Development" })).toBeNull();
    const dialog = await screen.findByRole("dialog", { name: "Install runtime" });
    expect(within(dialog).getByRole("checkbox", { name: "Development" })).toHaveProperty("checked", true);
    fireEvent.click(within(dialog).getByRole("button", { name: "Install" }));
    await within(dialog).findByText("SSH account identity changed.");
    expect(api.update).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ targets: [target] }), expect.any(Function), expect.any(Function));
    expect(api.restart).not.toHaveBeenCalled();
  });

  it("offers native-approved actions per component and uses reconnect impact without a destructive label", async () => {
    const rows = structuredClone(snapshot);
    rows.components[2].action = "reconnect";
    rows.components[2].restart_supported = false;
    rows.components.push({ ...rows.components[1], component_id: "local-taskd", component: "ctl-taskd", label: "ctl-taskd" });
    api.versions.mockResolvedValue(rows);
    const reconnect = { ...preflight, component_id: "remote-1", component: "ctl_agent" as const, location: "remote" as const, host_id: "dev", action: "reconnect" as const, label: "ctl-agent — Development", impact: { ...preflight.impact, description: "Reconnects these terminal transports. Remote sessions keep running." } };
    api.preflight.mockResolvedValue(reconnect);
    const execute_action = vi.fn().mockResolvedValue({ detail: "Two terminal transports reconnected." });
    renderPage(<AboutPage {...props()} execute_action={execute_action} />);
    await servicesReady();
    expect(await screen.findByRole("button", { name: "Restart ctl-taskd" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Restart ctmux" })).toBeNull();
    await selectMachine("Development");
    fireEvent.click(screen.getByRole("button", { name: "Reconnect ctl-agent — Development" }));
    const dialog = await screen.findByRole("dialog", { name: "Reconnect ctl-agent — Development" });
    expect(within(dialog).getByText(/Remote sessions keep running/)).toBeTruthy();
    fireEvent.click(within(dialog).getByRole("button", { name: "Reconnect ctl-agent" }));
    await waitFor(() => expect(execute_action).toHaveBeenCalledWith(reconnect));
    expect(api.restart).not.toHaveBeenCalled();
    await waitFor(() => expect(firstNotification()).toMatchObject({ severity: "success", source: "Components", message: "Two terminal transports reconnected." }));
    expect(screen.queryByText("Two terminal transports reconnected.")).toBeNull();
  });

  it("distinguishes unreported builds from incomplete verification without hiding protocol mismatch", async () => {
    const legacy = structuredClone(snapshot);
    legacy.components[1] = { ...legacy.components[1], component: "ctl-taskd", label: "ctl-taskd", status: "unknown", running: { ...current, version: null, source_revision: null, source_fingerprint: null } };
    legacy.components[2].running = { ...current, source_fingerprint: null };
    api.versions.mockResolvedValue(legacy);
    renderPage(<AboutPage {...props()} />);
    await servicesReady();
    const legacy_row = (await screen.findByText("ctl-taskd")).closest("tbody")!;
    expect(within(legacy_row).getByText("Build not reported")).toBeTruthy();
    await selectMachine("Development");
    expect(screen.getByText("Build unverified")).toBeTruthy();
    await selectMachine("This computer");
    legacy.components[1].status = "incompatible";
    fireEvent.click(screen.getByRole("button", { name: "Refresh service status" }));
    expect(await screen.findAllByText("Protocol mismatch")).toBeTruthy();
  });

  it("checks only when opened, and distinguishes observed build metadata from current versions", async () => {
    const options = props();
    const page = renderPage(<AboutPage {...options} visible={false} />);
    expect(api.versions).not.toHaveBeenCalled();
    page.rerender(<AboutPage {...options} />);
    await servicesReady();
    expect(await screen.findAllByText("Restart required")).toBeTruthy();
    await selectMachine("Development");
    const service = screen.getByRole("button", { name: "Show details for ctl-agent — Development" });
    const observed = within(service.closest("tr")!);
    expect(observed.getByText("Running · last observed")).toBeTruthy();
    expect(observed.getByText("Build not reported")).toBeTruthy();
    expect(screen.queryByText("Not running")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Back to workspace" }));
    expect(options.on_close).toHaveBeenCalledOnce();
  });

  it("keeps other components visible when one version probe fails", async () => {
    const partial = structuredClone(snapshot);
    partial.components[1].status = "unavailable";
    partial.components[1].error = "ctld timed out.";
    partial.components[1].running = null;
    api.versions.mockResolvedValue(partial);
    renderPage(<AboutPage {...props()} />);
    await servicesReady();
    expect(await screen.findByText("ctld timed out.")).toBeTruthy();
    expect(screen.getAllByText("Unavailable").length).toBeGreaterThan(0);
    await selectMachine("Development");
    expect(screen.getByRole("button", { name: "Show details for ctl-agent — Development" })).toBeTruthy();
  });

  it("preflights before confirming restart, then refreshes versions and connections", async () => {
    const options = props();
    renderPage(<AboutPage {...options} />);
    await servicesReady();
    fireEvent.click(await screen.findByRole("button", { name: "Restart ctld (SSH)" }));
    const dialog = await screen.findByRole("dialog", { name: "Restart ctld (SSH)" });
    expect(api.preflight).toHaveBeenCalledWith("owner-1");
    expect(api.restart).not.toHaveBeenCalled();
    expect(within(dialog).getByText(/Stops 2 managed VPN connections/)).toBeTruthy();
    await waitFor(() => expect(options.on_dialog_change).toHaveBeenLastCalledWith(true));
    fireEvent.click(within(dialog).getByRole("button", { name: "Restart ctld" }));
    await waitFor(() => expect(api.restart).toHaveBeenCalledWith("native-owner-token"));
    await waitFor(() => expect(api.versions).toHaveBeenCalledTimes(2));
    expect(options.on_restarted).toHaveBeenCalledOnce();
    await waitFor(() => expect(options.on_dialog_change).toHaveBeenLastCalledWith(false));
  });

  it("canceling the impact confirmation leaves the daemon untouched", async () => {
    renderPage(<AboutPage {...props()} />);
    await servicesReady();
    fireEvent.click(await screen.findByRole("button", { name: "Restart ctld (SSH)" }));
    const dialog = await screen.findByRole("dialog", { name: "Restart ctld (SSH)" });
    fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(api.restart).not.toHaveBeenCalled();
    expect((screen.getByRole("button", { name: "Restart ctld (SSH)" }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("shows preflight errors at their component without opening a confirmation", async () => {
    api.preflight.mockRejectedValue({ message: "Update ctld before it can restart cooperatively." });
    renderPage(<AboutPage {...props()} />);
    await servicesReady();
    fireEvent.click(await screen.findByRole("button", { name: "Restart ctld (SSH)" }));
    expect(await screen.findByText("Update ctld before it can restart cooperatively.")).toBeTruthy();
    expect(notifications.snapshot().entries).toHaveLength(0);
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(api.restart).not.toHaveBeenCalled();
  });

  it("refreshes observations after restart failure and keeps the error visible", async () => {
    api.restart.mockRejectedValue({ message: "Replacement readiness timed out." });
    const options = props();
    renderPage(<AboutPage {...options} />);
    await servicesReady();
    fireEvent.click(await screen.findByRole("button", { name: "Restart ctld (SSH)" }));
    const dialog = await screen.findByRole("dialog", { name: "Restart ctld (SSH)" });
    fireEvent.click(within(dialog).getByRole("button", { name: "Restart ctld" }));
    await waitFor(() => expect(firstNotification()).toMatchObject({ severity: "error", source: "Components", message: "ctld (SSH): Replacement readiness timed out." }));
    expect(screen.queryByText("Replacement readiness timed out.")).toBeNull();
    await waitFor(() => expect(api.versions).toHaveBeenCalledTimes(2));
    expect(options.on_restarted).toHaveBeenCalledOnce();
  });

  it("retains the last successful snapshot when refresh fails", async () => {
    renderPage(<AboutPage {...props()} />);
    await servicesReady();
    await screen.findAllByText("Restart required");
    api.versions.mockRejectedValue({ message: "Version service unavailable." });
    fireEvent.click(screen.getByRole("button", { name: "Refresh service status" }));
    expect(await screen.findByText(/Showing the last successful check/)).toBeTruthy();
    expect(screen.getAllByText("Restart required").length).toBeGreaterThan(0);
  });

  it("shows required protocols and separate build identities when running metadata is incomplete", async () => {
    const partial = structuredClone(snapshot);
    partial.components[1].running = { ...current, protocols: [], source_fingerprint: "running-build" };
    api.versions.mockResolvedValue(partial);
    renderPage(<AboutPage {...props()} />);
    await servicesReady();
    const toggle = await screen.findByRole("button", { name: "Show details for ctld (SSH)" });
    const details = openDetails(toggle);
    expect(details.getByLabelText("ctld IPC Not reported: Unverified").title).toContain("Requires 1.0.12");
    expect(details.getByText("running-build")).toBeTruthy();
    expect(details.getByText(current.source_fingerprint!)).toBeTruthy();
  });

  it("compares running protocols with app requirements even when the available binary is also stale", async () => {
    const stale = structuredClone(snapshot);
    stale.components[1].running = { ...current, protocols: [{ name: "ctld", build: 11, version: "1.0.11", supported_versions: ["1.0.11"] }] };
    stale.components[1].available = { ...current, protocols: [{ name: "ctld", build: 11, version: "1.0.11", supported_versions: ["1.0.11"] }] };
    stale.components[1].required_protocols = [{ name: "ctld", build: 12, version: "1.0.12", supported_versions: ["1.0.12"] }, { name: "ctld_lifecycle", build: 1, version: "1.0.1", supported_versions: ["1.0.1"] }];
    stale.components[1].status = "incompatible";
    api.versions.mockResolvedValue(stale);
    renderPage(<AboutPage {...props()} />);
    await servicesReady();
    const toggle = await screen.findByRole("button", { name: "Show details for ctld (SSH)" });
    expect(within(toggle.closest("tr")!).getByText("Protocol mismatch")).toBeTruthy();
    const protocols = openDetails(toggle).getByRole("list", { name: "Running + on disk protocols" });
    expect(within(protocols).getByLabelText("ctld IPC 1.0.11: Incompatible").title).toContain("Requires 1.0.12");
    expect(within(protocols).getByLabelText("Lifecycle Not reported: Unverified").title).toContain("Requires 1.0.1");
  });

  it("rejects a late observation from before About was reopened", async () => {
    let complete!: (value: ComponentVersionsSnapshot) => void;
    api.versions.mockImplementationOnce(() => new Promise((resolve) => { complete = resolve; }));
    const options = props();
    const page = renderPage(<AboutPage {...options} />);
    await waitFor(() => expect(api.versions).toHaveBeenCalledOnce());
    page.rerender(<AboutPage {...options} visible={false} />);
    page.rerender(<AboutPage {...options} />);
    await servicesReady();
    await screen.findAllByText("Restart required");
    const old = structuredClone(snapshot);
    old.components[1].status = "not_running";
    old.components[1].restart_required = false;
    await act(async () => { complete(old); });
    expect(screen.getAllByText("Restart required").length).toBeGreaterThan(0);
    expect(screen.queryByText("Not running")).toBeNull();
  });

  it("does not present a late restart confirmation after leaving About", async () => {
    let complete!: (value: ComponentActionPreflight) => void;
    api.preflight.mockImplementationOnce(() => new Promise((resolve) => { complete = resolve; }));
    const options = props();
    const page = renderPage(<AboutPage {...options} />);
    await servicesReady();
    fireEvent.click(await screen.findByRole("button", { name: "Restart ctld (SSH)" }));
    await waitFor(() => expect(api.preflight).toHaveBeenCalledOnce());
    page.rerender(<AboutPage {...options} visible={false} />);
    await act(async () => { complete(preflight); });
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(api.restart).not.toHaveBeenCalled();
  });

  it("does not let an abandoned preflight replace a new confirmation after reopening About", async () => {
    let complete!: (value: ComponentActionPreflight) => void;
    api.preflight.mockImplementationOnce(() => new Promise((resolve) => { complete = resolve; }));
    const options = props();
    const page = renderPage(<AboutPage {...options} />);
    await servicesReady();
    fireEvent.click(await screen.findByRole("button", { name: "Restart ctld (SSH)" }));
    await waitFor(() => expect(api.preflight).toHaveBeenCalledOnce());
    page.rerender(<AboutPage {...options} visible={false} />);
    page.rerender(<AboutPage {...options} />);
    fireEvent.click(screen.getByRole("button", { name: "Restart ctld (SSH)" }));
    const dialog = await screen.findByRole("dialog", { name: "Restart ctld (SSH)" });
    await act(async () => { complete({ ...preflight, action_token: "abandoned-token" }); });
    fireEvent.click(within(dialog).getByRole("button", { name: "Restart ctld" }));
    await waitFor(() => expect(api.restart).toHaveBeenCalledWith("native-owner-token"));
  });

  it("reports a successful restart separately from a failed version refresh", async () => {
    api.versions.mockResolvedValueOnce(structuredClone(snapshot)).mockRejectedValueOnce(new Error("Probe timed out."));
    const options = props();
    renderPage(<AboutPage {...options} />);
    await servicesReady();
    fireEvent.click(await screen.findByRole("button", { name: "Restart ctld (SSH)" }));
    const dialog = await screen.findByRole("dialog", { name: "Restart ctld (SSH)" });
    fireEvent.click(within(dialog).getByRole("button", { name: "Restart ctld" }));
    await waitFor(() => expect(firstNotification()).toMatchObject({ severity: "success", message: "ctld (SSH) restarted." }));
    expect(screen.queryByText("ctld (SSH) restarted.")).toBeNull();
    expect(await screen.findByText(/Could not refresh versions: Probe timed out./)).toBeTruthy();
    expect(screen.getAllByText("Restart required").length).toBeGreaterThan(0);
    expect(options.on_restarted).toHaveBeenCalledOnce();
  });

  it("does not lose completion when About is hidden during restart", async () => {
    let complete!: (value: unknown) => void;
    api.restart.mockReturnValue(new Promise((resolve) => { complete = resolve; }));
    const options = props();
    const page = renderPage(<AboutPage {...options} />);
    await servicesReady();
    fireEvent.click(await screen.findByRole("button", { name: "Restart ctld (SSH)" }));
    const dialog = await screen.findByRole("dialog", { name: "Restart ctld (SSH)" });
    fireEvent.click(within(dialog).getByRole("button", { name: "Restart ctld" }));
    await waitFor(() => expect(api.restart).toHaveBeenCalledOnce());
    page.rerender(<AboutPage {...options} visible={false} />);
    await act(async () => { complete({ component_id: "owner-1", running: current }); });
    expect(options.on_restarted).toHaveBeenCalledOnce();
    expect(api.versions).toHaveBeenCalledTimes(2);
    expect(firstNotification()).toMatchObject({ severity: "success", source: "Components", message: "ctld (SSH) restarted." });
  });

  it("keeps component update results in the dialog without duplicating its summary on About or in notifications", async () => {
    api.runtime.mockResolvedValueOnce({ active: null, build_label: null, origin: null, missing: false })
      .mockResolvedValue({ active: "verified-build", build_label: "stable-v0.1.5-installed", origin: { channel: "stable" }, missing: false });
    api.update.mockResolvedValue([{ host_index: 0, state: "complete", error: null, result: { package: "full_bundle", bundle_id: "verified-build", target_triple: "test-platform", services_preserved: true } }]);
    renderPage(<AboutPage {...props()} />);
    fireEvent.click(within(await screen.findByRole("region", { name: "This computer" })).getByRole("button", { name: "Install from…" }));
    const dialog = screen.getByRole("dialog", { name: "Install runtime" });
    fireEvent.click(within(dialog).getByRole("button", { name: "Install" }));
    expect(await within(dialog).findByText("1 of 1 hosts updated. Running services were preserved.")).toBeTruthy();
    expect(screen.queryByText("1 hosts updated. Running services were preserved; restart separately where required.")).toBeNull();
    expect(notifications.snapshot().entries).toHaveLength(0);
    fireEvent.click(within(dialog).getByRole("button", { name: "Done" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(screen.queryByText(/hosts updated/)).toBeNull();
    expect(await within(screen.getByRole("region", { name: "This computer" })).findByText("stable-v0.1.5-installed")).toBeTruthy();
    expect(api.runtime).toHaveBeenLastCalledWith({ kind: "local" });
  });
});
