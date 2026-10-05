// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ComponentVersionInfo, ComponentVersionsSnapshot, ComponentActionPreflight } from "../lib/types";
import { AboutPage } from "./AboutPage";

const api = vi.hoisted(() => ({ versions: vi.fn(), preflight: vi.fn(), restart: vi.fn(), bundles: vi.fn(), select: vi.fn() }));
vi.mock("../lib/tauri", () => ({ getComponentVersions: api.versions, preflightComponentAction: api.preflight, executeComponentAction: api.restart, getComponentBundles: api.bundles, selectComponentBundle: api.select }));

const current: ComponentVersionInfo = { version: "0.1.0", source_revision: "abcdef123456", source_fingerprint: "current", dirty: false, protocols: [{ name: "ctld", build: 12, version: "1.0.12", supported_versions: ["1.0.12"] }] };
const snapshot: ComponentVersionsSnapshot = { components: [
  { component_id: "app", component: "ctmux", label: "ctmux", location: "local", host_id: null, observation: "bundled", status: "current", running: current, available: current, restart_supported: false, action: null, detail: null, error: null },
  { component_id: "owner-1", component: "ctld", label: "ctld (SSH)", location: "local", host_id: null, observation: "running", status: "different_build", running: { ...current, source_revision: "112233445566" }, available: current, restart_supported: true, action: "restart", detail: "SSH and VPN broker", error: null },
  { component_id: "remote-1", component: "ctl_agent", label: "ctl-agent — Development", location: "remote", host_id: "dev", host_name: "Development", observation: "last_observed", status: "unknown", running: { ...current, source_revision: null, source_fingerprint: null }, available: current, restart_supported: false, action: null, detail: null, error: null },
] };
const preflight: ComponentActionPreflight = { action_token: "native-owner-token", component_id: "owner-1", component: "ctld", location: "local", host_id: null, action: "restart", label: "ctld (SSH)", running: snapshot.components[1].running, available: current, impact: { ssh_connections: null, port_forwards: null, vpn_connections: 2, terminal_sessions: null, description: "Stops 2 managed VPN connections and may interrupt SSH connections and port forwards." } };
async function expandGroups() {
  await screen.findByRole("button", { name: "Show components for This computer" });
  for (const toggle of screen.getAllByRole("button", { name: /^Show components for / })) fireEvent.click(toggle);
}

const props = () => ({ visible: true, on_close: vi.fn(), on_restarted: vi.fn(), on_dialog_change: vi.fn() });

beforeEach(() => {
  vi.clearAllMocks();
  api.bundles.mockResolvedValue({ bundles: [], errors: [] });
  api.versions.mockResolvedValue(structuredClone(snapshot));
  api.preflight.mockResolvedValue(structuredClone(preflight));
  api.restart.mockResolvedValue({ component_id: "owner-1", running: current });
});
afterEach(cleanup);

