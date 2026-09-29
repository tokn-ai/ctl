// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ComponentVersionInfo, ComponentVersionsSnapshot, ComponentActionPreflight } from "../lib/types";
import { AboutPage } from "./AboutPage";

const api = vi.hoisted(() => ({ versions: vi.fn(), preflight: vi.fn(), restart: vi.fn() }));
vi.mock("../lib/tauri", () => ({ getComponentVersions: api.versions, preflightComponentAction: api.preflight, executeComponentAction: api.restart }));

const current: ComponentVersionInfo = { version: "0.1.0", source_revision: "abcdef123456", source_fingerprint: "current", dirty: false, protocols: [{ name: "ctld", version: 11 }] };
const snapshot: ComponentVersionsSnapshot = { components: [
  { component_id: "app", component: "rmux", label: "rmux", location: "local", host_id: null, observation: "bundled", status: "current", running: current, available: current, restart_supported: false, action: null, detail: null, error: null },
  { component_id: "owner-1", component: "ctld", label: "ctld", location: "local", host_id: null, observation: "running", status: "different_build", running: { ...current, source_revision: "112233445566" }, available: current, restart_supported: true, action: "restart", detail: "SSH and VPN broker", error: null },
  { component_id: "remote-1", component: "ctl_agent", label: "Development · ctl-agent", location: "remote", host_id: "dev", observation: "last_observed", status: "unknown", running: { ...current, source_revision: null, source_fingerprint: null }, available: current, restart_supported: false, action: null, detail: null, error: null },
] };
const preflight: ComponentActionPreflight = { action_token: "native-owner-token", component_id: "owner-1", component: "ctld", location: "local", host_id: null, action: "restart", label: "ctld", running: snapshot.components[1].running, available: current, impact: { ssh_connections: null, port_forwards: null, vpn_connections: 2, terminal_sessions: null, description: "Stops 2 managed VPN connections and may interrupt SSH connections and port forwards." } };
const props = () => ({ visible: true, on_close: vi.fn(), on_restarted: vi.fn(), on_dialog_change: vi.fn() });

beforeEach(() => {
  vi.clearAllMocks();
  api.versions.mockResolvedValue(structuredClone(snapshot));
  api.preflight.mockResolvedValue(structuredClone(preflight));
  api.restart.mockResolvedValue({ component_id: "owner-1", running: current });
});
afterEach(cleanup);

