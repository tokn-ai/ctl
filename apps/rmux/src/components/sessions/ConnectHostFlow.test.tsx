// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { StrictMode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { SshConnectionTarget, SshPrompt, WorkspaceHost } from "../../lib/types";
import { cancelSshProbe, installRemoteAgent, probeSshHost, respondSshPrompt } from "../../lib/tauri";
import { hostTarget } from "../../features/workspace/workspaceModel";
import { ConnectHostFlow } from "./ConnectHostFlow";

vi.mock("../../lib/tauri", async (original) => ({
  ...(await original<object>()),
  probeSshHost: vi.fn(),
  cancelSshProbe: vi.fn(),
  respondSshPrompt: vi.fn(),
  installRemoteAgent: vi.fn(),
}));

const remoteInfo = { remote_id: "builder-account", agent_version: "0.1.0" };
const host: WorkspaceHost = {
  host_id: "builder",
  name: "Builder",
  preferred_method_id: "office",
  remote_info: remoteInfo,
  connection_methods: [
    { method_id: "home", name: "Home", target: { kind: "ssh", destination: "builder-home", hostname: "10.0.0.2" } },
    { method_id: "office", name: "Office", ssh_config_alias: "builder-office", target: { kind: "ssh", destination: "builder-office" } },
  ],
};
const target = hostTarget(host, [], "home") as SshConnectionTarget;

beforeEach(() => {
  vi.mocked(probeSshHost).mockReset().mockResolvedValue(remoteInfo);
  vi.mocked(cancelSshProbe).mockReset().mockResolvedValue(undefined);
  vi.mocked(respondSshPrompt).mockReset().mockResolvedValue(undefined);
  vi.mocked(installRemoteAgent).mockReset().mockResolvedValue({
    app_version: "0.1.0", bundle_id: "test", git_revision: "test", target_triple: "x86_64-unknown-linux-gnu",
  });
});
afterEach(cleanup);

function setup(overrides: Partial<Parameters<typeof ConnectHostFlow>[0]> = {}) {
  const onClose = vi.fn();
  const onConnected = vi.fn();
  const props = { suggestions: [], warning: null, target, host, onClose, onConnected, ...overrides };
  const view = render(<StrictMode><ConnectHostFlow {...props} /></StrictMode>);
  return { ...view, props, onClose, onConnected, user: userEvent.setup() };
}