describe("About page", () => {
  it("shows installed remote builds without treating this app's reference as an installation", async () => {
    const next = structuredClone(snapshot);
    next.components[2] = { ...next.components[2], component: "ctmuxd", label: "ctmuxd — Saved host", installed: current, running: null,
      legacy_protocols: [{ name: "ctmux_control", version: 1 }, { name: "ctmux", version: 13 }], restart_required: true, status: "incompatible" };
    api.versions.mockResolvedValue(next);
    render(<AboutPage {...props()} />);
    await expandGroups();
    const row = (await screen.findByTitle(/ctmuxd — Saved host/)).closest("tbody")!;
    expect(screen.getAllByRole("columnheader", { name: "State / build" })).toHaveLength(2);
    expect(within(row).getByText("Restart required")).toBeTruthy();
    expect(within(row).getByLabelText("Local control legacy 1: Unverified")).toBeTruthy();
    expect(within(row).getByLabelText("Session legacy 13: Unverified")).toBeTruthy();
    expect(within(row).getByTitle(/Build: current/).textContent).toContain("abcdef12");
    expect(within(row).queryByText(/1\.0\.13/)).toBeNull();
  });

  it("keeps failed saved hosts manageable without an active terminal transport", async () => {
    const next = structuredClone(snapshot);
    next.components[2] = { ...next.components[2], component_id: "remote:saved:dev:ctl_agent", observation: "not_checked", running: null, installed: null, status: "unavailable", error: "SSH account is not connected." };
    api.versions.mockResolvedValue(next);
    render(<AboutPage {...props()} remote_targets={[{ kind: "ssh", host_id: "dev", destination: "dev" }]} />);
    await expandGroups();
    expect(await screen.findByRole("button", { name: "Update components Development" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Check host Development" })).toBeTruthy();
    expect(screen.getAllByText("Not checked").length).toBeGreaterThan(0);
    expect(api.preflight).not.toHaveBeenCalled();
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
    render(<AboutPage {...props()} execute_action={execute_action} />);
    await expandGroups();
    expect(await screen.findByRole("button", { name: "Restart ctl-taskd" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Restart ctmux" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Reconnect ctl-agent — Development" }));
    const dialog = await screen.findByRole("dialog", { name: "Reconnect ctl-agent — Development" });
    expect(within(dialog).getByText(/Remote sessions keep running/)).toBeTruthy();
    fireEvent.click(within(dialog).getByRole("button", { name: "Reconnect ctl-agent" }));
    await waitFor(() => expect(execute_action).toHaveBeenCalledWith(reconnect));
    expect(api.restart).not.toHaveBeenCalled();
    expect(await screen.findByText("Two terminal transports reconnected.")).toBeTruthy();
  });

  it("distinguishes unreported builds from incomplete verification without hiding protocol mismatch", async () => {
    const legacy = structuredClone(snapshot);
    legacy.components[1] = { ...legacy.components[1], component: "ctl-taskd", label: "ctl-taskd", status: "unknown", running: { ...current, version: null, source_revision: null, source_fingerprint: null } };
    legacy.components[2].running = { ...current, source_fingerprint: null };
    api.versions.mockResolvedValue(legacy);
    render(<AboutPage {...props()} />);
    await expandGroups();
    const legacy_row = (await screen.findByText("ctl-taskd")).closest("tbody")!;
    expect(within(legacy_row).getByText("Build not reported")).toBeTruthy();
    expect(screen.getByText("Build unverified")).toBeTruthy();
    legacy.components[1].status = "incompatible";
    fireEvent.click(screen.getByRole("button", { name: "Refresh versions" }));
    expect(await screen.findAllByText("Protocol mismatch")).toBeTruthy();
  });

  it("checks only when opened, and distinguishes observed build metadata from current versions", async () => {
    const options = props();
    const page = render(<AboutPage {...options} visible={false} />);
    expect(api.versions).not.toHaveBeenCalled();
    page.rerender(<AboutPage {...options} />);
    await expandGroups();
    expect(await screen.findAllByText("Different build")).toBeTruthy();
    expect(screen.getByTitle(/Last observed/).textContent).toBe("ctl-agent");
    expect(screen.getByText("Build not reported")).toBeTruthy();
    expect(screen.queryByText("Outdated")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Back to workspace" }));
    expect(options.on_close).toHaveBeenCalledOnce();
  });

  it("keeps other components visible when one version probe fails", async () => {
    const partial = structuredClone(snapshot);
    partial.components[1].status = "unavailable";
    partial.components[1].error = "ctld timed out.";
    partial.components[1].running = null;
    api.versions.mockResolvedValue(partial);
    render(<AboutPage {...props()} />);
    await expandGroups();
    expect(await screen.findByText("ctld timed out.")).toBeTruthy();
    expect(screen.getByTitle(/ctl-agent — Development/)).toBeTruthy();
    expect(screen.getAllByText("Unavailable").length).toBeGreaterThan(0);
  });

  it("preflights before confirming restart, then refreshes versions and connections", async () => {
    const options = props();
    render(<AboutPage {...options} />);
    await expandGroups();
    fireEvent.click(await screen.findByRole("button", { name: "Restart ctld (SSH)" }));
    const dialog = await screen.findByRole("dialog", { name: "Restart ctld (SSH)" });
    expect(api.preflight).toHaveBeenCalledWith("owner-1");
    expect(api.restart).not.toHaveBeenCalled();
    expect(within(dialog).getByText(/Stops 2 managed VPN connections/)).toBeTruthy();
    expect(options.on_dialog_change).toHaveBeenLastCalledWith(true);
    fireEvent.click(within(dialog).getByRole("button", { name: "Restart ctld" }));
    await waitFor(() => expect(api.restart).toHaveBeenCalledWith("native-owner-token"));
    await waitFor(() => expect(api.versions).toHaveBeenCalledTimes(2));
    expect(options.on_restarted).toHaveBeenCalledOnce();
    expect(options.on_dialog_change).toHaveBeenLastCalledWith(false);
  });

  it("canceling the impact confirmation leaves the daemon untouched", async () => {
    render(<AboutPage {...props()} />);
    await expandGroups();
    fireEvent.click(await screen.findByRole("button", { name: "Restart ctld (SSH)" }));
    const dialog = await screen.findByRole("dialog", { name: "Restart ctld (SSH)" });
    fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(api.restart).not.toHaveBeenCalled();
    expect((screen.getByRole("button", { name: "Restart ctld (SSH)" }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("shows preflight errors at their component without opening a confirmation", async () => {
    api.preflight.mockRejectedValue({ message: "Update ctld before it can restart cooperatively." });
    render(<AboutPage {...props()} />);
    await expandGroups();
    fireEvent.click(await screen.findByRole("button", { name: "Restart ctld (SSH)" }));
    expect(await screen.findByText("Update ctld before it can restart cooperatively.")).toBeTruthy();
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(api.restart).not.toHaveBeenCalled();
  });

  it("refreshes observations after restart failure and keeps the error visible", async () => {
    api.restart.mockRejectedValue({ message: "Replacement readiness timed out." });
    const options = props();
    render(<AboutPage {...options} />);
    await expandGroups();
    fireEvent.click(await screen.findByRole("button", { name: "Restart ctld (SSH)" }));
    const dialog = await screen.findByRole("dialog", { name: "Restart ctld (SSH)" });
    fireEvent.click(within(dialog).getByRole("button", { name: "Restart ctld" }));
    expect(await screen.findByText("Replacement readiness timed out.")).toBeTruthy();
    await waitFor(() => expect(api.versions).toHaveBeenCalledTimes(2));
    expect(options.on_restarted).toHaveBeenCalledOnce();
  });

  it("retains the last successful snapshot when refresh fails", async () => {
    render(<AboutPage {...props()} />);
    await expandGroups();
    await screen.findAllByText("Different build");
    api.versions.mockRejectedValue({ message: "Version service unavailable." });
    fireEvent.click(screen.getByRole("button", { name: "Refresh versions" }));
    expect(await screen.findByText(/Showing the last successful check/)).toBeTruthy();
    expect(screen.getAllByText("Different build").length).toBeGreaterThan(0);
  });

  it("shows required protocols and separate build identities when running metadata is incomplete", async () => {
    const partial = structuredClone(snapshot);
    partial.components[1].running = { ...current, protocols: [], source_fingerprint: "running-build" };
    api.versions.mockResolvedValue(partial);
    render(<AboutPage {...props()} />);
    await expandGroups();
    const row = (await screen.findByText("ctld (SSH)")).closest("tbody")!;
    expect(within(row).getByTitle(/SSH and VPN broker/)).toBeTruthy();
    expect(within(row).getByLabelText("ctld IPC Not reported: Unverified").title).toContain("Requires 1.0.12");
    expect(within(row).getByTitle(/Build: running-build/).textContent).toContain("Build running-bu");
    expect(within(row).getByTitle(/Build: current/).textContent).toContain("Build current");
  });

  it("compares running protocols with app requirements even when the available binary is also stale", async () => {
    const stale = structuredClone(snapshot);
    stale.components[1].running = { ...current, protocols: [{ name: "ctld", build: 11, version: "1.0.11", supported_versions: ["1.0.11"] }] };
    stale.components[1].available = { ...current, protocols: [{ name: "ctld", build: 11, version: "1.0.11", supported_versions: ["1.0.11"] }] };
    stale.components[1].required_protocols = [{ name: "ctld", build: 12, version: "1.0.12", supported_versions: ["1.0.12"] }, { name: "ctld_lifecycle", build: 1, version: "1.0.1", supported_versions: ["1.0.1"] }];
    stale.components[1].status = "incompatible";
    api.versions.mockResolvedValue(stale);
    render(<AboutPage {...props()} />);
    await expandGroups();
    const row = (await screen.findByText("ctld (SSH)")).closest("tbody")!;
    const protocols = within(row).getByRole("list", { name: "Running + on disk protocols" });
    expect(within(protocols).getByLabelText("ctld IPC 1.0.11: Incompatible").title).toContain("Requires 1.0.12");
    expect(within(protocols).getByLabelText("Lifecycle Not reported: Unverified").title).toContain("Requires 1.0.1");
    expect(within(row).getByText("Protocol mismatch")).toBeTruthy();
  });

  it("rejects a late observation from before About was reopened", async () => {
    let complete!: (value: ComponentVersionsSnapshot) => void;
    api.versions.mockImplementationOnce(() => new Promise((resolve) => { complete = resolve; }));
    const options = props();
    const page = render(<AboutPage {...options} />);
    await waitFor(() => expect(api.versions).toHaveBeenCalledOnce());
    page.rerender(<AboutPage {...options} visible={false} />);
    page.rerender(<AboutPage {...options} />);
    await screen.findAllByText("Different build");
    const old = structuredClone(snapshot);
    old.components[1].status = "outdated";
    await act(async () => { complete(old); });
    expect(screen.getAllByText("Different build").length).toBeGreaterThan(0);
    expect(screen.queryByText("Outdated")).toBeNull();
  });

  it("does not present a late restart confirmation after leaving About", async () => {
    let complete!: (value: ComponentActionPreflight) => void;
    api.preflight.mockImplementationOnce(() => new Promise((resolve) => { complete = resolve; }));
    const options = props();
    const page = render(<AboutPage {...options} />);
    await expandGroups();
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
    const page = render(<AboutPage {...options} />);
    await expandGroups();
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
    render(<AboutPage {...options} />);
    await expandGroups();
    fireEvent.click(await screen.findByRole("button", { name: "Restart ctld (SSH)" }));
    const dialog = await screen.findByRole("dialog", { name: "Restart ctld (SSH)" });
    fireEvent.click(within(dialog).getByRole("button", { name: "Restart ctld" }));
    expect(await screen.findByText("ctld (SSH) restarted.")).toBeTruthy();
    expect(await screen.findByText(/Could not refresh versions: Probe timed out./)).toBeTruthy();
    expect(screen.getAllByText("Different build").length).toBeGreaterThan(0);
    expect(options.on_restarted).toHaveBeenCalledOnce();
  });

  it("does not lose completion when About is hidden during restart", async () => {
    let complete!: (value: unknown) => void;
    api.restart.mockReturnValue(new Promise((resolve) => { complete = resolve; }));
    const options = props();
    const page = render(<AboutPage {...options} />);
    await expandGroups();
    fireEvent.click(await screen.findByRole("button", { name: "Restart ctld (SSH)" }));
    const dialog = await screen.findByRole("dialog", { name: "Restart ctld (SSH)" });
    fireEvent.click(within(dialog).getByRole("button", { name: "Restart ctld" }));
    await waitFor(() => expect(api.restart).toHaveBeenCalledOnce());
    page.rerender(<AboutPage {...options} visible={false} />);
    await act(async () => { complete({ component_id: "owner-1", running: current }); });
    expect(options.on_restarted).toHaveBeenCalledOnce();
    expect(api.versions).toHaveBeenCalledTimes(2);
  });
});