describe("About page", () => {
  it("offers native-approved actions per component and uses reconnect impact without a destructive label", async () => {
    const rows = structuredClone(snapshot);
    rows.components[2].action = "reconnect";
    rows.components[2].restart_supported = false;
    rows.components.push({ ...rows.components[1], component_id: "local-taskd", component: "taskd", label: "taskd" });
    api.versions.mockResolvedValue(rows);
    const reconnect = { ...preflight, component_id: "remote-1", component: "ctl_agent" as const, location: "remote" as const, host_id: "dev", action: "reconnect" as const, label: "Development · ctl-agent", impact: { ...preflight.impact, description: "Reconnects these terminal transports. Remote sessions keep running." } };
    api.preflight.mockResolvedValue(reconnect);
    const execute_action = vi.fn().mockResolvedValue({ detail: "Two terminal transports reconnected." });
    render(<AboutPage {...props()} execute_action={execute_action} />);
    expect(await screen.findByRole("button", { name: "Restart taskd" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Restart rmux" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Reconnect Development · ctl-agent" }));
    const dialog = await screen.findByRole("dialog", { name: "Reconnect Development · ctl-agent" });
    expect(within(dialog).getByText(/Remote sessions keep running/)).toBeTruthy();
    fireEvent.click(within(dialog).getByRole("button", { name: "Reconnect ctl-agent" }));
    await waitFor(() => expect(execute_action).toHaveBeenCalledWith(reconnect));
    expect(api.restart).not.toHaveBeenCalled();
    expect(await screen.findByText("Two terminal transports reconnected.")).toBeTruthy();
  });

  it("distinguishes unreported builds from incomplete verification without hiding protocol mismatch", async () => {
    const legacy = structuredClone(snapshot);
    legacy.components[1] = { ...legacy.components[1], component: "taskd", status: "unknown", running: { ...current, version: null, source_revision: null, source_fingerprint: null } };
    legacy.components[2].running = { ...current, source_fingerprint: null };
    api.versions.mockResolvedValue(legacy);
    render(<AboutPage {...props()} />);
    const legacy_row = (await screen.findByText("ctld")).closest("tr")!;
    expect(within(legacy_row).getByText("Build not reported")).toBeTruthy();
    expect(screen.getByText("Build unverified")).toBeTruthy();
    legacy.components[1].status = "incompatible";
    fireEvent.click(screen.getByRole("button", { name: "Refresh versions" }));
    expect(await screen.findByText("Protocol mismatch")).toBeTruthy();
  });

  it("checks only when opened, and distinguishes observed build metadata from current versions", async () => {
    const options = props();
    const page = render(<AboutPage {...options} visible={false} />);
    expect(api.versions).not.toHaveBeenCalled();
    page.rerender(<AboutPage {...options} />);
    expect(await screen.findByText("Different build")).toBeTruthy();
    expect(screen.getByTitle(/Last observed/).textContent).toBe("Development · ctl-agent");
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
    expect(await screen.findByText("ctld timed out.")).toBeTruthy();
    expect(screen.getByText("Development · ctl-agent")).toBeTruthy();
    expect(screen.getByText("Unavailable")).toBeTruthy();
  });

  it("preflights before confirming restart, then refreshes versions and connections", async () => {
    const options = props();
    render(<AboutPage {...options} />);
    fireEvent.click(await screen.findByRole("button", { name: "Restart ctld" }));
    const dialog = await screen.findByRole("dialog", { name: "Restart ctld" });
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
    fireEvent.click(await screen.findByRole("button", { name: "Restart ctld" }));
    const dialog = await screen.findByRole("dialog", { name: "Restart ctld" });
    fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(api.restart).not.toHaveBeenCalled();
    expect((screen.getByRole("button", { name: "Restart ctld" }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("shows preflight errors at their component without opening a confirmation", async () => {
    api.preflight.mockRejectedValue({ message: "Update ctld before it can restart cooperatively." });
    render(<AboutPage {...props()} />);
    fireEvent.click(await screen.findByRole("button", { name: "Restart ctld" }));
    expect(await screen.findByText("Update ctld before it can restart cooperatively.")).toBeTruthy();
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(api.restart).not.toHaveBeenCalled();
  });

  it("refreshes observations after restart failure and keeps the error visible", async () => {
    api.restart.mockRejectedValue({ message: "Replacement readiness timed out." });
    const options = props();
    render(<AboutPage {...options} />);
    fireEvent.click(await screen.findByRole("button", { name: "Restart ctld" }));
    const dialog = await screen.findByRole("dialog", { name: "Restart ctld" });
    fireEvent.click(within(dialog).getByRole("button", { name: "Restart ctld" }));
    expect(await screen.findByText("Replacement readiness timed out.")).toBeTruthy();
    await waitFor(() => expect(api.versions).toHaveBeenCalledTimes(2));
    expect(options.on_restarted).toHaveBeenCalledOnce();
  });

  it("retains the last successful snapshot when refresh fails", async () => {
    render(<AboutPage {...props()} />);
    await screen.findByText("Different build");
    api.versions.mockRejectedValue({ message: "Version service unavailable." });
    fireEvent.click(screen.getByRole("button", { name: "Refresh versions" }));
    expect(await screen.findByText(/Showing the last successful check/)).toBeTruthy();
    expect(screen.getByText("Different build")).toBeTruthy();
  });

  it("shows required protocols and separate build identities when running metadata is incomplete", async () => {
    const partial = structuredClone(snapshot);
    partial.components[1].running = { ...current, protocols: [], source_fingerprint: "running-build" };
    api.versions.mockResolvedValue(partial);
    render(<AboutPage {...props()} />);
    const row = (await screen.findByText("ctld")).closest("tr")!;
    expect(within(row).getByTitle(/SSH and VPN broker/)).toBeTruthy();
    expect(within(row).getByTitle("ctld IPC: not reported (requires 11)").textContent).toBe("IPC ?");
    expect(within(row).getByTitle(/Build: running-build/).textContent).toBe("0.1.0");
    expect(within(row).getByTitle(/Build: current/).textContent).toBe("0.1.0");
  });

  it("compares running protocols with app requirements even when the available binary is also stale", async () => {
    const stale = structuredClone(snapshot);
    stale.components[1].running = { ...current, protocols: [{ name: "ctld", version: 10 }] };
    stale.components[1].available = { ...current, protocols: [{ name: "ctld", version: 10 }] };
    stale.components[1].required_protocols = [{ name: "ctld", version: 11 }, { name: "ctld_lifecycle", version: 1 }];
    stale.components[1].status = "incompatible";
    api.versions.mockResolvedValue(stale);
    render(<AboutPage {...props()} />);
    const row = (await screen.findByText("ctld")).closest("tr")!;
    const protocols = within(row).getByText("IPC 10 · Lifecycle ?");
    expect(protocols.title).toContain("ctld IPC 10 (requires 11)");
    expect(protocols.title).toContain("Lifecycle: not reported (requires 1)");
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
    await screen.findByText("Different build");
    const old = structuredClone(snapshot);
    old.components[1].status = "outdated";
    await act(async () => { complete(old); });
    expect(screen.getByText("Different build")).toBeTruthy();
    expect(screen.queryByText("Outdated")).toBeNull();
  });

  it("does not present a late restart confirmation after leaving About", async () => {
    let complete!: (value: ComponentActionPreflight) => void;
    api.preflight.mockImplementationOnce(() => new Promise((resolve) => { complete = resolve; }));
    const options = props();
    const page = render(<AboutPage {...options} />);
    fireEvent.click(await screen.findByRole("button", { name: "Restart ctld" }));
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
    fireEvent.click(await screen.findByRole("button", { name: "Restart ctld" }));
    await waitFor(() => expect(api.preflight).toHaveBeenCalledOnce());
    page.rerender(<AboutPage {...options} visible={false} />);
    page.rerender(<AboutPage {...options} />);
    fireEvent.click(screen.getByRole("button", { name: "Restart ctld" }));
    const dialog = await screen.findByRole("dialog", { name: "Restart ctld" });
    await act(async () => { complete({ ...preflight, action_token: "abandoned-token" }); });
    fireEvent.click(within(dialog).getByRole("button", { name: "Restart ctld" }));
    await waitFor(() => expect(api.restart).toHaveBeenCalledWith("native-owner-token"));
  });

  it("reports a successful restart separately from a failed version refresh", async () => {
    api.versions.mockResolvedValueOnce(structuredClone(snapshot)).mockRejectedValueOnce(new Error("Probe timed out."));
    const options = props();
    render(<AboutPage {...options} />);
    fireEvent.click(await screen.findByRole("button", { name: "Restart ctld" }));
    const dialog = await screen.findByRole("dialog", { name: "Restart ctld" });
    fireEvent.click(within(dialog).getByRole("button", { name: "Restart ctld" }));
    expect(await screen.findByText("ctld restarted.")).toBeTruthy();
    expect(await screen.findByText(/Could not refresh versions: Probe timed out./)).toBeTruthy();
    expect(screen.getByText("Different build")).toBeTruthy();
    expect(options.on_restarted).toHaveBeenCalledOnce();
  });

  it("does not lose completion when About is hidden during restart", async () => {
    let complete!: (value: unknown) => void;
    api.restart.mockReturnValue(new Promise((resolve) => { complete = resolve; }));
    const options = props();
    const page = render(<AboutPage {...options} />);
    fireEvent.click(await screen.findByRole("button", { name: "Restart ctld" }));
    const dialog = await screen.findByRole("dialog", { name: "Restart ctld" });
    fireEvent.click(within(dialog).getByRole("button", { name: "Restart ctld" }));
    await waitFor(() => expect(api.restart).toHaveBeenCalledOnce());
    page.rerender(<AboutPage {...options} visible={false} />);
    await act(async () => { complete({ component_id: "owner-1", running: current }); });
    expect(options.on_restarted).toHaveBeenCalledOnce();
    expect(api.versions).toHaveBeenCalledTimes(2);
  });
});