describe("connection method selection", () => {
  it("starts the only saved method once without a confirmation", async () => {
    const onlyHost = { ...host, connection_methods: [host.connection_methods[1]] };
    const { onClose, onConnected } = setup({ host: onlyHost });
    expect(screen.getByRole("dialog", { name: "Connecting to host" })).toBeTruthy();
    expect(screen.queryByRole("option", { name: "Connect" })).toBeNull();
    await waitFor(() => expect(onClose).toHaveBeenCalledOnce());
    expect(probeSshHost).toHaveBeenCalledExactlyOnceWith(hostTarget(onlyHost, []), expect.any(String), expect.any(Function));
    expect(onConnected).toHaveBeenCalledExactlyOnceWith(hostTarget(onlyHost, []));
  });

  it("focuses the preferred method even when the last used route was different", async () => {
    const { user, onClose } = setup();
    expect(probeSshHost).not.toHaveBeenCalled();
    expect(document.activeElement).toBe(screen.getByRole("option", { name: /Office.*Preferred/ }));
    await user.keyboard("{Enter}");
    await waitFor(() => expect(onClose).toHaveBeenCalledOnce());
    expect(probeSshHost).toHaveBeenCalledExactlyOnceWith(hostTarget(host, []), expect.any(String), expect.any(Function));
  });

  it("connects an alternate immediately without changing the preferred method", async () => {
    const before = structuredClone(host);
    const { user, onClose } = setup();
    await user.click(screen.getByRole("option", { name: /Home/ }));
    await waitFor(() => expect(onClose).toHaveBeenCalledOnce());
    expect(probeSshHost).toHaveBeenCalledExactlyOnceWith(target, expect.any(String), expect.any(Function));
    expect(host).toEqual(before);
  });

  it("uses an explicitly selected method without showing another picker", async () => {
    const { onClose } = setup({ selected_method_id: "home" });
    expect(screen.getByRole("dialog", { name: "Connecting to host" })).toBeTruthy();
    await waitFor(() => expect(onClose).toHaveBeenCalledOnce());
    expect(probeSshHost).toHaveBeenCalledExactlyOnceWith(target, expect.any(String), expect.any(Function));
  });

  it("cancels method selection without contacting any host", async () => {
    const { user, onClose } = setup();
    await user.keyboard("{Escape}");
    expect(onClose).toHaveBeenCalledOnce();
    expect(probeSshHost).not.toHaveBeenCalled();
  });

  it.each([
    { host: { ...host, connection_methods: [] }, message: "no connection methods" },
    { selected_method_id: "removed", message: "no longer saved" },
    { host: { ...host, source: "unavailable" as const }, message: "Restore its saved definition" },
    { host: undefined, target: { ...target, unavailable: "The SSH alias was removed." }, message: "SSH alias was removed" },
    { host: { ...host, connection_methods: [{ ...host.connection_methods[0], target: { ...target, unavailable: "The device is offline." } }] }, message: "device is offline" },
    { host: { ...host, connection_methods: [{ ...host.connection_methods[0], target: { ...target, gateway_route: [{ gateway_id: "removed", mode: "automatic" as const }] } }] }, message: "gateway for this connection method is missing" },
  ])("blocks unavailable routes before a probe ($message)", async ({ message, ...overrides }) => {
    const { user, onClose } = setup(overrides);
    expect(screen.getByRole("alert").textContent).toContain(message);
    await act(async () => { await Promise.resolve(); });
    expect(probeSshHost).not.toHaveBeenCalled();
    await user.click(screen.getByRole("option", { name: "Close" }));
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("does not probe a method removed while its picker is open", async () => {
    const { rerender, props, user } = setup();
    rerender(<StrictMode><ConnectHostFlow {...props} host={{ ...host, connection_methods: [] }} /></StrictMode>);
    expect(screen.queryByRole("option", { name: /Office/ })).toBeNull();
    await user.keyboard("{Enter}");
    expect(probeSshHost).not.toHaveBeenCalled();
  });

  it("answers authentication and can cancel without accepting a late success", async () => {
    let showPrompt!: (prompt: SshPrompt) => void;
    let finish!: (identity: typeof remoteInfo) => void;
    vi.mocked(probeSshHost).mockImplementationOnce((_target, _attempt, prompt) => {
      showPrompt = prompt;
      return new Promise((resolve) => { finish = resolve; });
    });
    const { user, onClose, onConnected } = setup({ host: { ...host, connection_methods: [host.connection_methods[0]] } });
    await waitFor(() => expect(probeSshHost).toHaveBeenCalledOnce());
    await act(async () => showPrompt({ prompt_id: "password", kind: "secret", message: "Password:" }));
    await user.type(screen.getByLabelText("SSH response"), "secret{Enter}");
    expect(respondSshPrompt).toHaveBeenCalledWith(expect.any(String), "password", "secret");
    await user.keyboard("{Escape}");
    await act(async () => finish(remoteInfo));
    expect(cancelSshProbe).toHaveBeenCalledExactlyOnceWith(vi.mocked(probeSshHost).mock.calls[0][1]);
    expect(onClose).toHaveBeenCalledOnce();
    expect(onConnected).not.toHaveBeenCalled();
  });

  it("keeps the automatically selected method while a catalog refresh adds another route", async () => {
    let finish!: (identity: typeof remoteInfo) => void;
    vi.mocked(probeSshHost).mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    const onlyHost = { ...host, connection_methods: [host.connection_methods[0]] };
    const { rerender, props, onConnected } = setup({ host: onlyHost });
    await waitFor(() => expect(probeSshHost).toHaveBeenCalledOnce());
    rerender(<StrictMode><ConnectHostFlow {...props} host={host} /></StrictMode>);
    expect(screen.getByRole("dialog", { name: "Connecting to host" })).toBeTruthy();
    expect(cancelSshProbe).not.toHaveBeenCalled();
    await act(async () => finish(remoteInfo));
    expect(onConnected).toHaveBeenCalledExactlyOnceWith(target);
    expect(probeSshHost).toHaveBeenCalledOnce();
  });

  it("keeps a failed route selected for an explicit retry", async () => {
    vi.mocked(probeSshHost).mockRejectedValueOnce(new Error("Connection refused"));
    const { user, onClose } = setup();
    await user.click(screen.getByRole("option", { name: /Home/ }));
    expect((await screen.findByRole("alert")).textContent).toBe("Connection refused");
    expect(screen.queryByRole("option", { name: /Office/ })).toBeNull();
    await user.click(screen.getByRole("option", { name: "Connect" }));
    await waitFor(() => expect(onClose).toHaveBeenCalledOnce());
    expect(probeSshHost).toHaveBeenCalledTimes(2);
    expect(vi.mocked(probeSshHost).mock.calls.map(([candidate]) => candidate)).toEqual([target, target]);
  });

  it("preserves the exact running route for an explicit remote component update", async () => {
    const running = { ...target, hostname: "old-running-address" };
    const { user, onClose } = setup({ target: running, updateRequired: true });
    expect(screen.getByRole("dialog", { name: "Update remote components" })).toBeTruthy();
    expect(probeSshHost).not.toHaveBeenCalled();
    await user.click(screen.getByRole("option", { name: /Update remote components/ }));
    await waitFor(() => expect(onClose).toHaveBeenCalledOnce());
    expect(installRemoteAgent).toHaveBeenCalledExactlyOnceWith(running, expect.any(String), expect.any(Function), expect.any(Function));
    expect(probeSshHost).toHaveBeenCalledExactlyOnceWith(running, expect.any(String), expect.any(Function));
  });
});
