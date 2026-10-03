// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { StrictMode } from "react";
import { SshHostFlow } from "./SshHostFlow";
import { resolveVpnRouteStep } from "../../features/workspace/sshRoute";
import {
  cancelSshProbe,
  forgetSshCredentials,
  installRemoteAgent,
  listSshIdentityFiles,
  openVpnSignIn,
  probeSshHost,
  respondSshPrompt,
  saveSshConfigHost,
} from "../../lib/tauri";
import type { RemoteAgentInstallProgress, SshConnectionTarget, SshPrompt, TailscaleDevice, VpnConnection, WorkspaceHost } from "../../lib/types";

const remoteInfo = { remote_id: "ad6a8b53-bae0-45ce-8f09-5cb084a6c843", agent_version: "0.1.0" };
const vpn: VpnConnection = {
  connection_id: "office-vpn", name: "Office VPN", url: "https://vpn.example", username: "operator",
  has_password: true, auth_method: null, target_ip: null,
};
const tailscaleDevice: TailscaleDevice = {
  node_id: "n123", name: "Builder", dns_name: "builder.tailnet.ts.net",
  addresses: ["100.64.0.2"], online: true, os: "linux",
};
const installedBundle = { app_version: "0.1.0", bundle_id: "0.1.0-dev.0123456789ab",
  git_revision: "0123456789abcdef0123456789abcdef01234567", target_triple: "x86_64-unknown-linux-musl" };

function remoteVpnRecoveryFixture() {
  const edge = { kind: "ssh" as const, gateway_id: "edge", name: "Office edge", destination: "edge.example", mode: "native_only" as const };
  const remote_vpn = resolveVpnRouteStep({ vpn_connection_id: vpn.connection_id });
  const owner_info = { remote_id: "3c9e56f3-bae0-45ce-8f09-5cb084a6c843", agent_version: "0.0.1" };
  const jump = { kind: "ssh" as const, gateway_id: "host:jump:office", name: "Jump host", destination: "jump-alias",
    hostname: "10.0.0.7", user: "alice", mode: "automatic" as const, remote_info: owner_info };
  const target: SshConnectionTarget = { kind: "ssh", host_id: "build", host_name: "Build", destination: "build-alias",
    remote_info: { ...remoteInfo, agent_version: "0.0.1" }, vpn_connection_id: "local-vpn",
    gateway_route: [{ host_id: "removed-source", method_id: "removed-method", mode: "automatic" }],
    gateways: [edge, remote_vpn, jump, remote_vpn] };
  const owner: SshConnectionTarget = { kind: "ssh", destination: "jump-alias", ssh_config_alias: "jump-alias",
    hostname: "10.0.0.7", user: "alice", remote_info: owner_info, use_ssh_config_master: false,
    gateways: [resolveVpnRouteStep({ vpn_connection_id: "local-vpn" }), edge, remote_vpn] };
  const failure = { code: "remote_vpn_components_update_required", message: "Jump host requires updated remote VPN components.", vpn_route_index: 2 };
  return { target, owner, failure };
}

vi.mock("../../lib/tauri", () => ({
  probeSshHost: vi.fn(),
  cancelSshProbe: vi.fn(async () => undefined),
  respondSshPrompt: vi.fn(async () => undefined),
  forgetSshCredentials: vi.fn(async () => undefined),
  installRemoteAgent: vi.fn(),
  listSshIdentityFiles: vi.fn(),
  openVpnSignIn: vi.fn(async () => undefined),
  saveSshConfigHost: vi.fn(),
}));
afterEach(cleanup);
beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(listSshIdentityFiles).mockResolvedValue({
    identity_files: [],
    warnings: [],
  });
});

function setup() {
  const save = vi.fn(async () => undefined);
  const close = vi.fn();
  render(
    <StrictMode>
      <SshHostFlow
        suggestions={[]}
        warning={null}
        onVerified={async () => null}
        onSaveHost={save}
        onActivateHost={vi.fn(() => true)}
        onConnected={vi.fn()}
        onClose={close}
      />
    </StrictMode>,
  );
  return { save, close, user: userEvent.setup() };
}

async function details(user: ReturnType<typeof userEvent.setup>) {
  await user.type(
    screen.getByLabelText("SSH host"),
    "ctmux@127.0.0.1:2222{Enter}",
  );
  await user.clear(screen.getByLabelText("Name / SSH alias"));
  await user.type(
    screen.getByLabelText("Name / SSH alias"),
    "ctmux-test{Enter}",
  );
  await user.click(screen.getByRole("option", { name: /^Direct/ }));
}

function setupNewHost(suggestions: string[] = [], tailscaleDevices: TailscaleDevice[] = []) {
  const save = vi.fn(async (_name: string, _target: unknown, _remote_info: unknown) => undefined);
  const close = vi.fn();
  const recover = vi.fn();
  render(
    <StrictMode>
      <SshHostFlow suggestions={suggestions} tailscaleDevices={tailscaleDevices} warning={null}
        onSaveNewHost={save} onVerified={recover} onClose={close} />
    </StrictMode>,
  );
  return { save, close, recover, user: userEvent.setup() };
}

async function newHostDetails(user: ReturnType<typeof userEvent.setup>, selectDirect = true) {
  await user.type(screen.getByLabelText("SSH host"), "ctmux@127.0.0.1:2222{Enter}");
  await user.clear(screen.getByLabelText("Host name"));
  await user.type(screen.getByLabelText("Host name"), "Development server{Enter}");
  if (selectDirect) await user.click(screen.getByRole("option", { name: /^Direct/ }));
}

describe("SSH host quick-input flow", () => {
  it("verifies a component update without attaching a terminal or restarting sessions", async () => {
    vi.mocked(installRemoteAgent).mockResolvedValue(installedBundle);
    const target = { kind: "ssh" as const, host_id: "saved", destination: "example", remote_info: remoteInfo };
    const complete = vi.fn();
    const close = vi.fn();
    const connected = vi.fn();
    const verified = vi.fn();
    render(<SshHostFlow suggestions={[]} warning={null} target={target} component_mode="update"
      on_components_complete={complete} onConnected={connected} onVerified={verified} onClose={close} />);
    await userEvent.setup().click(screen.getByRole("option", { name: /Update remote components/ }));
    await waitFor(() => expect(complete).toHaveBeenCalledExactlyOnceWith(true));
    expect(installRemoteAgent).toHaveBeenCalledExactlyOnceWith(target, expect.any(String), expect.any(Function), expect.any(Function));
    expect(probeSshHost).not.toHaveBeenCalled();
    expect(connected).not.toHaveBeenCalled();
    expect(verified).not.toHaveBeenCalled();
    expect(close).toHaveBeenCalledOnce();
  });

  it("checking components uses the passive account probe without recovery or terminal callbacks", async () => {
    vi.mocked(probeSshHost).mockResolvedValue(remoteInfo);
    const target = { kind: "ssh" as const, destination: "example", remote_info: remoteInfo };
    const complete = vi.fn();
    const close = vi.fn();
    const verified = vi.fn();
    render(<StrictMode><SshHostFlow suggestions={[]} warning={null} target={target} component_mode="inspect" autoConnect
      on_components_complete={complete} onVerified={verified} onClose={close} /></StrictMode>);
    await waitFor(() => expect(complete).toHaveBeenCalledExactlyOnceWith(false));
    expect(probeSshHost).toHaveBeenCalledExactlyOnceWith(target, expect.any(String), expect.any(Function), true);
    expect(installRemoteAgent).not.toHaveBeenCalled();
    expect(verified).not.toHaveBeenCalled();
    expect(close).toHaveBeenCalledOnce();
  });

  it("checking after uncertain activation does not claim an update succeeded", async () => {
    vi.mocked(installRemoteAgent).mockRejectedValue({ code: "remote_install_verification_failed", message: "Installed, but could not verify activation." });
    vi.mocked(probeSshHost).mockResolvedValue(remoteInfo);
    const complete = vi.fn();
    render(<SshHostFlow suggestions={[]} warning={null} target={{ kind: "ssh", destination: "example" }} component_mode="update"
      on_components_complete={complete} onClose={vi.fn()} />);
    const user = userEvent.setup();
    await user.click(screen.getByRole("option", { name: /Update remote components/ }));
    await screen.findByText("Installed, but could not verify activation.");
    expect(complete).not.toHaveBeenCalled();
    await user.click(screen.getByRole("option", { name: "Check host" }));
    await waitFor(() => expect(complete).toHaveBeenCalledExactlyOnceWith(false));
    expect(installRemoteAgent).toHaveBeenCalledOnce();
  });

  it("defaults Connect through to Direct and preserves that choice when going back", async () => {
    vi.mocked(probeSshHost).mockResolvedValue(remoteInfo);
    const { user, save } = setupNewHost();
    await newHostDetails(user, false);
    expect(screen.getByRole("dialog", { name: "Connect through · 3/4" })).toBeTruthy();
    expect(screen.getByRole("option", { name: /^Direct/ }).getAttribute("aria-selected")).toBe("true");
    expect(document.activeElement).toBe(screen.getByRole("option", { name: /^Direct/ }));
    expect(probeSshHost).not.toHaveBeenCalled();
    await user.keyboard("{Enter}");
    await user.click(screen.getByRole("button", { name: "Previous step" }));
    expect(screen.getByRole("dialog", { name: "Connect through · 3/4" })).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Previous step" }));
    expect(screen.getByLabelText("Host name")).toHaveProperty("value", "Development server");
    await user.keyboard("{Enter}");
    await user.keyboard("{Enter}");
    await user.click(screen.getByRole("option", { name: /SSH config \/ agent/ }));
    await waitFor(() => expect(save).toHaveBeenCalledOnce());
    expect(save.mock.calls[0][1]).not.toHaveProperty("vpn_connection_id");
    expect(save.mock.calls[0][1]).not.toHaveProperty("gateway_route");
  });

  it("retains a selected VPN through backtracking and saves its stable profile ID", async () => {
    vi.mocked(probeSshHost).mockResolvedValue(remoteInfo);
    const save = vi.fn(async () => undefined);
    render(<SshHostFlow suggestions={[]} warning={null} vpn_connections={[vpn]}
      vpn_statuses={[{ vpn_id: vpn.connection_id, connection_id: vpn.connection_id, state: "connected", running: true,
        endpoint: "socks5h://127.0.0.1:49152", container_name: "sample" }]}
      onSaveNewHost={save} onClose={vi.fn()} />);
    const user = userEvent.setup();
    await newHostDetails(user, false);
    await user.click(screen.getByRole("option", { name: /Office VPN.*VPN · Connected/ }));
    expect(probeSshHost).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "Previous step" }));
    const selected = screen.getByRole("option", { name: /Office VPN.*VPN · Connected/ });
    expect(selected.getAttribute("aria-selected")).toBe("true");
    expect(document.activeElement).toBe(selected);
    await user.keyboard("{Enter}");
    await user.click(screen.getByRole("option", { name: /SSH config \/ agent/ }));
    await waitFor(() => expect(save).toHaveBeenCalledWith("Development server", expect.objectContaining({
      vpn_connection_id: vpn.connection_id, use_ssh_config_master: false,
    }), remoteInfo));
    const candidate = vi.mocked(probeSshHost).mock.calls[0][0];
    expect(candidate).not.toHaveProperty("gateways");
    expect(JSON.stringify(candidate)).not.toContain("49152");
    expect(saveSshConfigHost).not.toHaveBeenCalled();
  });

  it("replaces a VPN selection with a saved gateway and resolves it for verification", async () => {
    vi.mocked(probeSshHost).mockResolvedValue(remoteInfo);
    const gateway = { gateway_id: "edge", name: "Office edge", destination: "edge.example" };
    const save = vi.fn(async () => undefined);
    render(<SshHostFlow suggestions={[]} warning={null} vpn_connections={[vpn]} gateways={[gateway]}
      onSaveNewHost={save} onClose={vi.fn()} />);
    const user = userEvent.setup();
    await newHostDetails(user, false);
    await user.click(screen.getByRole("option", { name: /Office VPN/ }));
    await user.click(screen.getByRole("button", { name: "Previous step" }));
    await user.click(screen.getByRole("option", { name: /Office edge.*SSH gateway/ }));
    await user.click(screen.getByRole("option", { name: /SSH config \/ agent/ }));
    await waitFor(() => expect(save).toHaveBeenCalledWith("Development server", expect.objectContaining({
      gateway_route: [{ gateway_id: "edge", mode: "automatic" }],
      gateways: [{ ...gateway, mode: "automatic" }],
    }), remoteInfo));
    expect(vi.mocked(probeSshHost).mock.calls[0][0]).not.toHaveProperty("vpn_connection_id");
  });

  it("verifies and saves a linked host method with its inherited gateway route", async () => {
    vi.mocked(probeSshHost).mockResolvedValue(remoteInfo);
    const gateway = { gateway_id: "edge", name: "Office edge", destination: "edge.example" };
    const host: WorkspaceHost = { host_id: "jump", name: "Jump host", preferred_method_id: "office", remote_info: remoteInfo,
      connection_methods: [{ method_id: "office", name: "Office network", target: { kind: "ssh", destination: "jump.internal", user: "alice",
        gateway_route: [{ gateway_id: "edge", mode: "native_only" }] } }] };
    const save = vi.fn(async () => undefined);
    render(<SshHostFlow suggestions={[]} warning={null} hosts={[host]} gateways={[gateway]}
      onSaveNewHost={save} onClose={vi.fn()} />);
    const user = userEvent.setup();
    await newHostDetails(user, false);
    await user.click(screen.getByRole("option", { name: /Jump host.*Office network/ }));
    await user.click(screen.getByRole("button", { name: "Previous step" }));
    expect(screen.getByRole("option", { name: /Jump host.*Office network/ }).getAttribute("aria-selected")).toBe("true");
    await user.keyboard("{Enter}");
    await user.click(screen.getByRole("option", { name: /SSH config \/ agent/ }));
    await waitFor(() => expect(save).toHaveBeenCalledWith("Development server", expect.objectContaining({
      gateway_route: [{ host_id: "jump", method_id: "office", mode: "automatic" }],
      gateways: [{ ...gateway, mode: "native_only" }, { kind: "ssh", gateway_id: "host:jump:office", name: "Jump host",
        destination: "jump.internal", user: "alice", remote_info: remoteInfo, mode: "automatic" }],
    }), remoteInfo));
  });

  it("excludes the edited host from its route picker even when the initial target has no host ID", () => {
    const host: WorkspaceHost = { host_id: "build", name: "Build", preferred_method_id: "default",
      connection_methods: [{ method_id: "default", name: "SSH", target: { kind: "ssh", destination: "build.internal" } }] };
    render(<SshHostFlow suggestions={[]} warning={null} hosts={[host]} editing_host_id="build"
      initialTarget={{ kind: "ssh", destination: "build.internal" }} onSaveConnection={vi.fn()} onClose={vi.fn()} />);
    expect(screen.queryByRole("button", { name: "Add Build as hop" })).toBeNull();
  });

  it("requires a private master when a linked hop overrides its SSH alias hostname", () => {
    const host: WorkspaceHost = { host_id: "jump", name: "Jump host", preferred_method_id: "default",
      connection_methods: [{ method_id: "default", name: "SSH", ssh_config_alias: "jump",
        target: { kind: "ssh", destination: "jump", hostname: "10.0.0.10" } }] };
    render(<SshHostFlow suggestions={["build", "jump"]} warning={null} hosts={[host]}
      initialTarget={{ kind: "ssh", destination: "build", ssh_config_alias: "build", use_ssh_config_master: true,
        gateway_route: [{ host_id: "jump", method_id: "default", mode: "automatic" }] }}
      onSaveConnection={vi.fn()} onClose={vi.fn()} />);
    expect(screen.getByLabelText("Use SSH-config master")).toHaveProperty("disabled", true);
    expect(screen.getByLabelText("Use SSH-config master")).toHaveProperty("checked", false);
    expect(screen.getByText(/hostname override routes use a private SSH master/)).toBeTruthy();
  });

  it("builds a new host route through SSH and a VPN on that jump host", async () => {
    vi.mocked(probeSshHost).mockResolvedValue(remoteInfo);
    const save = vi.fn(async () => undefined);
    render(<SshHostFlow suggestions={[]} warning={null} vpn_connections={[vpn]}
      onSaveNewHost={save} onClose={vi.fn()} />);
    const user = userEvent.setup();
    await newHostDetails(user, false);
    await user.click(screen.getByRole("option", { name: /Build connection route/ }));
    await user.click(screen.getByRole("button", { name: "+ New gateway" }));
    await user.type(screen.getByLabelText("Name"), "Bastion");
    await user.type(screen.getByLabelText("SSH destination / alias"), "bastion.example");
    await user.click(screen.getByRole("button", { name: "Save gateway" }));
    await user.click(screen.getByRole("button", { name: "Add Office VPN to route" }));
    expect(screen.getByText("VPN · Runs on Bastion")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Connect" }));
    await waitFor(() => expect(save).toHaveBeenCalledOnce());
    const [name, candidate, remote_info, gateways] = save.mock.calls[0] as unknown as [string, SshConnectionTarget, typeof remoteInfo, Array<{ gateway_id: string; destination: string }>];
    expect(name).toBe("Development server");
    expect(remote_info).toEqual(remoteInfo);
    expect(gateways).toMatchObject([{ name: "Bastion", destination: "bastion.example" }]);
    expect(candidate.gateway_route).toEqual([
      { gateway_id: gateways[0].gateway_id, mode: "automatic" }, { vpn_connection_id: vpn.connection_id },
    ]);
    expect(candidate.gateways?.map((gateway) => gateway.kind ?? "ssh")).toEqual(["ssh", "vpn"]);
    expect(candidate.vpn_connection_id).toBeUndefined();
  });

  it("retains a discovered provider binding when building an ordered route", async () => {
    vi.mocked(probeSshHost).mockResolvedValue(remoteInfo);
    const gateway = { gateway_id: "edge", name: "Office edge", destination: "edge.example" };
    const save = vi.fn(async () => undefined);
    render(<SshHostFlow suggestions={[]} warning={null} tailscaleDevices={[tailscaleDevice]}
      vpn_connections={[vpn]} gateways={[gateway]} onSaveNewHost={save} onClose={vi.fn()} />);
    const user = userEvent.setup();
    await user.click(screen.getByRole("option", { name: /Builder/ }));
    await user.keyboard("{Enter}");
    await user.type(screen.getByLabelText("SSH user"), "operator{Enter}");
    await user.click(screen.getByRole("option", { name: /Build connection route/ }));
    await user.click(screen.getByRole("button", { name: "Add" }));
    await user.click(screen.getByRole("button", { name: "Add Office VPN to route" }));
    await user.click(screen.getByRole("button", { name: "Connect" }));
    await waitFor(() => expect(save).toHaveBeenCalledOnce());
    expect(save).toHaveBeenCalledWith("Builder", expect.objectContaining({
      tailscale_node_id: tailscaleDevice.node_id, hostname: "100.64.0.2", user: "operator",
      gateway_route: [{ gateway_id: "edge", mode: "automatic" }, { vpn_connection_id: vpn.connection_id }],
    }), remoteInfo);
  });

  it("shows VPN failures without attempting a direct connection", async () => {
    vi.mocked(probeSshHost).mockRejectedValue({ code: "vpn_failed", message: "Could not connect Office VPN" });
    const save = vi.fn();
    render(<SshHostFlow suggestions={[]} warning={null} vpn_connections={[vpn]} onSaveNewHost={save} onClose={vi.fn()} />);
    const user = userEvent.setup();
    await newHostDetails(user, false);
    await user.click(screen.getByRole("option", { name: /Office VPN/ }));
    await user.click(screen.getByRole("option", { name: /SSH config \/ agent/ }));
    expect(await screen.findByRole("alert")).toHaveProperty("textContent", "Could not connect Office VPN");
    expect(probeSshHost).toHaveBeenCalledOnce();
    expect(probeSshHost).toHaveBeenCalledWith(expect.objectContaining({ vpn_connection_id: vpn.connection_id }), expect.any(String), expect.any(Function));
    expect(save).not.toHaveBeenCalled();
  });

  it("signs in to a selected Tailscale VPN and retries with the same stable route", async () => {
    vi.mocked(probeSshHost).mockRejectedValueOnce({ code: "vpn_sign_in_required", message: "Sign in to this VPN to continue." }).mockResolvedValueOnce(remoteInfo);
    const tailscale: VpnConnection = { provider: "tailscale", connection_id: "tailnet", name: "Tailnet", hostname: null, accept_routes: false };
    const save = vi.fn();
    render(<SshHostFlow suggestions={[]} warning={null} vpn_connections={[tailscale]} onSaveNewHost={save} onClose={vi.fn()} />);
    const user = userEvent.setup();
    await newHostDetails(user, false);
    await user.click(screen.getByRole("option", { name: /Tailnet.*Tailscale/ }));
    await user.click(screen.getByRole("option", { name: /SSH config \/ agent/ }));
    await user.click(await screen.findByRole("option", { name: "Sign in to Tailscale" }));
    expect(openVpnSignIn).toHaveBeenCalledExactlyOnceWith("tailnet");
    expect(probeSshHost).toHaveBeenCalledOnce();
    expect(save).not.toHaveBeenCalled();
    await user.click(screen.getByRole("option", { name: "Connect" }));
    await waitFor(() => expect(save).toHaveBeenCalledWith("Development server", expect.objectContaining({ vpn_connection_id: "tailnet" }), remoteInfo));
    expect(vi.mocked(probeSshHost).mock.calls[1][0]).toEqual(vi.mocked(probeSshHost).mock.calls[0][0]);
  });

  it("edits a connection through a VPN without exporting an unusable SSH alias", async () => {
    vi.mocked(probeSshHost).mockResolvedValue(remoteInfo);
    const save = vi.fn(async () => undefined);
    render(<SshHostFlow suggestions={[]} warning={null} vpn_connections={[vpn]} onSaveConnection={save} onClose={vi.fn()} />);
    const user = userEvent.setup();
    await user.type(screen.getByLabelText("SSH host or config alias"), "build.example");
    await user.click(screen.getByLabelText("Use SSH-config master"));
    await user.click(screen.getByLabelText("Also save to OpenSSH config"));
    await user.selectOptions(screen.getByLabelText("Connect through"), `vpn:${vpn.connection_id}`);
    expect(screen.getByLabelText("Use SSH-config master")).toHaveProperty("disabled", true);
    expect(screen.getByLabelText("Use SSH-config master")).toHaveProperty("checked", false);
    expect(screen.getByLabelText("Also save to OpenSSH config")).toHaveProperty("disabled", true);
    await user.click(screen.getByRole("button", { name: "Verify and save" }));
    await waitFor(() => expect(save).toHaveBeenCalledWith(expect.objectContaining({ vpn_connection_id: vpn.connection_id, use_ssh_config_master: false }), [], remoteInfo));
    expect(saveSshConfigHost).not.toHaveBeenCalled();
  });

  it("removes a saved VPN when editing its method back to Direct", async () => {
    vi.mocked(probeSshHost).mockResolvedValue(remoteInfo);
    const save = vi.fn(async () => undefined);
    render(<SshHostFlow suggestions={["build"]} warning={null} vpn_connections={[vpn]}
      initialTarget={{ kind: "ssh", destination: "build", ssh_config_alias: "build", vpn_connection_id: vpn.connection_id }}
      onSaveConnection={save} onClose={vi.fn()} />);
    const user = userEvent.setup();
    expect(screen.getByLabelText("Connect through")).toHaveProperty("value", `vpn:${vpn.connection_id}`);
    await user.selectOptions(screen.getByLabelText("Connect through"), "direct");
    expect(screen.getByLabelText("Use SSH-config master")).toHaveProperty("disabled", false);
    expect(screen.getByLabelText("Use SSH-config master")).toHaveProperty("checked", true);
    await user.click(screen.getByRole("button", { name: "Verify and save" }));
    await waitFor(() => expect(save).toHaveBeenCalledOnce());
    expect(vi.mocked(probeSshHost).mock.calls[0][0]).not.toHaveProperty("vpn_connection_id");
  });

  it.each([false, true])("chooses the SSH account before verifying a new Tailscale target (autoConnect=%s)", async (autoConnect) => {
    const target = {
      kind: "ssh" as const, host_id: "tailscale:n123", host_name: "Builder", method_id: "tailscale", tailscale_node_id: "n123",
      destination: "builder.tailnet.ts.net", hostname: "100.64.0.2",
    };
    const onConnected = vi.fn();
    const onVerified = vi.fn(async () => null);
    vi.mocked(probeSshHost).mockResolvedValue(remoteInfo);
    render(<StrictMode><SshHostFlow suggestions={[]} warning={null} target={target} autoConnect={autoConnect}
      onVerified={onVerified} onConnected={onConnected} onClose={vi.fn()} /></StrictMode>);
    await act(async () => { await Promise.resolve(); });
    expect(screen.getByRole("textbox", { name: "SSH user" })).toHaveProperty("value", "");
    expect(screen.getByText(/Choosing an account saves this host customization/)).toBeTruthy();
    expect(probeSshHost).not.toHaveBeenCalled();
    const user = userEvent.setup();
    await user.type(screen.getByRole("textbox", { name: "SSH user" }), "deploy");
    expect(probeSshHost).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "Connect" }));
    const candidate = { ...target, user: "deploy" };
    await waitFor(() => expect(onConnected).toHaveBeenCalledExactlyOnceWith(candidate));
    expect(probeSshHost).toHaveBeenCalledExactlyOnceWith(candidate, expect.any(String), expect.any(Function));
    expect(onVerified).toHaveBeenCalledExactlyOnceWith(candidate, remoteInfo);
    expect(saveSshConfigHost).not.toHaveBeenCalled();
  });

  it("allows the SSH default for a new Tailscale target without persisting an explicit account", async () => {
    const target = { kind: "ssh" as const, host_id: "tailscale:n123", tailscale_node_id: "n123", destination: "builder.tailnet.ts.net" };
    const onVerified = vi.fn(async () => null);
    vi.mocked(probeSshHost).mockResolvedValue(remoteInfo);
    render(<SshHostFlow suggestions={[]} warning={null} target={target} autoConnect
      onVerified={onVerified} onClose={vi.fn()} />);
    expect(probeSshHost).not.toHaveBeenCalled();
    await userEvent.setup().click(screen.getByRole("button", { name: "Connect" }));
    await waitFor(() => expect(onVerified).toHaveBeenCalledExactlyOnceWith(target, remoteInfo));
    expect(probeSshHost).toHaveBeenCalledExactlyOnceWith(target, expect.any(String), expect.any(Function));
  });

  it.each([{ user: "deploy" }, { remote_info: remoteInfo }])("reuses a known Tailscale account without asking again (%j)", async (known) => {
    const target = { kind: "ssh" as const, tailscale_node_id: "n123", destination: "builder.tailnet.ts.net", ...known };
    vi.mocked(probeSshHost).mockResolvedValue(remoteInfo);
    render(<StrictMode><SshHostFlow suggestions={[]} warning={null} target={target} autoConnect onClose={vi.fn()} /></StrictMode>);
    expect(screen.queryByRole("textbox", { name: "SSH user" })).toBeNull();
    await waitFor(() => expect(probeSshHost).toHaveBeenCalledExactlyOnceWith(target, expect.any(String), expect.any(Function)));
  });

  it("can correct the chosen Tailscale account after failed verification", async () => {
    const target = { kind: "ssh" as const, tailscale_node_id: "n123", destination: "builder.tailnet.ts.net" };
    vi.mocked(probeSshHost).mockRejectedValueOnce(new Error("Unknown SSH user")).mockResolvedValueOnce(remoteInfo);
    render(<SshHostFlow suggestions={[]} warning={null} target={target} autoConnect onClose={vi.fn()} />);
    const user = userEvent.setup();
    await user.type(screen.getByRole("textbox", { name: "SSH user" }), "wrong{Enter}");
    expect(await screen.findByText("Unknown SSH user")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Previous step" }));
    expect(screen.getByRole("textbox", { name: "SSH user" })).toHaveProperty("value", "wrong");
    await user.clear(screen.getByRole("textbox", { name: "SSH user" }));
    await user.type(screen.getByRole("textbox", { name: "SSH user" }), "deploy{Enter}");
    await waitFor(() => expect(probeSshHost).toHaveBeenCalledTimes(2));
    expect(probeSshHost).toHaveBeenNthCalledWith(2, { ...target, user: "deploy" }, expect.any(String), expect.any(Function));
  });

  it("automatically verifies a selected target once in StrictMode and returns the verified target", async () => {
    const target = { kind: "ssh" as const, host_id: "build", destination: "build-alias", method_id: "ssh_config" };
    const verified = { ...target, remote_info: remoteInfo };
    const onVerified = vi.fn(async () => verified);
    const onConnected = vi.fn();
    const onClose = vi.fn();
    vi.mocked(probeSshHost).mockResolvedValue(remoteInfo);
    render(<StrictMode><SshHostFlow suggestions={[]} warning={null} target={target} autoConnect
      onVerified={onVerified} onConnected={onConnected} onClose={onClose} /></StrictMode>);
    expect(screen.getByRole("dialog", { name: "Connecting to host" })).toBeTruthy();
    expect(screen.queryByRole("option", { name: "Connect" })).toBeNull();
    await waitFor(() => expect(onConnected).toHaveBeenCalledExactlyOnceWith(verified));
    expect(probeSshHost).toHaveBeenCalledExactlyOnceWith(target, expect.any(String), expect.any(Function));
    expect(onVerified).toHaveBeenCalledExactlyOnceWith(target, remoteInfo);
    expect(onClose).toHaveBeenCalledOnce();
    expect(cancelSshProbe).not.toHaveBeenCalled();
  });

  it("answers automatic connection credentials before continuing with the verified target", async () => {
    const target = { kind: "ssh" as const, host_id: "build", destination: "build-alias" };
    const verified = { ...target, remote_info: remoteInfo };
    const onConnected = vi.fn();
    const onVerified = vi.fn(async () => verified);
    let showPrompt!: (value: SshPrompt) => void;
    let finish!: (value: typeof remoteInfo) => void;
    vi.mocked(probeSshHost).mockImplementationOnce((_target, _attempt, prompt) => {
      showPrompt = prompt;
      return new Promise((resolve) => { finish = resolve; });
    });
    render(<SshHostFlow suggestions={[]} warning={null} target={target} autoConnect
      onVerified={onVerified} onConnected={onConnected} onClose={vi.fn()} />);
    await waitFor(() => expect(probeSshHost).toHaveBeenCalledOnce());
    await act(async () => showPrompt({ prompt_id: "password", kind: "secret", message: "Password:" }));
    const input = screen.getByLabelText("SSH response");
    expect(input.getAttribute("type")).toBe("password");
    await userEvent.setup().type(input, "test-secret{Enter}");
    expect(respondSshPrompt).toHaveBeenCalledExactlyOnceWith(
      vi.mocked(probeSshHost).mock.calls[0][1], "password", "test-secret",
    );
    expect(onVerified).not.toHaveBeenCalled();
    expect(onConnected).not.toHaveBeenCalled();
    await act(async () => finish(remoteInfo));
    expect(onConnected).toHaveBeenCalledExactlyOnceWith(verified);
  });

  it("cancels an automatic connection and ignores its late prompt and successful verification", async () => {
    const target = { kind: "ssh" as const, host_id: "build", destination: "build-alias" };
    const onVerified = vi.fn();
    const onConnected = vi.fn();
    const onClose = vi.fn();
    let showPrompt!: (value: SshPrompt) => void;
    let finish!: (value: typeof remoteInfo) => void;
    vi.mocked(probeSshHost).mockImplementationOnce((_target, _attempt, prompt) => {
      showPrompt = prompt;
      return new Promise((resolve) => { finish = resolve; });
    });
    const { unmount } = render(<StrictMode><SshHostFlow suggestions={[]} warning={null} target={target} autoConnect
      onVerified={onVerified} onConnected={onConnected} onClose={onClose} /></StrictMode>);
    await waitFor(() => expect(probeSshHost).toHaveBeenCalledOnce());
    await userEvent.setup().keyboard("{Escape}");
    expect(cancelSshProbe).toHaveBeenCalledExactlyOnceWith(vi.mocked(probeSshHost).mock.calls[0][1]);
    await act(async () => {
      showPrompt({ prompt_id: "late", kind: "secret", message: "Late password:" });
      finish(remoteInfo);
    });
    expect(screen.queryByText("Late password:")).toBeNull();
    expect(onVerified).not.toHaveBeenCalled();
    expect(onConnected).not.toHaveBeenCalled();
    expect(onClose).toHaveBeenCalledOnce();
    unmount();
    expect(forgetSshCredentials).not.toHaveBeenCalled();
  });

  it("keeps explicit component updates unchanged when automatic connection is requested", async () => {
    render(<SshHostFlow suggestions={[]} warning={null}
      target={{ kind: "ssh", destination: "build-alias" }} autoConnect updateRequired onClose={vi.fn()} />);
    await act(async () => { await Promise.resolve(); });
    expect(screen.getByRole("dialog", { name: "Update remote components" })).toBeTruthy();
    expect(screen.getByRole("option", { name: /Update remote components/ })).toBeTruthy();
    expect(probeSshHost).not.toHaveBeenCalled();
  });

  it("adds an address, display name, and credentials before automatically saving the verified host", async () => {
    let finishProbe!: (value: typeof remoteInfo) => void;
    vi.mocked(probeSshHost).mockImplementationOnce(() => new Promise((resolve) => { finishProbe = resolve; }));
    const { user, save, close, recover } = setupNewHost();
    await newHostDetails(user);
    expect(probeSshHost).not.toHaveBeenCalled();
    await user.click(screen.getByRole("option", { name: /SSH config \/ agent/ }));
    expect(probeSshHost).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({
      destination: "127.0.0.1", hostname: "127.0.0.1", user: "ctmux", port: 2222,
    }), expect.any(String), expect.any(Function));
    expect(save).not.toHaveBeenCalled();
    await act(async () => finishProbe(remoteInfo));
    await waitFor(() => expect(save).toHaveBeenCalledExactlyOnceWith("Development server", expect.objectContaining({
      destination: "127.0.0.1", hostname: "127.0.0.1", user: "ctmux", port: 2222,
    }), remoteInfo));
    expect(close).toHaveBeenCalledOnce();
    expect(recover).not.toHaveBeenCalled();
    expect(screen.queryByRole("dialog", { name: "Save host" })).toBeNull();
    expect(saveSshConfigHost).not.toHaveBeenCalled();
  });

  it("takes a selected SSH alias through naming and credentials while preserving its transport settings", async () => {
    vi.mocked(probeSshHost).mockResolvedValue(remoteInfo);
    const { user, save } = setupNewHost(["build-alias"]);
    expect(screen.getByRole("group", { name: "SSH config · Virtual" })).toBeTruthy();
    await user.click(screen.getByRole("option", { name: "build-alias" }));
    expect(screen.getByLabelText("Host name")).toHaveProperty("value", "build-alias");
    expect(probeSshHost).not.toHaveBeenCalled();
    await user.clear(screen.getByLabelText("Host name"));
    await user.type(screen.getByLabelText("Host name"), "Office build machine{Enter}");
    await user.click(screen.getByRole("option", { name: /^Direct/ }));
    await user.click(screen.getByRole("option", { name: /Identity file/ }));
    await user.type(screen.getByRole("combobox", { name: "Identity file" }), "~/.ssh/office{Enter}");
    await waitFor(() => expect(save).toHaveBeenCalledExactlyOnceWith("Office build machine", {
      kind: "ssh", destination: "build-alias", identity_file: "~/.ssh/office", ssh_config_alias: "build-alias",
    }, remoteInfo));
    expect(saveSshConfigHost).not.toHaveBeenCalled();
  });

  it("names a virtual Tailscale device before authentication and retains its binding through key selection and retries", async () => {
    vi.mocked(probeSshHost).mockRejectedValueOnce(new Error("Key unavailable")).mockResolvedValueOnce(remoteInfo);
    const { user, save, close } = setupNewHost(["office"], [tailscaleDevice]);
    expect(screen.getByRole("group", { name: "SSH config · Virtual" })).toBeTruthy();
    expect(screen.getByRole("group", { name: "Tailscale · Virtual" })).toBeTruthy();
    expect(probeSshHost).not.toHaveBeenCalled();
    await user.click(screen.getByRole("option", { name: /Builder.*Online · linux · builder.tailnet.ts.net/ }));
    expect(screen.getByLabelText("Host name")).toHaveProperty("value", "Builder");
    await user.clear(screen.getByLabelText("Host name"));
    await user.type(screen.getByLabelText("Host name"), "Home builder{Enter}");
    expect(probeSshHost).not.toHaveBeenCalled();
    expect(screen.getByRole("textbox", { name: "SSH user" })).toHaveProperty("value", "");
    await user.type(screen.getByRole("textbox", { name: "SSH user" }), "deploy{Enter}");
    await user.click(screen.getByRole("option", { name: /^Direct/ }));
    expect(probeSshHost).not.toHaveBeenCalled();
    await user.click(screen.getByRole("option", { name: /Identity file/ }));
    await user.type(screen.getByRole("combobox", { name: "Identity file" }), "~/.ssh/home{Enter}");
    expect(await screen.findByText("Key unavailable")).toBeTruthy();
    const candidate = {
      kind: "ssh", host_id: "tailscale:n123", host_name: "Builder", method_id: "tailscale", tailscale_node_id: "n123",
      destination: "builder.tailnet.ts.net", hostname: "100.64.0.2", identity_file: "~/.ssh/home",
      user: "deploy",
    };
    expect(probeSshHost).toHaveBeenNthCalledWith(1, candidate, expect.any(String), expect.any(Function));
    expect(save).not.toHaveBeenCalled();
    await user.click(screen.getByRole("option", { name: "Connect" }));
    await waitFor(() => expect(save).toHaveBeenCalledExactlyOnceWith("Home builder", candidate, remoteInfo));
    expect(probeSshHost).toHaveBeenNthCalledWith(2, candidate, expect.any(String), expect.any(Function));
    expect(close).toHaveBeenCalledOnce();
    expect(saveSshConfigHost).not.toHaveBeenCalled();
  });

  it("clears a selected provider binding when replacing it with a manually entered host", async () => {
    vi.mocked(probeSshHost).mockResolvedValue(remoteInfo);
    const { user, save } = setupNewHost([], [tailscaleDevice]);
    await user.click(screen.getByRole("option", { name: /Builder/ }));
    await user.click(screen.getByRole("button", { name: "Previous step" }));
    await user.clear(screen.getByLabelText("SSH host"));
    await user.type(screen.getByLabelText("SSH host"), "deploy@10.0.0.8{Enter}");
    await user.type(screen.getByLabelText("Host name"), "{Enter}");
    await user.click(screen.getByRole("option", { name: /^Direct/ }));
    await user.click(screen.getByRole("option", { name: /SSH config \/ agent/ }));
    await waitFor(() => expect(save).toHaveBeenCalledOnce());
    expect(save.mock.calls[0][1]).toEqual({ kind: "ssh", destination: "10.0.0.8", hostname: "10.0.0.8", user: "deploy" });
    expect(saveSshConfigHost).not.toHaveBeenCalled();
  });

  it("keeps manual host input and SSH config available while hiding stale Tailscale devices during discovery", async () => {
    render(<SshHostFlow suggestions={["office"]} tailscaleDevices={[tailscaleDevice]} discoveryLoading warning={null}
      onSaveNewHost={vi.fn()} onClose={vi.fn()} />);
    expect(screen.getByRole("status").textContent).toContain("Discovering hosts");
    expect(screen.getByRole("option", { name: "office" })).toBeTruthy();
    expect(screen.queryByRole("group", { name: "Tailscale · Virtual" })).toBeNull();
    expect(screen.queryByRole("option", { name: /Builder/ })).toBeNull();
    await userEvent.setup().type(screen.getByLabelText("SSH host"), "10.0.0.8{Enter}");
    expect(screen.getByLabelText("Host name")).toHaveProperty("value", "10.0.0.8");
    expect(probeSshHost).not.toHaveBeenCalled();
  });

  it("only offers online Tailscale devices after discovery completes", () => {
    const devices: TailscaleDevice[] = [
      tailscaleDevice,
      { ...tailscaleDevice, node_id: "offline", name: "Offline builder", online: false },
      { ...tailscaleDevice, node_id: "unknown", name: "Unknown builder", online: null },
    ];
    const props = { suggestions: [], tailscaleDevices: devices, warning: null, onSaveNewHost: vi.fn(), onClose: vi.fn() };
    const { rerender } = render(<SshHostFlow {...props} discoveryLoading />);
    expect(screen.queryByRole("group", { name: "Tailscale · Virtual" })).toBeNull();
    rerender(<SshHostFlow {...props} discoveryLoading={false} />);
    expect(screen.getByRole("option", { name: /Builder.*Online/ })).toBeTruthy();
    expect(screen.queryByRole("option", { name: /Offline builder/ })).toBeNull();
    expect(screen.queryByRole("option", { name: /Unknown builder/ })).toBeNull();
  });

  it.each([false, true])("rejects a stale virtual device selection while discoveryLoading=%s", async (discoveryLoading) => {
    render(<SshHostFlow suggestions={[]} tailscaleDevices={[{ ...tailscaleDevice, online: false }]} discoveryLoading={discoveryLoading}
      warning={null} onSaveNewHost={vi.fn()} onClose={vi.fn()} />);
    await userEvent.setup().type(screen.getByLabelText("SSH host"), "tailscale:n123{Enter}");
    expect(screen.getByText(discoveryLoading
      ? "Wait for Tailscale discovery to finish before choosing a device."
      : "This Tailscale device is no longer online. Choose another host.")).toBeTruthy();
    expect(screen.queryByLabelText("Host name")).toBeNull();
    expect(probeSshHost).not.toHaveBeenCalled();
  });

  it("retries verification with the same named host after a connection failure", async () => {
    vi.mocked(probeSshHost).mockRejectedValueOnce(new Error("SSH unavailable")).mockResolvedValueOnce(remoteInfo);
    const { user, save, close } = setupNewHost();
    await newHostDetails(user);
    await user.click(screen.getByRole("option", { name: /Password \/ interactive/ }));
    expect(await screen.findByText("SSH unavailable")).toBeTruthy();
    expect(save).not.toHaveBeenCalled();
    expect(close).not.toHaveBeenCalled();
    await user.click(screen.getByRole("option", { name: "Connect" }));
    await waitFor(() => expect(save).toHaveBeenCalledExactlyOnceWith("Development server", expect.objectContaining({
      destination: "127.0.0.1", user: "ctmux", port: 2222,
    }), remoteInfo));
    expect(probeSshHost).toHaveBeenCalledTimes(2);
    expect(close).toHaveBeenCalledOnce();
    expect(saveSshConfigHost).not.toHaveBeenCalled();
  });

  it("retries a failed host save without verifying again or writing SSH config", async () => {
    vi.mocked(probeSshHost).mockResolvedValue(remoteInfo);
    const { user, save, close } = setupNewHost();
    save.mockRejectedValueOnce(new Error("Could not save hosts.json"));
    await newHostDetails(user);
    await user.click(screen.getByRole("option", { name: /SSH config \/ agent/ }));
    expect(await screen.findByText("Could not save hosts.json")).toBeTruthy();
    expect(close).not.toHaveBeenCalled();
    await user.click(screen.getByRole("option", { name: "Retry saving host" }));
    await waitFor(() => expect(close).toHaveBeenCalledOnce());
    expect(save).toHaveBeenCalledTimes(2);
    expect(save.mock.calls[1]).toEqual(save.mock.calls[0]);
    expect(probeSshHost).toHaveBeenCalledOnce();
    expect(saveSshConfigHost).not.toHaveBeenCalled();
  });

  it("installs missing remote components then saves the original named host", async () => {
    vi.mocked(probeSshHost)
      .mockRejectedValueOnce({ code: "ctl_agent_not_found", message: "Install required" })
      .mockResolvedValueOnce(remoteInfo);
    vi.mocked(installRemoteAgent).mockResolvedValue({
      app_version: "0.1.0",
      bundle_id: "0.1.0-dev.0123456789ab",
      git_revision: "0123456789abcdef0123456789abcdef01234567",
      target_triple: "x86_64-unknown-linux-musl",
    });
    const { user, save, close } = setupNewHost();
    await newHostDetails(user);
    await user.click(screen.getByRole("option", { name: /SSH config \/ agent/ }));
    await user.click(await screen.findByRole("option", { name: /Install remote components/ }));
    await waitFor(() => expect(save).toHaveBeenCalledExactlyOnceWith("Development server", expect.objectContaining({
      hostname: "127.0.0.1", user: "ctmux", port: 2222,
    }), remoteInfo));
    expect(installRemoteAgent).toHaveBeenCalledOnce();
    expect(close).toHaveBeenCalledOnce();
    expect(saveSshConfigHost).not.toHaveBeenCalled();
  });

  it("verifies a direct connection method without recovering or reconnecting existing sessions", async () => {
    vi.mocked(probeSshHost).mockResolvedValue(remoteInfo);
    const onSaveConnection = vi.fn(async () => undefined);
    const onVerified = vi.fn();
    const onConnected = vi.fn();
    const onClose = vi.fn();
    const user = userEvent.setup();
    render(<SshHostFlow suggestions={[]} warning={null} expectedIdentity={remoteInfo}
      onSaveConnection={onSaveConnection} onVerified={onVerified} onConnected={onConnected} onClose={onClose} />);
    expect(screen.getByRole("dialog", { name: "Add connection method" })).toBeTruthy();
    await user.type(screen.getByLabelText("SSH host or config alias"), "deploy@10.0.0.8:2222");
    await user.click(screen.getByRole("button", { name: "Verify and save" }));
    await waitFor(() => expect(onSaveConnection).toHaveBeenCalledOnce());
    expect(probeSshHost).toHaveBeenCalledWith(expect.objectContaining({
      hostname: "10.0.0.8", user: "deploy", port: 2222, remote_info: remoteInfo,
    }), expect.any(String), expect.any(Function));
    expect(onSaveConnection).toHaveBeenCalledWith(expect.objectContaining({ hostname: "10.0.0.8" }), [], remoteInfo);
    expect(onVerified).not.toHaveBeenCalled();
    expect(onConnected).not.toHaveBeenCalled();
    expect(onClose).toHaveBeenCalledOnce();
    expect(saveSshConfigHost).not.toHaveBeenCalled();
  });

  it("discovers identity suggestions for connection methods while retaining manual entry", async () => {
    vi.mocked(listSshIdentityFiles).mockResolvedValue({
      identity_files: [{ path: "/home/test/.ssh/deploy", display_path: "~/.ssh/deploy" }], warnings: [],
    });
    vi.mocked(probeSshHost).mockResolvedValue(remoteInfo);
    const onSaveConnection = vi.fn(async () => undefined);
    const user = userEvent.setup();
    render(<SshHostFlow suggestions={[]} warning={null} onSaveConnection={onSaveConnection} onClose={vi.fn()} />);
    await waitFor(() => expect(screen.getByLabelText("Identity file (optional)").parentElement?.querySelector("option")?.value).toBe("/home/test/.ssh/deploy"));
    await user.type(screen.getByLabelText("SSH host or config alias"), "deploy@10.0.0.8");
    await user.type(screen.getByLabelText("Identity file (optional)"), "/custom/private-key");
    await user.click(screen.getByRole("button", { name: "Verify and save" }));
    await waitFor(() => expect(onSaveConnection).toHaveBeenCalledWith(expect.objectContaining({ identity_file: "/custom/private-key" }), [], remoteInfo));
  });

  it.each<{ name: string; endpoint: SshConnectionTarget; address: string }>([
    { name: "SSH config alias with saved overrides", address: "", endpoint: {
      kind: "ssh", destination: "build", ssh_config_alias: "build", user: "deploy", port: 2222,
      identity_file: "~/.ssh/deploy", use_ssh_config_master: false,
    } },
    { name: "managed hostname matching a discovered alias", address: "   ", endpoint: {
      kind: "ssh", destination: "managed-build", hostname: "build", user: "deploy", port: 2222,
      identity_file: "~/.ssh/deploy",
    } },
    { name: "SSH config alias with a hostname override", address: "", endpoint: {
      kind: "ssh", destination: "build", hostname: "10.0.0.8", ssh_config_alias: "build", user: "deploy", port: 2222,
    } },
    { name: "Tailscale node binding", address: "", endpoint: {
      kind: "ssh", destination: "builder.tailnet.ts.net", hostname: "100.64.0.2", tailscale_node_id: "n123", user: "deploy",
    } },
  ])("defaults a blank Add connection address to the preferred $name without copying its route", async ({ endpoint, address }) => {
    vi.mocked(probeSshHost).mockResolvedValue(remoteInfo);
    const onSaveConnection = vi.fn(async () => undefined);
    const user = userEvent.setup();
    render(<SshHostFlow suggestions={["build"]} warning={null} editing_host_id="saved-host" expectedIdentity={remoteInfo}
      default_target={{ ...endpoint, host_id: "stale-host", method_id: "old-method", host_name: "Old name",
        remote_info: { ...remoteInfo, remote_id: "untrusted-default" },
        vpn_connection_id: "old-vpn", gateway_route: [{ gateway_id: "old-edge", mode: "automatic" }],
        gateways: [{ kind: "ssh", gateway_id: "old-edge", name: "Old edge", destination: "edge.example", mode: "automatic" }] }}
      onSaveConnection={onSaveConnection} onClose={vi.fn()} />);
    expect(screen.getByRole("dialog", { name: "Add connection method" })).toBeTruthy();
    const input = screen.getByLabelText("SSH host or config alias");
    expect(input).toHaveProperty("value", "");
    expect(input.getAttribute("placeholder")).toMatch(/^Preferred: /u);
    expect(screen.getByText("Leave blank to use the preferred connection's SSH endpoint and settings.")).toBeTruthy();
    if (address) await user.type(input, address);
    await user.click(screen.getByRole("button", { name: "Verify and save" }));
    await waitFor(() => expect(onSaveConnection).toHaveBeenCalledOnce());
    const expected = { ...endpoint, host_id: "saved-host", gateway_route: [], remote_info: remoteInfo };
    expect(probeSshHost).toHaveBeenCalledExactlyOnceWith(expected, expect.any(String), expect.any(Function));
    expect(onSaveConnection).toHaveBeenCalledExactlyOnceWith(expected, [], remoteInfo);
    expect(saveSshConfigHost).not.toHaveBeenCalled();
  });

  it("applies manual alias, key, and master overrides to the default managed endpoint", async () => {
    vi.mocked(probeSshHost).mockResolvedValue(remoteInfo);
    const onSaveConnection = vi.fn(async () => undefined);
    const user = userEvent.setup();
    render(<SshHostFlow suggestions={[]} warning={null}
      default_target={{ kind: "ssh", destination: "build", hostname: "10.0.0.8", user: "deploy", port: 2222,
        identity_file: "~/.ssh/default", use_ssh_config_master: false }}
      onSaveConnection={onSaveConnection} onClose={vi.fn()} />);
    await user.type(screen.getByLabelText("SSH alias (optional)"), "office-build");
    await user.type(screen.getByLabelText("Identity file (optional)"), "~/.ssh/office");
    await user.click(screen.getByRole("checkbox", { name: "Use SSH-config master" }));
    await user.click(screen.getByRole("button", { name: "Verify and save" }));
    await waitFor(() => expect(onSaveConnection).toHaveBeenCalledOnce());
    const expected = { kind: "ssh", destination: "office-build", hostname: "10.0.0.8", user: "deploy", port: 2222,
      identity_file: "~/.ssh/office", use_ssh_config_master: true, gateway_route: [] };
    expect(probeSshHost).toHaveBeenCalledExactlyOnceWith(expected, expect.any(String), expect.any(Function));
    expect(onSaveConnection).toHaveBeenCalledExactlyOnceWith(expected, [], remoteInfo);
  });

  it("uses an explicit Add connection address without inheriting the preferred provider or authentication settings", async () => {
    vi.mocked(probeSshHost).mockResolvedValue(remoteInfo);
    const onSaveConnection = vi.fn(async () => undefined);
    const user = userEvent.setup();
    render(<SshHostFlow suggestions={["build"]} warning={null}
      default_target={{ kind: "ssh", destination: "build", ssh_config_alias: "build", tailscale_node_id: "n123",
        user: "deploy", port: 2222, identity_file: "~/.ssh/default", use_ssh_config_master: true }}
      onSaveConnection={onSaveConnection} onClose={vi.fn()} />);
    await user.type(screen.getByLabelText("SSH host or config alias"), "other.internal");
    await user.click(screen.getByRole("button", { name: "Verify and save" }));
    await waitFor(() => expect(onSaveConnection).toHaveBeenCalledOnce());
    const expected = { kind: "ssh", destination: "other.internal", hostname: "other.internal", gateway_route: [] };
    expect(probeSshHost).toHaveBeenCalledExactlyOnceWith(expected, expect.any(String), expect.any(Function));
    expect(onSaveConnection).toHaveBeenCalledExactlyOnceWith(expected, [], remoteInfo);
  });

  it.each([undefined, { kind: "ssh" as const, destination: "editing-build", hostname: "10.0.0.8" }])(
    "requires an address when adding without a default or clearing an existing method (%j)", async (initialTarget) => {
      const onSaveConnection = vi.fn(async () => undefined);
      const user = userEvent.setup();
      render(<SshHostFlow suggestions={[]} warning={null} initialTarget={initialTarget}
        default_target={initialTarget ? { kind: "ssh", destination: "preferred-build" } : undefined}
        onSaveConnection={onSaveConnection} onClose={vi.fn()} />);
      await user.clear(screen.getByLabelText("SSH host or config alias"));
      await user.click(screen.getByRole("button", { name: "Verify and save" }));
      expect(await screen.findByRole("alert")).toHaveProperty("textContent", "Enter the SSH host or config alias.");
      expect(probeSshHost).not.toHaveBeenCalled();
      expect(onSaveConnection).not.toHaveBeenCalled();
    },
  );

  it("requires an explicit address when the preferred endpoint is unavailable", async () => {
    const onSaveConnection = vi.fn(async () => undefined);
    const user = userEvent.setup();
    render(<SshHostFlow suggestions={[]} warning={null}
      default_target={{ kind: "ssh", destination: "removed-build", unavailable: "This SSH endpoint is no longer available." }}
      onSaveConnection={onSaveConnection} onClose={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: "Verify and save" }));
    expect(await screen.findByRole("alert")).toHaveProperty("textContent",
      "This SSH endpoint is no longer available. Enter an SSH host or config alias for this connection.");
    expect(probeSshHost).not.toHaveBeenCalled();
    expect(onSaveConnection).not.toHaveBeenCalled();
  });

  it("exports a direct alias only when explicitly selected and after successful verification", async () => {
    let finishProbe!: (value: typeof remoteInfo) => void;
    vi.mocked(probeSshHost).mockImplementationOnce(() => new Promise((resolve) => { finishProbe = resolve; }));
    vi.mocked(saveSshConfigHost).mockResolvedValue({ destination: "office-build" });
    const onSaveConnection = vi.fn(async () => undefined);
    const user = userEvent.setup();
    render(<SshHostFlow suggestions={[]} warning={null} onSaveConnection={onSaveConnection} onClose={vi.fn()} />);
    await user.type(screen.getByLabelText("SSH host or config alias"), "deploy@10.0.0.8:2222");
    await user.type(screen.getByLabelText("SSH alias (optional)"), "office-build");
    await user.click(screen.getByLabelText("Also save to OpenSSH config"));
    await user.click(screen.getByRole("button", { name: "Verify and save" }));
    expect(saveSshConfigHost).not.toHaveBeenCalled();
    await act(async () => finishProbe(remoteInfo));
    await waitFor(() => expect(onSaveConnection).toHaveBeenCalledOnce());
    expect(saveSshConfigHost).toHaveBeenCalledExactlyOnceWith({
      alias: "office-build", hostname: "10.0.0.8", user: "deploy", port: 2222, identity_file: null,
    });
    expect(onSaveConnection).toHaveBeenCalledWith(expect.objectContaining({ destination: "office-build", hostname: "10.0.0.8" }), [], remoteInfo);
  });

  it("does not export an alias after the user adds a gateway to a direct method", async () => {
    vi.mocked(probeSshHost).mockResolvedValue(remoteInfo);
    const onSaveConnection = vi.fn(async () => undefined);
    const user = userEvent.setup();
    render(<SshHostFlow suggestions={["saved-alias"]} warning={null} onSaveConnection={onSaveConnection} onClose={vi.fn()}
      gateways={[{ gateway_id: "edge", name: "Edge", destination: "edge.example" }]} />);
    await user.type(screen.getByLabelText("SSH host or config alias"), "saved-alias");
    expect((screen.getByRole("checkbox", { name: "Also save to OpenSSH config" }) as HTMLInputElement).disabled).toBe(true);
    await user.clear(screen.getByLabelText("SSH host or config alias"));
    await user.type(screen.getByLabelText("SSH host or config alias"), "10.0.0.8");
    await user.click(screen.getByRole("checkbox", { name: "Also save to OpenSSH config" }));
    await user.click(screen.getByRole("button", { name: "Add" }));
    expect((screen.getByRole("checkbox", { name: "Also save to OpenSSH config" }) as HTMLInputElement).disabled).toBe(true);
    expect((screen.getByRole("checkbox", { name: "Also save to OpenSSH config" }) as HTMLInputElement).checked).toBe(false);
    await user.click(screen.getByRole("button", { name: "Verify and save" }));
    await waitFor(() => expect(onSaveConnection).toHaveBeenCalledOnce());
    expect(saveSshConfigHost).not.toHaveBeenCalled();
  });

  it("prepopulates connection editing and preserves its gateway route during verification", async () => {
    vi.mocked(probeSshHost).mockResolvedValue(remoteInfo);
    const onSaveConnection = vi.fn(async () => undefined);
    const user = userEvent.setup();
    render(<SshHostFlow suggestions={[]} warning={null} expectedIdentity={remoteInfo}
      initialTarget={{ kind: "ssh", destination: "build", hostname: "2001:db8::1", user: "deploy", port: 2222,
        identity_file: "~/.ssh/deploy", gateway_route: [{ gateway_id: "edge", mode: "native_only" }] }}
      gateways={[{ gateway_id: "edge", name: "Edge", destination: "edge.example" }]}
      onSaveConnection={onSaveConnection} onClose={vi.fn()} />);
    expect(screen.getByRole("dialog", { name: "Edit connection method" })).toBeTruthy();
    expect(screen.getByLabelText("SSH host or config alias")).toHaveProperty("value", "deploy@[2001:db8::1]:2222");
    expect(screen.getByLabelText("SSH alias (optional)")).toHaveProperty("value", "build");
    expect(screen.getByLabelText("Identity file (optional)")).toHaveProperty("value", "~/.ssh/deploy");
    await user.click(screen.getByRole("button", { name: "Verify and save" }));
    await waitFor(() => expect(onSaveConnection).toHaveBeenCalledOnce());
    expect(onSaveConnection).toHaveBeenCalledWith(expect.objectContaining({
      hostname: "2001:db8::1", destination: "build", user: "deploy", port: 2222,
      gateway_route: [{ gateway_id: "edge", mode: "native_only" }],
      gateways: [expect.objectContaining({ destination: "edge.example", mode: "native_only" })],
    }), expect.any(Array), remoteInfo);
  });

  it.each([
    ["build", "build"],
    ["deploy@build:2222", "build"],
    ["deploy@10.0.0.8:2222", undefined],
  ])("retains SSH-config origin only while the alias remains the endpoint (%s)", async (address, sshConfigAlias) => {
    vi.mocked(probeSshHost).mockResolvedValue(remoteInfo);
    const onSaveConnection = vi.fn(async (_target: unknown, _gateways: unknown, _identity: unknown) => undefined);
    const user = userEvent.setup();
    render(<SshHostFlow suggestions={["build"]} warning={null} expectedIdentity={remoteInfo}
      initialTarget={{ kind: "ssh", destination: "build", ssh_config_alias: "build", user: "deploy", port: 2222,
        gateway_route: [{ gateway_id: "edge", mode: "native_only" }] }}
      gateways={[{ gateway_id: "edge", name: "Edge", destination: "edge.example" }]}
      onSaveConnection={onSaveConnection} onClose={vi.fn()} />);
    await user.clear(screen.getByLabelText("SSH host or config alias"));
    await user.type(screen.getByLabelText("SSH host or config alias"), address);
    await user.type(screen.getByLabelText("Identity file (optional)"), "~/.ssh/deploy");
    await user.click(screen.getByRole("button", { name: "Verify and save" }));
    await waitFor(() => expect(onSaveConnection).toHaveBeenCalledOnce());
    const target = onSaveConnection.mock.calls[0][0] as { ssh_config_alias?: string };
    expect(target.ssh_config_alias).toBe(sshConfigAlias);
    expect(target).toMatchObject({ user: "deploy", port: 2222, identity_file: "~/.ssh/deploy",
      gateway_route: [{ gateway_id: "edge", mode: "native_only" }] });
    if (sshConfigAlias) expect(target).not.toHaveProperty("hostname");
    else expect(target).toHaveProperty("hostname", "10.0.0.8");
  });

  it("keeps a managed alias-shaped method managed when its SSH settings are edited", async () => {
    vi.mocked(probeSshHost).mockResolvedValue(remoteInfo);
    const onSaveConnection = vi.fn(async (_target: unknown, _gateways: unknown, _identity: unknown) => undefined);
    const user = userEvent.setup();
    render(<SshHostFlow suggestions={["build"]} warning={null}
      initialTarget={{ kind: "ssh", destination: "build", user: "deploy" }}
      onSaveConnection={onSaveConnection} onClose={vi.fn()} />);
    await user.type(screen.getByLabelText("Identity file (optional)"), "~/.ssh/deploy");
    await user.click(screen.getByRole("button", { name: "Verify and save" }));
    await waitFor(() => expect(onSaveConnection).toHaveBeenCalledOnce());
    expect(onSaveConnection.mock.calls[0][0]).not.toHaveProperty("ssh_config_alias");
  });

  it("marks a newly selected SSH-config method before probing", async () => {
    vi.mocked(probeSshHost).mockResolvedValue(remoteInfo);
    const onSaveConnection = vi.fn(async () => undefined);
    const user = userEvent.setup();
    render(<SshHostFlow suggestions={["build"]} warning={null} onSaveConnection={onSaveConnection} onClose={vi.fn()} />);
    await user.type(screen.getByLabelText("SSH host or config alias"), "build");
    await user.click(screen.getByRole("button", { name: "Verify and save" }));
    await waitFor(() => expect(onSaveConnection).toHaveBeenCalledOnce());
    expect(probeSshHost).toHaveBeenCalledWith(expect.objectContaining({ destination: "build", ssh_config_alias: "build" }),
      expect.any(String), expect.any(Function));
    expect(onSaveConnection).toHaveBeenCalledWith(expect.objectContaining({ ssh_config_alias: "build" }), [], remoteInfo);
  });

  it("follows the address source until the master checkbox is explicitly changed", async () => {
    vi.mocked(probeSshHost).mockResolvedValue(remoteInfo);
    const onSaveConnection = vi.fn(async () => undefined);
    render(<SshHostFlow suggestions={["build"]} warning={null} onSaveConnection={onSaveConnection} onClose={vi.fn()} />);
    const user = userEvent.setup();
    const checkbox = screen.getByRole("checkbox", { name: "Use SSH-config master" });
    const address = screen.getByLabelText("SSH host or config alias");
    expect(checkbox).toHaveProperty("checked", false);
    await user.type(address, "deploy@build:2222");
    expect(checkbox).toHaveProperty("checked", true);
    await user.clear(address);
    await user.type(address, "10.0.0.8");
    expect(checkbox).toHaveProperty("checked", false);
    await user.clear(address);
    await user.type(address, "build");
    await user.click(checkbox);
    expect(checkbox).toHaveProperty("checked", false);
    await user.clear(address);
    await user.type(address, "deploy@build:2222");
    expect(checkbox).toHaveProperty("checked", false);
    await user.click(screen.getByRole("button", { name: "Verify and save" }));
    await waitFor(() => expect(onSaveConnection).toHaveBeenCalledWith(expect.objectContaining({
      destination: "build", ssh_config_alias: "build", user: "deploy", port: 2222, use_ssh_config_master: false,
    }), [], remoteInfo));
    expect(probeSshHost).toHaveBeenCalledWith(expect.objectContaining({ use_ssh_config_master: false, ssh_config_alias: "build" }),
      expect.any(String), expect.any(Function));
  });

  it("omits master selection on Windows while retaining an imported preference for native validation", async () => {
    const platform = vi.spyOn(window.navigator, "platform", "get").mockReturnValue("Win32");
    try {
      vi.mocked(probeSshHost).mockRejectedValue(new Error("SSH master selection is unavailable on this platform."));
      const initialTarget: SshConnectionTarget = {
        kind: "ssh", destination: "build", ssh_config_alias: "build", use_ssh_config_master: false,
      };
      const onSaveConnection = vi.fn(async () => undefined);
      render(<SshHostFlow suggestions={["build"]} warning={null} initialTarget={initialTarget}
        onSaveConnection={onSaveConnection} onClose={vi.fn()} />);
      expect(screen.queryByRole("checkbox", { name: "Use SSH-config master" })).toBeNull();
      await userEvent.setup().click(screen.getByRole("button", { name: "Verify and save" }));
      expect(await screen.findByText("SSH master selection is unavailable on this platform.")).toBeTruthy();
      expect(probeSshHost).toHaveBeenCalledWith(expect.objectContaining(initialTarget), expect.any(String), expect.any(Function));
      expect(onSaveConnection).not.toHaveBeenCalled();
    } finally {
      platform.mockRestore();
    }
  });

  it.each<SshConnectionTarget>([
    { kind: "ssh", destination: "build", ssh_config_alias: "build" },
    { kind: "ssh", destination: "build", hostname: "10.0.0.8", ssh_config_alias: "build" },
    { kind: "ssh", destination: "direct", hostname: "10.0.0.8" },
    { kind: "ssh", destination: "builder.tailnet.ts.net", hostname: "100.64.0.2", tailscale_node_id: "n123" },
    { kind: "ssh", destination: "build", ssh_config_alias: "build", use_ssh_config_master: false },
    { kind: "ssh", destination: "direct", hostname: "10.0.0.8", use_ssh_config_master: true },
  ])("edits and verifies the master preference without losing provider identity (%j)", async (initialTarget) => {
    const current = initialTarget.use_ssh_config_master ?? Boolean(initialTarget.ssh_config_alias);
    const onSaveConnection = vi.fn(async () => undefined);
    const onClose = vi.fn();
    let showPrompt!: (prompt: SshPrompt) => void;
    let finish!: (identity: typeof remoteInfo) => void;
    vi.mocked(probeSshHost).mockImplementationOnce((_target, _attempt, prompt) => {
      showPrompt = prompt;
      return new Promise((resolve) => { finish = resolve; });
    });
    render(<SshHostFlow suggestions={["build"]} warning={null} initialTarget={initialTarget}
      onSaveConnection={onSaveConnection} onClose={onClose} />);
    const user = userEvent.setup();
    const checkbox = screen.getByRole("checkbox", { name: "Use SSH-config master" });
    expect(checkbox).toHaveProperty("checked", current);
    await user.click(checkbox);
    await user.click(screen.getByRole("button", { name: "Verify and save" }));
    const expected = expect.objectContaining({ ...initialTarget, use_ssh_config_master: !current });
    expect(probeSshHost).toHaveBeenCalledWith(expected, expect.any(String), expect.any(Function));
    expect(onSaveConnection).not.toHaveBeenCalled();
    await act(async () => showPrompt({ prompt_id: "password", kind: "secret", message: "Password:" }));
    await user.type(screen.getByLabelText("SSH response"), "secret{Enter}");
    expect(respondSshPrompt).toHaveBeenCalledWith(expect.any(String), "password", "secret");
    await act(async () => finish(remoteInfo));
    expect(onSaveConnection).toHaveBeenCalledWith(expected, [], remoteInfo);
    expect(onClose).toHaveBeenCalledOnce();
  });

  it.each([
    ["deploy@100.64.0.2:2222", "n123"],
    ["deploy@10.0.0.8:2222", undefined],
  ])("retains a Tailscale node binding only while editing the same endpoint (%s)", async (address, nodeId) => {
    vi.mocked(probeSshHost).mockResolvedValue(remoteInfo);
    const onSaveConnection = vi.fn(async (_target: unknown, _gateways: unknown, _identity: unknown) => undefined);
    const user = userEvent.setup();
    render(<SshHostFlow suggestions={[]} warning={null} expectedIdentity={remoteInfo}
      initialTarget={{ kind: "ssh", destination: "builder.tailnet.ts.net", hostname: "100.64.0.2", tailscale_node_id: "n123" }}
      onSaveConnection={onSaveConnection} onClose={vi.fn()} />);
    await user.clear(screen.getByLabelText("SSH host or config alias"));
    await user.type(screen.getByLabelText("SSH host or config alias"), address);
    await user.type(screen.getByLabelText("Identity file (optional)"), "~/.ssh/deploy");
    expect(probeSshHost).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "Verify and save" }));
    await waitFor(() => expect(onSaveConnection).toHaveBeenCalledOnce());
    expect(onSaveConnection.mock.calls[0][0]).toMatchObject({ user: "deploy", port: 2222, identity_file: "~/.ssh/deploy" });
    expect(onSaveConnection.mock.calls[0][0]).toHaveProperty("hostname", address.includes("10.0.0.8") ? "10.0.0.8" : "100.64.0.2");
    expect((onSaveConnection.mock.calls[0][0] as { tailscale_node_id?: string }).tailscale_node_id).toBe(nodeId);
    expect(saveSshConfigHost).not.toHaveBeenCalled();
  });

  it("rejects methods that verify as another remote environment and preserves the draft", async () => {
    vi.mocked(probeSshHost).mockResolvedValue({ ...remoteInfo, remote_id: "different-environment" });
    const onSaveConnection = vi.fn(async () => undefined);
    const user = userEvent.setup();
    render(<SshHostFlow suggestions={[]} warning={null} expectedIdentity={remoteInfo}
      onSaveConnection={onSaveConnection} onClose={vi.fn()} />);
    await user.type(screen.getByLabelText("SSH host or config alias"), "10.0.0.8");
    await user.click(screen.getByRole("button", { name: "Verify and save" }));
    expect(await screen.findByText(/This connection reaches a different remote environment/)).toBeTruthy();
    expect(onSaveConnection).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "Previous step" }));
    expect(screen.getByLabelText("SSH host or config alias")).toHaveProperty("value", "10.0.0.8");
  });

  it("keeps existing method credentials after a failed edit is cancelled", async () => {
    vi.mocked(probeSshHost).mockRejectedValue(new Error("Connection unavailable"));
    const user = userEvent.setup();
    const onClose = vi.fn();
    const { unmount } = render(<SshHostFlow suggestions={[]} warning={null} expectedIdentity={remoteInfo}
      initialTarget={{ kind: "ssh", destination: "build-office", user: "deploy", port: 2222 }}
      onSaveConnection={vi.fn()} onClose={onClose} />);
    await user.click(screen.getByRole("button", { name: "Verify and save" }));
    expect(await screen.findByText("Connection unavailable")).toBeTruthy();
    expect(probeSshHost).toHaveBeenCalledWith(expect.objectContaining({
      destination: "build-office", user: "deploy", port: 2222,
    }), expect.any(String), expect.any(Function));
    await user.keyboard("{Escape}");
    expect(onClose).toHaveBeenCalledOnce();
    unmount();
    expect(forgetSshCredentials).not.toHaveBeenCalled();
  });

  it("validates routed host details in the initial dialog before probing", async () => {
    const user = userEvent.setup();
    render(
      <SshHostFlow
        complex
        gateways={[{ gateway_id: "edge", name: "Edge", destination: "edge.example" }]}
        suggestions={[]}
        warning={null}
        onVerified={async () => null}
        onSaveHost={vi.fn()}
        onSaveRoutedHost={vi.fn()}
        onActivateHost={vi.fn()}
        onConnected={vi.fn()}
        onClose={vi.fn()}
      />,
    );

    await user.click(screen.getByRole("button", { name: "Add" }));
    await user.click(screen.getByRole("button", { name: "Connect" }));
    expect(screen.getByRole("alert").textContent).toContain("Enter the SSH host");
    expect(probeSshHost).not.toHaveBeenCalled();
    expect(screen.getByRole("dialog", { name: "Add host with gateways" })).toBeTruthy();
  });

  it("uses a saved gateway on the first connection and saves only after verification", async () => {
    let completeProbe: ((value: typeof remoteInfo) => void) | undefined;
    vi.mocked(probeSshHost).mockImplementationOnce(() =>
      new Promise((resolve) => { completeProbe = resolve; }));
    const onSaveRoutedHost = vi.fn(async () => undefined);
    const user = userEvent.setup();
    render(
      <SshHostFlow
        complex
        gateways={[{
          gateway_id: "edge",
          name: "Edge",
          destination: "edge.example",
        }]}
        suggestions={[]}
        warning={null}
        onVerified={async () => null}
        onSaveHost={vi.fn()}
        onSaveRoutedHost={onSaveRoutedHost}
        onActivateHost={vi.fn()}
        onConnected={vi.fn()}
        onClose={vi.fn()}
      />,
    );

    expect(screen.getByRole("dialog", { name: "Add host with gateways" })).toBeTruthy();
    expect(screen.queryByLabelText("SSH host")).toBeNull();
    await user.type(screen.getByLabelText("SSH host or config alias"), "ctmux@127.0.0.1:2222");
    await user.type(screen.getByLabelText("SSH alias (optional)"), "ctmux-test");
    await user.click(screen.getByRole("button", { name: "Add" }));
    await user.click(screen.getByRole("button", { name: "Connect" }));

    expect(probeSshHost).toHaveBeenCalledWith(
      expect.objectContaining({
        destination: "ctmux-test",
        gateway_route: [{ gateway_id: "edge", mode: "automatic" }],
        gateways: [expect.objectContaining({ destination: "edge.example" })],
      }),
      expect.any(String),
      expect.any(Function),
    );
    expect(onSaveRoutedHost).not.toHaveBeenCalled();
    await act(async () => completeProbe?.(remoteInfo));
    await waitFor(() => expect(onSaveRoutedHost).toHaveBeenCalledWith(
      expect.objectContaining({ destination: "ctmux-test" }),
      [{ gateway_id: "edge", name: "Edge", destination: "edge.example" }],
      remoteInfo,
    ));
  });

  it("routes a selected SSH config host before its first probe", async () => {
    vi.mocked(probeSshHost).mockResolvedValue(remoteInfo);
    const onSaveRoutedHost = vi.fn(async () => undefined);
    const user = userEvent.setup();
    render(
      <SshHostFlow
        complex
        gateways={[{
          gateway_id: "edge",
          name: "Edge",
          destination: "edge.example",
        }]}
        suggestions={["internal-server"]}
        warning={null}
        onVerified={async () => null}
        onSaveHost={vi.fn()}
        onSaveRoutedHost={onSaveRoutedHost}
        onActivateHost={vi.fn()}
        onConnected={vi.fn()}
        onClose={vi.fn()}
      />,
    );

    await user.type(screen.getByLabelText("SSH host or config alias"), "internal-server");
    await user.click(screen.getByRole("button", { name: "Add" }));
    await user.click(screen.getByRole("button", { name: "Connect" }));
    await waitFor(() => expect(probeSshHost).toHaveBeenCalledWith(
      expect.objectContaining({
        destination: "internal-server",
        gateway_route: [{ gateway_id: "edge", mode: "automatic" }],
      }),
      expect.any(String),
      expect.any(Function),
    ));
    await waitFor(() => expect(onSaveRoutedHost).toHaveBeenCalledOnce());
  });

  it("keeps a newly created gateway in the draft until the routed host verifies", async () => {
    vi.mocked(probeSshHost).mockRejectedValueOnce(new Error("SSH unavailable"));
    const onSaveRoutedHost = vi.fn(async () => undefined);
    const user = userEvent.setup();
    render(
      <SshHostFlow
        complex
        gateways={[]}
        suggestions={[]}
        warning={null}
        onVerified={async () => null}
        onSaveHost={vi.fn()}
        onSaveRoutedHost={onSaveRoutedHost}
        onActivateHost={vi.fn()}
        onConnected={vi.fn()}
        onClose={vi.fn()}
      />,
    );

    await user.type(screen.getByLabelText("SSH host or config alias"), "ctmux@127.0.0.1:2222");
    await user.type(screen.getByLabelText("SSH alias (optional)"), "ctmux-test");
    expect((screen.getByRole("button", { name: "Connect" }) as HTMLButtonElement).disabled).toBe(true);
    await user.click(screen.getByRole("button", { name: "+ New gateway" }));
    await user.type(screen.getByLabelText("Name"), "Bastion");
    await user.type(screen.getByLabelText("SSH destination / alias"), "bastion.example");
    await user.click(screen.getByRole("button", { name: "Save gateway" }));
    await user.click(screen.getByRole("button", { name: "Connect" }));

    expect(await screen.findByText("SSH unavailable")).toBeTruthy();
    expect(probeSshHost).toHaveBeenCalledWith(
      expect.objectContaining({
        gateways: [expect.objectContaining({ destination: "bastion.example" })],
      }),
      expect.any(String),
      expect.any(Function),
    );
    expect(onSaveRoutedHost).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "Previous step" }));
    expect(screen.getByRole("dialog", { name: "Add host with gateways" })).toBeTruthy();
    expect((screen.getByLabelText("SSH host or config alias") as HTMLInputElement).value)
      .toBe("ctmux@127.0.0.1:2222");
    expect(screen.getByText("1. Bastion")).toBeTruthy();
  });

  it("opens directly on the confirmed remote-component update action", () => {
    const target = { kind: "ssh" as const, host_id: "known-host", destination: "example" };
    render(
      <SshHostFlow
        suggestions={[]}
        warning={null}
        target={target}
        updateRequired
        onVerified={async () => target}
        onSaveHost={vi.fn()}
        onActivateHost={vi.fn()}
        onConnected={vi.fn()}
        onClose={vi.fn()}
      />,
    );

    expect(screen.getByRole("dialog", { name: "Update remote components" })).toBeTruthy();
    expect(screen.getByRole("option", { name: /Update remote components/ })).toBeTruthy();
    expect(probeSshHost).not.toHaveBeenCalled();
  });

  it("recovers a verified host automatically before offering storage", async () => {
    vi.mocked(probeSshHost).mockResolvedValue(remoteInfo);
    const recovered = { kind: "ssh" as const, host_id: "known-host", destination: "new-ip", remote_info: remoteInfo };
    const onVerified = vi.fn(async () => recovered);
    const onConnected = vi.fn();
    const onSaveHost = vi.fn();
    const onClose = vi.fn();
    render(<SshHostFlow suggestions={[]} warning={null} onVerified={onVerified} onActivateHost={vi.fn()} onSaveHost={onSaveHost} onConnected={onConnected} onClose={onClose} />);
    const user = userEvent.setup();
    await details(user);
    await user.click(screen.getByRole("option", { name: /SSH config \/ agent/ }));
    await waitFor(() => expect(onConnected).toHaveBeenCalledExactlyOnceWith(recovered));
    expect(onVerified).toHaveBeenCalledWith(expect.objectContaining({ hostname: "127.0.0.1" }), remoteInfo);
    expect(onSaveHost).not.toHaveBeenCalled();
    expect(onClose).toHaveBeenCalledOnce();
    expect(screen.queryByText("Save host")).toBeNull();
  });

  it("offers an update for agents without identity support", async () => {
    vi.mocked(probeSshHost).mockRejectedValueOnce({ code: "ctl_agent_identity_unsupported", message: "Update required" });
    const { user } = setup();
    await details(user);
    await user.click(screen.getByRole("option", { name: /SSH config \/ agent/ }));
    expect(await screen.findByRole("option", { name: /Update remote components/ })).toBeTruthy();
  });

  it("keeps a closed daemon handshake retryable without assuming components need updating", async () => {
    const message = "SSH verified final-destination, but its terminal connection closed before the daemon replied.";
    vi.mocked(probeSshHost).mockRejectedValueOnce({ code: "remote_ctmux_handshake_closed", message });
    render(<SshHostFlow suggestions={[]} warning={null} target={{ kind: "ssh", destination: "final-destination" }}
      autoConnect onClose={vi.fn()} />);
    expect(await screen.findByText(message)).toBeTruthy();
    expect(screen.getByRole("option", { name: "Connect" })).toBeTruthy();
    expect(screen.queryByRole("option", { name: /Update remote components/ })).toBeNull();
    expect(screen.queryByRole("option", { name: /Force restart/ })).toBeNull();
    expect(installRemoteAgent).not.toHaveBeenCalled();
  });

  it("discovers identities only on the identity step and connects with a selected path", async () => {
    vi.mocked(listSshIdentityFiles).mockResolvedValue({
      identity_files: [
        {
          path: "/test-home/.ssh/local.id_rsa",
          display_path: "~/.ssh/local.id_rsa",
        },
      ],
      warnings: [],
    });
    vi.mocked(probeSshHost).mockResolvedValue(remoteInfo);
    const { user } = setup();
    await details(user);
    expect(listSshIdentityFiles).not.toHaveBeenCalled();
    await user.click(screen.getByRole("option", { name: /Identity file/ }));
    await screen.findByRole("option", { name: "~/.ssh/local.id_rsa" });
    await user.keyboard("{ArrowDown}{Enter}");
    expect(probeSshHost).toHaveBeenCalledWith(
      expect.objectContaining({
        identity_file: "/test-home/.ssh/local.id_rsa",
      }),
      expect.any(String),
      expect.any(Function),
    );
  });

  it("allows a manual identity when discovery fails", async () => {
    vi.mocked(listSshIdentityFiles).mockRejectedValue(
      new Error("Permission denied"),
    );
    vi.mocked(probeSshHost).mockResolvedValue(remoteInfo);
    const { user } = setup();
    await details(user);
    await user.click(screen.getByRole("option", { name: /Identity file/ }));
    await screen.findByText(/Could not list ~\/.ssh: Permission denied/);
    await user.type(
      screen.getByRole("combobox", { name: "Identity file" }),
      "/custom/key{Enter}",
    );
    expect(probeSshHost).toHaveBeenCalledWith(
      expect.objectContaining({ identity_file: "/custom/key" }),
      expect.any(String),
      expect.any(Function),
    );
  });

  it("ignores a stale discovery after leaving and reopening the identity step", async () => {
    let resolveFirst:
      | ((catalog: { identity_files: []; warnings: string[] }) => void)
      | undefined;
    vi.mocked(listSshIdentityFiles).mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveFirst = resolve;
        }),
    );
    const { user } = setup();
    await details(user);
    await user.click(screen.getByRole("option", { name: /Identity file/ }));
    await screen.findByText("Loading identity files…");
    await user.click(screen.getByRole("button", { name: "Previous step" }));
    await user.click(screen.getByRole("option", { name: /Identity file/ }));
    await screen.findByText(
      "No identity-file candidates in ~/.ssh. Enter a path manually.",
    );
    await act(async () =>
      resolveFirst?.({ identity_files: [], warnings: ["stale error"] }),
    );
    expect(screen.queryByText("stale error")).toBeNull();
    expect(listSshIdentityFiles).toHaveBeenCalledTimes(2);
  });

  it("forgets unsaved credentials when the storage step is cancelled", async () => {
    vi.mocked(probeSshHost).mockResolvedValue(remoteInfo);
    const { user, save, close } = setup();
    await details(user);
    await user.click(
      screen.getByRole("option", { name: /SSH config \/ agent/ }),
    );
    await screen.findByRole("dialog", { name: "Save host" });
    await user.keyboard("{Escape}");
    expect(forgetSshCredentials).toHaveBeenCalledWith(
      expect.objectContaining({ destination: "ctmux-test" }),
    );
    expect(save).not.toHaveBeenCalled();
    expect(close).toHaveBeenCalledOnce();
  });

  it("clears transient credentials when an agent install is abandoned", async () => {
    vi.mocked(probeSshHost).mockRejectedValueOnce({
      code: "ctl_agent_not_found",
      message: "Install required",
    });
    const { user, close } = setup();
    await details(user);
    await user.click(
      screen.getByRole("option", { name: /SSH config \/ agent/ }),
    );
    await screen.findByRole("option", { name: /Install remote components/ });
    await user.keyboard("{Escape}");
    expect(forgetSshCredentials).toHaveBeenCalledWith(
      expect.objectContaining({ destination: "ctmux-test" }),
    );
    expect(close).toHaveBeenCalledOnce();
  });

  it("requires an explicit trust choice and ignores late prompts after cancellation", async () => {
    let prompt: ((value: SshPrompt) => void) | undefined;
    vi.mocked(probeSshHost).mockImplementation(
      (_target, _attempt, callback) => {
        prompt = callback;
        return new Promise(() => undefined);
      },
    );
    const { user, close } = setup();
    await details(user);
    await user.click(
      screen.getByRole("option", { name: /SSH config \/ agent/ }),
    );
    await act(async () =>
      prompt?.({
        prompt_id: "host-key",
        kind: "confirm",
        message: "Verify fingerprint SHA256:test",
      }),
    );
    expect(screen.getByRole("button", { name: /^Cancel$/ })).toBe(
      document.activeElement,
    );
    expect(respondSshPrompt).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "Trust and connect" }));
    expect(respondSshPrompt).toHaveBeenCalledWith(
      expect.any(String),
      "host-key",
      "yes",
    );
    await user.keyboard("{Escape}");
    await act(async () =>
      prompt?.({
        prompt_id: "late",
        kind: "secret",
        message: "Late password:",
      }),
    );
    expect(screen.queryByText("Late password:")).toBeNull();
    expect(close).toHaveBeenCalledOnce();
  });

  it("keeps failed storage writes recoverable without connecting again", async () => {
    vi.mocked(probeSshHost).mockResolvedValue(remoteInfo);
    const { user, save, close } = setup();
    save.mockRejectedValueOnce(new Error("Alias already exists"));
    await details(user);
    await user.click(
      screen.getByRole("option", { name: /SSH config \/ agent/ }),
    );
    await user.click(
      await screen.findByRole("option", { name: /OpenSSH config/ }),
    );
    await screen.findByText("Alias already exists");
    expect(close).not.toHaveBeenCalled();
    await user.click(screen.getByRole("option", { name: /This app only/ }));
    expect(close).toHaveBeenCalledOnce();
    expect(probeSshHost).toHaveBeenCalledOnce();
  });

  it("types every stage, verifies before saving, and keeps the save location choice", async () => {
    vi.mocked(probeSshHost).mockResolvedValue(remoteInfo);
    const { save, user } = setup();
    await details(user);
    await user.click(screen.getByRole("option", { name: /Identity file/ }));
    await user.type(
      screen.getByRole("combobox", { name: "Identity file" }),
      "~/.ssh/local.id_rsa{Enter}",
    );
    await screen.findByText(
      "Connection verified. Where should this host be saved?",
    );
    expect(save).not.toHaveBeenCalled();
    await user.click(screen.getByRole("option", { name: /This app only/ }));
    expect(save).toHaveBeenCalledWith(
      {
        alias: "ctmux-test",
        hostname: "127.0.0.1",
        user: "ctmux",
        port: 2222,
        identity_file: "~/.ssh/local.id_rsa",
      },
      "local_storage",
      remoteInfo,
    );
  });

  it("brokers masked SSH prompts and cancels the native attempt on Escape", async () => {
    let prompt: ((value: SshPrompt) => void) | undefined;
    vi.mocked(probeSshHost).mockImplementation(
      (_target, _attempt, callback) => {
        prompt = callback;
        return new Promise(() => undefined);
      },
    );
    const { save, close, user } = setup();
    await details(user);
    await user.click(
      screen.getByRole("option", { name: /Password \/ interactive/ }),
    );
    await act(async () =>
      prompt?.({ prompt_id: "secret-1", kind: "secret", message: "Password:" }),
    );
    expect(screen.getByLabelText("SSH response").getAttribute("type")).toBe(
      "password",
    );
    await user.type(
      screen.getByLabelText("SSH response"),
      "temporary-secret{Enter}",
    );
    expect(respondSshPrompt).toHaveBeenCalledWith(
      expect.any(String),
      "secret-1",
      "temporary-secret",
    );
    await user.keyboard("{Escape}");
    expect(cancelSshProbe).toHaveBeenCalledOnce();
    expect(close).toHaveBeenCalledOnce();
    expect(save).not.toHaveBeenCalled();
  });

  it.each([
    "No usable saved passphrase was found for this identity file.",
    "This ctld process is not authorized for Keychain access. Use the signed ctld app.",
  ])("shows the manual authentication reason: %s", async (warning) => {
    let prompt: ((value: SshPrompt) => void) | undefined;
    vi.mocked(probeSshHost).mockImplementation((_target, _attempt, callback) => {
      prompt = callback;
      return new Promise(() => undefined);
    });
    const { user } = setup();
    await details(user);
    await user.click(screen.getByRole("option", { name: /Password \/ interactive/ }));
    await act(async () => prompt?.({
      prompt_id: "manual-key", kind: "secret",
      message: "Enter passphrase for key '/keys/work':", warning,
    }));
    expect(screen.getByRole("status").textContent).toBe(warning);
    expect(screen.getByText("Enter passphrase for key '/keys/work':")).toBeTruthy();
    expect(screen.getByLabelText("SSH response").getAttribute("type")).toBe("password");
    expect(screen.queryByRole("option", { name: /^Yes/ })).toBeNull();
    await user.type(screen.getByLabelText("SSH response"), "manual-secret{Enter}");
    expect(respondSshPrompt).toHaveBeenCalledWith(expect.any(String), "manual-key", "manual-secret");
    expect(screen.queryByText(warning)).toBeNull();
  });

  it("offers yes, no, and never after SSH authentication without exposing the secret", async () => {
    let prompt: ((value: SshPrompt) => void) | undefined;
    vi.mocked(probeSshHost).mockImplementation(
      (_target, _attempt, callback) => {
        prompt = callback;
        return new Promise(() => undefined);
      },
    );
    const { user } = setup();
    await details(user);
    await user.click(
      screen.getByRole("option", { name: /Password \/ interactive/ }),
    );
    await act(async () =>
      prompt?.({
        prompt_id: "save-credential",
        kind: "credential_save",
        message: "Save this SSH credential?",
      }),
    );

    expect(screen.getByRole("option", { name: /^Yes/ })).toBeTruthy();
    expect(screen.getByRole("option", { name: /^No/ })).toBeTruthy();
    expect(screen.getByRole("option", { name: /^Never/ })).toBeTruthy();
    expect(screen.queryByRole("textbox")).toBeNull();
    await user.click(screen.getByRole("option", { name: /^Never/ }));
    expect(respondSshPrompt).toHaveBeenCalledWith(
      expect.any(String),
      "save-credential",
      "never",
    );
  });

  it("continues the verified connection after acknowledging a Keychain save error", async () => {
    let prompt: ((value: SshPrompt) => void) | undefined;
    vi.mocked(probeSshHost).mockImplementation(
      (_target, _attempt, callback) => {
        prompt = callback;
        return new Promise(() => undefined);
      },
    );
    const { user } = setup();
    await details(user);
    await user.click(
      screen.getByRole("option", { name: /Password \/ interactive/ }),
    );
    await act(async () =>
      prompt?.({
        prompt_id: "save-error",
        kind: "credential_save_error",
        message: "Connected, but the credential was not saved.",
      }),
    );

    await user.click(screen.getByRole("button", { name: "Continue" }));
    expect(respondSshPrompt).toHaveBeenCalledWith(
      expect.any(String),
      "save-error",
      "confirm",
    );
  });

  it("keeps preflight errors visible and lets the user backtrack", async () => {
    vi.mocked(probeSshHost).mockRejectedValue({
      message: "ctl-agent: command not found",
    });
    const { user, save } = setup();
    await details(user);
    await user.click(
      screen.getByRole("option", { name: /SSH config \/ agent/ }),
    );
    await waitFor(() =>
      expect(screen.getByRole("alert").textContent).toContain("ctl-agent"),
    );
    await user.click(screen.getByRole("button", { name: "Previous step" }));
    expect(
      screen.getByRole("dialog", { name: "Authentication · 4/4" }),
    ).toBeTruthy();
    expect(save).not.toHaveBeenCalled();
  });

  it("installs a missing remote agent bundle and retries the connection", async () => {
    vi.mocked(probeSshHost)
      .mockRejectedValueOnce({
        code: "ctl_agent_not_found",
        message: "ctl-agent: command not found",
      })
      .mockResolvedValueOnce(remoteInfo);
    vi.mocked(installRemoteAgent).mockResolvedValue({
      app_version: "0.1.0",
      bundle_id: "0.1.0-dev.0123456789ab",
      git_revision: "0123456789abcdef0123456789abcdef01234567",
      target_triple: "x86_64-unknown-linux-musl",
    });
    const { user } = setup();
    await details(user);
    await user.click(
      screen.getByRole("option", { name: /SSH config \/ agent/ }),
    );
    await user.click(
      await screen.findByRole("option", { name: /Install remote components/ }),
    );
    await screen.findByRole("dialog", { name: "Save host" });
    expect(installRemoteAgent).toHaveBeenCalledWith(
      expect.objectContaining({ destination: "ctmux-test" }),
      expect.any(String),
      expect.any(Function),
      expect.any(Function),
    );
    expect(probeSshHost).toHaveBeenCalledTimes(2);
  });

  it("updates the failing VPN's SSH owner while preserving destination identity and reconnect callbacks", async () => {
    const { target, owner, failure } = remoteVpnRecoveryFixture();
    const candidate = { ...target, remote_info: remoteInfo };
    vi.mocked(probeSshHost).mockRejectedValueOnce(failure).mockResolvedValueOnce(remoteInfo);
    vi.mocked(installRemoteAgent).mockResolvedValueOnce(installedBundle);
    const onVerified = vi.fn(async () => null);
    const onConnected = vi.fn();
    const onConnectionChange = vi.fn();
    const onClose = vi.fn();
    render(<StrictMode><SshHostFlow suggestions={[]} warning={null} target={target} autoConnect expectedIdentity={remoteInfo}
      onVerified={onVerified} onConnected={onConnected} onConnectionChange={onConnectionChange} onClose={onClose} /></StrictMode>);
    const user = userEvent.setup();
    await user.click(await screen.findByRole("option", { name: /^Update components on Jump host/ }));
    await waitFor(() => expect(onConnected).toHaveBeenCalledExactlyOnceWith(candidate));
    expect(installRemoteAgent).toHaveBeenCalledExactlyOnceWith(owner, expect.any(String), expect.any(Function), expect.any(Function));
    expect(probeSshHost).toHaveBeenNthCalledWith(1, candidate, expect.any(String), expect.any(Function));
    expect(probeSshHost).toHaveBeenNthCalledWith(2, candidate, expect.any(String), expect.any(Function));
    expect(onVerified).toHaveBeenCalledExactlyOnceWith(candidate, remoteInfo);
    for (const [changed] of onConnectionChange.mock.calls) expect(changed).toEqual(candidate);
    expect(onClose).toHaveBeenCalledOnce();
    expect(forgetSshCredentials).not.toHaveBeenCalled();
  });

  it("retries a failed owner installation on that same owner before reconnecting the destination", async () => {
    const { target, owner, failure } = remoteVpnRecoveryFixture();
    vi.mocked(probeSshHost).mockRejectedValueOnce(failure).mockResolvedValueOnce(remoteInfo);
    vi.mocked(installRemoteAgent).mockRejectedValueOnce({ code: "remote_agent_install_stalled", message: "Owner bundle transfer stalled." })
      .mockResolvedValueOnce(installedBundle);
    const onConnected = vi.fn();
    render(<SshHostFlow suggestions={[]} warning={null} target={target} autoConnect
      onVerified={async () => null} onConnected={onConnected} onClose={vi.fn()} />);
    const user = userEvent.setup();
    await user.click(await screen.findByRole("option", { name: /^Update components on Jump host/ }));
    expect(await screen.findByText("Owner bundle transfer stalled.")).toBeTruthy();
    expect(probeSshHost).toHaveBeenCalledOnce();
    expect(screen.queryByRole("option", { name: "Install remote components" })).toBeNull();
    await user.click(screen.getByRole("option", { name: /^Update components on Jump host/ }));
    await waitFor(() => expect(onConnected).toHaveBeenCalledOnce());
    expect(installRemoteAgent).toHaveBeenNthCalledWith(1, owner, expect.any(String), expect.any(Function), expect.any(Function));
    expect(installRemoteAgent).toHaveBeenNthCalledWith(2, owner, expect.any(String), expect.any(Function), expect.any(Function));
    expect(probeSshHost).toHaveBeenNthCalledWith(2, target, expect.any(String), expect.any(Function));
  });

  it("guides prefix VPN sign-in after an owner update fails and clears that owner on a fresh connection", async () => {
    const { target, owner, failure } = remoteVpnRecoveryFixture();
    vi.mocked(probeSshHost).mockRejectedValueOnce(failure)
      .mockRejectedValueOnce({ code: "ssh_failed", message: "The destination is unreachable." });
    vi.mocked(installRemoteAgent).mockRejectedValueOnce({
      code: "remote_vpn_sign_in_required", message: "The preceding SSH host's VPN requires sign-in.",
    });
    render(<SshHostFlow suggestions={[]} warning={null} target={target} autoConnect onClose={vi.fn()} />);
    const user = userEvent.setup();
    await user.click(await screen.findByRole("option", { name: /^Update components on Jump host/ }));
    expect(await screen.findByText("The preceding SSH host's VPN requires sign-in.")).toBeTruthy();
    expect(screen.getByText("Sign in to the VPN on its SSH host. Open this connection route and check the remote VPN status to sign in, then choose Connect to continue.")).toBeTruthy();
    expect(screen.getByRole("option", { name: /^Update components on Jump host/ })).toBeTruthy();
    expect(installRemoteAgent).toHaveBeenCalledExactlyOnceWith(owner, expect.any(String), expect.any(Function), expect.any(Function));
    await user.click(screen.getByRole("option", { name: "Connect" }));
    expect(await screen.findByText("The destination is unreachable.")).toBeTruthy();
    expect(screen.queryByRole("option", { name: /Update components on/ })).toBeNull();
    expect(screen.queryByText(/Sign in to the VPN on its SSH host/)).toBeNull();
    expect(probeSshHost).toHaveBeenNthCalledWith(2, target, expect.any(String), expect.any(Function));
    expect(installRemoteAgent).toHaveBeenCalledOnce();
  });

  it("cancels owner installation without retrying the destination or forgetting saved hop credentials", async () => {
    const { target, owner, failure } = remoteVpnRecoveryFixture();
    vi.mocked(probeSshHost).mockRejectedValueOnce(failure);
    let complete_install!: (bundle: typeof installedBundle) => void;
    vi.mocked(installRemoteAgent).mockImplementationOnce(() => new Promise((resolve) => { complete_install = resolve; }));
    const onConnected = vi.fn();
    const onVerified = vi.fn(async () => null);
    const onConnectionChange = vi.fn();
    const onClose = vi.fn();
    render(<SshHostFlow suggestions={[]} warning={null} target={target} autoConnect onVerified={onVerified}
      onConnected={onConnected} onConnectionChange={onConnectionChange} onClose={onClose} />);
    const user = userEvent.setup();
    await user.click(await screen.findByRole("option", { name: /^Update components on Jump host/ }));
    expect(installRemoteAgent).toHaveBeenCalledExactlyOnceWith(owner, expect.any(String), expect.any(Function), expect.any(Function));
    const attempt = vi.mocked(installRemoteAgent).mock.lastCall![1];
    await user.keyboard("{Escape}");
    expect(cancelSshProbe).toHaveBeenCalledWith(attempt);
    expect(onConnectionChange).toHaveBeenLastCalledWith(target, "cancelled");
    await act(async () => { complete_install(installedBundle); });
    expect(probeSshHost).toHaveBeenCalledOnce();
    expect(onVerified).not.toHaveBeenCalled();
    expect(onConnected).not.toHaveBeenCalled();
    expect(onClose).toHaveBeenCalledOnce();
    expect(forgetSshCredentials).not.toHaveBeenCalled();
  });

  it.each([
    { code: "remote_vpn_components_update_required", vpn_route_index: undefined },
    { code: "remote_vpn_components_update_required", vpn_route_index: "2" },
    { code: "remote_vpn_components_update_required", vpn_route_index: 0 },
    { code: "remote_vpn_components_update_required", vpn_route_index: 99 },
    { code: "ssh_failed", vpn_route_index: 2 },
  ])("does not offer an owner update for untrusted route metadata (%j)", async (metadata) => {
    const { target } = remoteVpnRecoveryFixture();
    vi.mocked(probeSshHost).mockRejectedValueOnce({ ...metadata, message: "Connection unavailable." });
    render(<SshHostFlow suggestions={[]} warning={null} target={target} autoConnect onClose={vi.fn()} />);
    expect(await screen.findByText("Connection unavailable.")).toBeTruthy();
    expect(screen.queryByRole("option", { name: /Update components on/ })).toBeNull();
    expect(installRemoteAgent).not.toHaveBeenCalled();
  });

  it("shows the current file, receiver progress, speed, and installation stages", async () => {
    vi.mocked(probeSshHost).mockRejectedValueOnce({
      code: "ctl_agent_not_found",
      message: "ctl-agent: command not found",
    });
    let report!: (progress: RemoteAgentInstallProgress) => void;
    vi.mocked(installRemoteAgent).mockImplementation((_target, _attempt, _prompt, progress) => {
      report = progress;
      return new Promise(() => undefined);
    });
    const { user, close } = setup();
    await details(user);
    await user.click(screen.getByRole("option", { name: /SSH config \/ agent/ }));
    await user.click(await screen.findByRole("option", { name: /Install remote components/ }));
    const bar = screen.getByRole("progressbar", { name: "Remote component transfer" });
    expect(bar.hasAttribute("value")).toBe(false);
    expect(screen.getByRole("status").textContent).toContain("Detecting remote");
    const progress: RemoteAgentInstallProgress = {
      phase: "transferring",
      file_name: "ctl-agent-bundle-linux.tar.gz",
      transferred_bytes: 2 * 1024 * 1024,
      total_bytes: 8 * 1024 * 1024,
      bytes_per_second: 256 * 1024,
    };
    act(() => report(progress));
    expect(screen.getByRole("status").textContent).toBe("Sending ctl-agent-bundle-linux.tar.gz…");
    expect(bar.getAttribute("value")).toBe(String(progress.transferred_bytes));
    expect(bar.getAttribute("max")).toBe(String(progress.total_bytes));
    expect(screen.getByText("2 MiB / 8 MiB · 25% · 256 KiB/s")).toBeTruthy();
    act(() => report({ ...progress, phase: "extracting", transferred_bytes: progress.total_bytes, bytes_per_second: 0 }));
    expect(screen.getByRole("status").textContent).toContain("Extracting ctl-agent-bundle-linux.tar.gz");
    expect(screen.getByText("8 MiB / 8 MiB · 100% · Transfer complete")).toBeTruthy();
    act(() => report({ ...progress, phase: "checking", file_name: "ctmuxd", transferred_bytes: progress.total_bytes }));
    expect(screen.getByRole("status").textContent).toBe("Checking ctmuxd…");
    const attempt_id = vi.mocked(installRemoteAgent).mock.lastCall![1];
    await user.keyboard("{Escape}");
    expect(close).toHaveBeenCalledOnce();
    expect(cancelSshProbe).toHaveBeenCalledWith(attempt_id);
    act(() => report({ ...progress, phase: "activating" }));
    expect(screen.getByRole("status").textContent).toBe("Checking ctmuxd…");
  });

  it("clears transfer progress on retry and ignores events from the failed attempt", async () => {
    vi.mocked(probeSshHost).mockRejectedValueOnce({ code: "ctl_agent_not_found", message: "Missing" });
    const reporters: ((progress: RemoteAgentInstallProgress) => void)[] = [];
    let reject_install!: (failure: unknown) => void;
    vi.mocked(installRemoteAgent).mockImplementation((_target, _attempt, _prompt, progress) => {
      reporters.push(progress);
      return new Promise((_resolve, reject) => { reject_install = reject; });
    });
    const { user } = setup();
    await details(user);
    await user.click(screen.getByRole("option", { name: /SSH config \/ agent/ }));
    await user.click(await screen.findByRole("option", { name: /Install remote components/ }));
    const progress: RemoteAgentInstallProgress = {
      phase: "transferring", file_name: "old.tar.gz", transferred_bytes: 10, total_bytes: 20, bytes_per_second: 5,
    };
    act(() => reporters[0](progress));
    await act(async () => reject_install({ code: "remote_agent_install_stalled", message: "Transfer stalled while sending old.tar.gz" }));
    expect(screen.getByRole("alert").textContent).toContain("Transfer stalled");
    await user.click(screen.getByRole("option", { name: /Install remote components/ }));
    expect(screen.getByRole("progressbar").hasAttribute("value")).toBe(false);
    act(() => reporters[0](progress));
    expect(screen.getByRole("status").textContent).toContain("Detecting remote");
    expect(screen.queryByText(/Sending old.tar.gz/)).toBeNull();
  });
});
