// @vitest-environment jsdom
import { cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";
import type {
  ConnectionTarget,
  HostConnectionStatus,
  ManagedTask,
  SessionSummary,
  ShellStateSummary,
} from "../../lib/types";
import { sessionKey, targetKey } from "../../features/targets/targets";
import { hostFromTarget } from "../../features/workspace/workspaceModel";
import { SessionSidebar } from "./SessionSidebar";

const session: SessionSummary = {
  target: { kind: "local" },
  session_id: "first",
  name: "first",
  status: "running",
  terminal_size: {
    columns: 80,
    rows: 24,
    pixel_width: null,
    pixel_height: null,
  },
  next_sequence: "0",
};

const secondSession: SessionSummary = {
  ...session,
  session_id: "second",
  name: "second",
};

const listedOnlySession: SessionSummary = {
  ...session,
  session_id: "listed-only",
  name: "listed-only",
};

const shellState: ShellStateSummary = {
  shell_type: "zsh",
  cwd: "/Users/clouds/Projects/Tools/ctl/apps/rmux",
  running_command: "cargo test -p rmux-app",
  prompt_phase: "running",
  tui_hint: "inline",
  revision: "1",
  observed_sequence: "1",
};

const remoteHost: ConnectionTarget = {
  kind: "ssh",
  host_id: "build",
  host_name: "Build machine",
  destination: "build-host",
  remote_info: { remote_id: "remote-1", agent_version: "0.1.0" },
};

function renderHostConnection(
  connection?: HostConnectionStatus,
  overrides: Partial<Parameters<typeof SessionSidebar>[0]> = {},
) {
  const props = {
    targets: [session.target, remoteHost],
    targetErrors: new Map(),
    hostConnections: connection ? new Map([["build", connection]]) : undefined,
    sessions: [{ ...session, target: remoteHost }],
    shellStates: new Map(),
    selectedSessionKey: null,
    openTabSessionKeys: new Set<string>(),
    loading: false,
    error: null,
    creating: false,
    closingSessionKeys: new Set<string>(),
    disconnectingSessionKey: null,
    onRefresh: vi.fn(),
    onSelect: vi.fn(),
    onNewShell: vi.fn(),
    onDisconnect: vi.fn(),
    onRequestClose: vi.fn(),
    onAddHost: vi.fn(),
    onConnectHost: vi.fn(),
    onDisconnectHost: vi.fn(),
    onRemoveHost: vi.fn(),
    onAddExisting: vi.fn(),
    onForget: vi.fn(),
    ...overrides,
  };
  render(<SessionSidebar {...props} />);
  return props;
}

afterEach(cleanup);

describe("SessionSidebar", () => {
  it("marks virtual Tailscale provenance separately from SSH connection status and omits removal", () => {
    renderHostConnection({ state: "disconnected", method_names: [], message: null }, {
      hosts: [{ ...hostFromTarget(remoteHost), source: "tailscale" }],
    });
    expect(screen.getByText("Tailscale").title).toBe("Discovered from Tailscale; saved only when customized");
    expect(screen.getByRole("status", { name: "Host connection for Build machine: Disconnected" }).textContent).toBe("Disconnected");
    expect(screen.queryByRole("button", { name: "Remove Build machine" })).toBeNull();
  });

  it("keeps a readable connection status visible when the host is collapsed and disconnects the host independently", async () => {
    const user = userEvent.setup();
    const props = renderHostConnection({
      state: "connected",
      method_names: ["Office network", "VPN"],
      message: null,
    });
    const status = screen.getByRole("status", {
      name: "Host connection for Build machine: Connected",
    });
    expect(status.textContent).toBe("Connected");
    expect(status.title).toContain("Connection methods: Office network, VPN");
    expect(status.querySelector(".host-connection-dot")?.getAttribute("aria-hidden")).toBe("true");
    expect(status.closest("button")).toBeNull();
    expect(screen.queryByRole("button", { name: "Connect to Build machine" })).toBeNull();

    await user.click(screen.getByRole("button", { name: "Collapse Build machine" }));
    expect(status.closest("[hidden]")).toBeNull();
    expect(screen.queryByRole("button", { name: "Shell — first" })).toBeNull();

    const disconnect = screen.getByRole("button", { name: "Disconnect host Build machine" });
    expect(disconnect.title).toBe("Close the shared SSH connection and pause forwards. Remote sessions keep running.");
    await user.click(disconnect);
    expect(props.onDisconnectHost).toHaveBeenCalledExactlyOnceWith(remoteHost);
    expect(props.onDisconnect).not.toHaveBeenCalled();
    expect(props.onRequestClose).not.toHaveBeenCalled();
    expect(props.onRemoveHost).not.toHaveBeenCalled();
    expect(within(screen.getByRole("region", { name: "local sessions" })).queryByRole("status")).toBeNull();
    expect(screen.queryByRole("button", { name: "Disconnect host local" })).toBeNull();
  });

  it.each([
    [undefined, "Checking…"],
    [{ state: "disconnected", method_names: [], message: null } as HostConnectionStatus, "Disconnected"],
  ])("does not infer a live connection from cached identity or running sessions (%s)", async (connection, label) => {
    const user = userEvent.setup();
    const props = renderHostConnection(connection);
    expect(screen.getByRole("status", { name: `Host connection for Build machine: ${label}` }).textContent).toBe(label);
    expect(screen.queryByRole("button", { name: "Disconnect host Build machine" })).toBeNull();
    await user.click(screen.getByRole("button", { name: "Connect to Build machine" }));
    expect(props.onConnectHost).toHaveBeenCalledExactlyOnceWith(remoteHost);
  });

  it.each([
    ["connecting", "Connecting…", "Connect to Build machine"],
    ["disconnecting", "Disconnecting…", "Disconnect host Build machine"],
  ] as const)("blocks repeated actions while %s", async (state, label, actionLabel) => {
    const user = userEvent.setup();
    const props = renderHostConnection({ state, method_names: ["Office network"], message: null }, {
      connectableHostKeys: new Set([targetKey(remoteHost)]),
    });
    expect(screen.getByRole("status", { name: `Host connection for Build machine: ${label}` }).textContent).toBe(label);
    const action = screen.getByRole("button", { name: actionLabel }) as HTMLButtonElement;
    expect(action.disabled).toBe(true);
    await user.click(action);
    expect(props.onConnectHost).not.toHaveBeenCalled();
    expect(props.onDisconnectHost).not.toHaveBeenCalled();
  });

  it("can disconnect known connected methods when another method reports an error", async () => {
    const user = userEvent.setup();
    const props = renderHostConnection({
      state: "error",
      method_names: ["Office network"],
      message: "VPN status check failed",
    });
    const status = screen.getByRole("status", { name: "Host connection for Build machine: Connection error" });
    expect(status.textContent).toBe("Connection error");
    expect(status.title).toContain("Office network");
    expect(status.title).toContain("VPN status check failed");
    await user.click(screen.getByRole("button", { name: "Disconnect host Build machine" }));
    expect(props.onDisconnectHost).toHaveBeenCalledExactlyOnceWith(remoteHost);
  });

  it("offers reconnect after a connection error without an active method", async () => {
    const user = userEvent.setup();
    const props = renderHostConnection({ state: "error", method_names: [], message: "SSH connection lost" });
    expect(screen.queryByRole("button", { name: "Disconnect host Build machine" })).toBeNull();
    await user.click(screen.getByRole("button", { name: "Connect to Build machine" }));
    expect(props.onConnectHost).toHaveBeenCalledExactlyOnceWith(remoteHost);
  });

  it("marks a missing SSH config entry unavailable and prevents connecting", async () => {
    const user = userEvent.setup();
    const unavailable = { ...remoteHost, unavailable: "SSH config alias build-host is missing" };
    const props = renderHostConnection(undefined, { targets: [unavailable] });
    const status = screen.getByRole("status", { name: "Host connection for Build machine: Unavailable" });
    expect(status.textContent).toBe("Unavailable");
    expect(status.title).toContain("SSH config alias build-host is missing");
    const connect = screen.getByRole("button", { name: "Connect to Build machine" }) as HTMLButtonElement;
    expect(connect.disabled).toBe(true);
    await user.click(connect);
    expect(props.onConnectHost).not.toHaveBeenCalled();
  });

  it.each([true, false])("uses saved host availability for Connect while retaining exact-route port availability (connectable: %s)", async (connectable) => {
    const user = userEvent.setup();
    const target = { ...remoteHost, ...(connectable ? { unavailable: "The preferred SSH alias is missing." } : {}) };
    const props = renderHostConnection(undefined, {
      targets: [target],
      connectableHostKeys: new Set(connectable ? [targetKey(target)] : []),
      onPortForward: vi.fn(),
    });
    const connect = screen.getByRole("button", { name: "Connect to Build machine" });
    expect(connect).toHaveProperty("disabled", !connectable);
    await user.click(connect);
    expect(props.onConnectHost).toHaveBeenCalledTimes(connectable ? 1 : 0);
    if (connectable) expect(props.onConnectHost).toHaveBeenCalledWith(target);
    expect(screen.getByRole("button", { name: "Port forwarding for Build machine" })).toHaveProperty("disabled", connectable);
  });

  it("delegates close, add-host, and new-shell interactions without inline forms", () => {
    const markup = renderToStaticMarkup(
      <SessionSidebar
        targets={[session.target]}
        targetErrors={new Map()}
        sessions={[session]}
        shellStates={new Map()}
        selectedSessionKey={sessionKey(session)}
        openTabSessionKeys={new Set([sessionKey(session)])}
        loading={false}
        error={null}
        creating={false}
        closingSessionKeys={new Set()}
        disconnectingSessionKey={null}
        onRefresh={vi.fn()}
        onSelect={vi.fn()}
        onNewShell={vi.fn()}
        onDisconnect={vi.fn()}
        onRequestClose={vi.fn()}
        onAddHost={vi.fn()}
        onConnectHost={vi.fn()}
        onRemoveHost={vi.fn()}
        onAddExisting={vi.fn()}
        onForget={vi.fn()}
      />,
    );

    expect(markup).toContain('aria-label="Terminate first"');
    expect(markup).toContain('aria-label="Add host"');
    expect(markup).not.toContain('aria-label="Add host with gateways"');
    expect(markup).not.toContain("session-close-confirmation");
    expect(markup).not.toContain("host-form");
    expect(markup).toContain("New shell");
    expect(markup).not.toContain("<form");
  });

  it("uses the observed terminal title as the primary label", () => {
    const markup = renderToStaticMarkup(
      <SessionSidebar
        targets={[session.target]}
        targetErrors={new Map()}
        sessions={[session]}
        shellStates={new Map([[sessionKey(session), shellState]])}
        selectedSessionKey={null}
        openTabSessionKeys={new Set()}
        loading={false}
        error={null}
        creating={false}
        closingSessionKeys={new Set()}
        disconnectingSessionKey={null}
        onRefresh={vi.fn()}
        onSelect={vi.fn()}
        onNewShell={vi.fn()}
        onDisconnect={vi.fn()}
        onRequestClose={vi.fn()}
        onAddHost={vi.fn()}
        onConnectHost={vi.fn()}
        onRemoveHost={vi.fn()}
        onAddExisting={vi.fn()}
        onForget={vi.fn()}
      />,
    );

    const fullTitle =
      "/Users/clouds/Projects/Tools/ctl/apps/rmux — cargo test -p rmux-app";
    expect(markup).toContain(`title="${fullTitle}"`);
    expect(markup).toContain("<strong>…pps/rmux — …mux-app</strong>");
    expect(markup).toContain(
      `<small title="${session.name} · running · 80×24">${session.name}<span aria-hidden="true"> · </span>`,
    );
    expect(markup).not.toContain(`<strong>${session.name}</strong>`);
  });

  it("uses a neutral primary label until shell state is observed", () => {
    const markup = renderToStaticMarkup(
      <SessionSidebar
        targets={[session.target]}
        targetErrors={new Map()}
        sessions={[session]}
        shellStates={new Map()}
        selectedSessionKey={null}
        openTabSessionKeys={new Set()}
        loading={false}
        error={null}
        creating={false}
        closingSessionKeys={new Set()}
        disconnectingSessionKey={null}
        onRefresh={vi.fn()}
        onSelect={vi.fn()}
        onNewShell={vi.fn()}
        onDisconnect={vi.fn()}
        onRequestClose={vi.fn()}
        onAddHost={vi.fn()}
        onConnectHost={vi.fn()}
        onRemoveHost={vi.fn()}
        onAddExisting={vi.fn()}
        onForget={vi.fn()}
      />,
    );

    expect(markup).toContain("<strong>Shell</strong>");
    expect(markup).toContain(session.name);
  });

  it("shows Disconnect for every open tab, not merely the active attachment", () => {
    const markup = renderToStaticMarkup(
      <SessionSidebar
        targets={[session.target]}
        targetErrors={new Map()}
        sessions={[session, secondSession, listedOnlySession]}
        shellStates={new Map()}
        selectedSessionKey={sessionKey(session)}
        openTabSessionKeys={
          new Set([sessionKey(session), sessionKey(secondSession)])
        }
        loading={false}
        error={null}
        creating={false}
        closingSessionKeys={new Set()}
        disconnectingSessionKey={null}
        onRefresh={vi.fn()}
        onSelect={vi.fn()}
        onNewShell={vi.fn()}
        onDisconnect={vi.fn()}
        onRequestClose={vi.fn()}
        onAddHost={vi.fn()}
        onConnectHost={vi.fn()}
        onRemoveHost={vi.fn()}
        onAddExisting={vi.fn()}
        onForget={vi.fn()}
      />,
    );

    expect(markup).toContain('aria-label="Disconnect from first"');
    expect(markup).toContain('aria-label="Disconnect from second"');
    expect(markup).not.toContain('aria-label="Disconnect from listed-only"');
  });

  it("keeps empty hosts actionable and supports keyboard disclosure without disconnecting sessions", async () => {
    const user = userEvent.setup();
    const remote: ConnectionTarget = {
      kind: "ssh",
      destination: "build-host",
      remote_info: { remote_id: "remote-1", agent_version: "0.1.0" },
    };
    const onConnectHost = vi.fn();
    const onPortForward = vi.fn();
    const onRemoveHost = vi.fn();
    const onDisconnect = vi.fn();
    const onAddHost = vi.fn();
    const onHostSettings = vi.fn();
    render(
      <SessionSidebar
        targets={[session.target, remote]}
        targetErrors={new Map()}
        sessions={[session]}
        shellStates={new Map()}
        selectedSessionKey={sessionKey(session)}
        openTabSessionKeys={new Set([sessionKey(session)])}
        loading={false}
        error={null}
        creating={false}
        closingSessionKeys={new Set()}
        disconnectingSessionKey={null}
        onRefresh={vi.fn()}
        onSelect={vi.fn()}
        onNewShell={vi.fn()}
        onDisconnect={onDisconnect}
        onRequestClose={vi.fn()}
        onAddHost={onAddHost}
        onHostSettings={onHostSettings}
        onConnectHost={onConnectHost}
        onRemoveHost={onRemoveHost}
        onPortForward={onPortForward}
        onAddExisting={vi.fn()}
        onForget={vi.fn()}
      />,
    );

    const localGroup = screen.getByRole("button", { name: "Collapse local" });
    localGroup.focus();
    await user.keyboard("{Enter}");
    expect(localGroup.getAttribute("aria-expanded")).toBe("false");
    expect(screen.queryByRole("button", { name: "Shell — first" })).toBeNull();
    expect(onDisconnect).not.toHaveBeenCalled();

    await user.keyboard(" ");
    expect(localGroup.getAttribute("aria-expanded")).toBe("true");
    expect(screen.getByRole("button", { name: "Shell — first" })).toBeTruthy();
    expect(screen.getByText("No known sessions")).toBeTruthy();
    const connect = screen.getByRole("button", { name: "Connect to build-host" });
    expect(connect.title).toContain("Agent 0.1.0\nRemote ID: remote-1");
    await user.click(connect);
    await user.click(screen.getByRole("button", { name: "Port forwarding for build-host" }));
    await user.click(screen.getByRole("button", { name: "Remove build-host" }));
    expect(onConnectHost).toHaveBeenCalledWith(remote);
    expect(onPortForward).toHaveBeenCalledWith(remote);
    expect(onRemoveHost).toHaveBeenCalledWith(remote);

    await user.click(screen.getByRole("button", { name: "Host settings for build-host" }));
    expect(onHostSettings).toHaveBeenCalledWith(remote);
    await user.click(screen.getByRole("button", { name: "Add host" }));
    expect(onAddHost).toHaveBeenCalledOnce();
    expect(screen.queryByRole("button", { name: "Add host with gateways" })).toBeNull();
  });

  it("groups ordinary sessions by host and lists active interactive tasks separately", () => {
    const remote = { kind: "ssh" as const, destination: "build-host" };
    const remoteSession = { ...secondSession, target: remote };
    const taskSession = { ...listedOnlySession, session_id: "task-session" };
    const task: ManagedTask = {
      task_id: "task-1",
      definition: {
        name: "Dev server",
        program: "cargo",
        arguments: ["run"],
        working_directory: null,
        execution_mode: "interactive",
      },
      desired_state: "running",
      active_run: {
        run_id: "run-1",
        state: "running",
        started_at_ms: 1,
        ended_at_ms: null,
        exit_code: null,
        interactive: {
          session_id: "task-session",
          instance_id: "rmux-1",
          rmux_socket: "/tmp/rmux.sock",
          released: false,
        },
      },
      last_run: null,
    };
    const markup = renderToStaticMarkup(
      <SessionSidebar
        targets={[session.target, remote]}
        targetErrors={new Map()}
        sessions={[session, remoteSession, taskSession]}
        interactiveTasks={[task]}
        shellStates={new Map()}
        selectedSessionKey="task:local:task-1"
        openTabSessionKeys={new Set()}
        loading={false}
        error={null}
        creating={false}
        closingSessionKeys={new Set()}
        disconnectingSessionKey={null}
        onRefresh={vi.fn()}
        onSelect={vi.fn()}
        onNewShell={vi.fn()}
        onDisconnect={vi.fn()}
        onRequestClose={vi.fn()}
        onAddHost={vi.fn()}
        onConnectHost={vi.fn()}
        onRemoveHost={vi.fn()}
        onAddExisting={vi.fn()}
        onForget={vi.fn()}
        onSelectTask={vi.fn()}
        onStopTask={vi.fn()}
      />,
    );

    expect(markup).toContain('id="session-group-tasks"');
    expect(markup).toContain("Dev server");
    expect(markup).toContain('aria-label="local sessions"');
    expect(markup).toContain('aria-label="build-host sessions"');
    expect(markup).not.toContain("Shell — listed-only");
    expect(markup.indexOf("session-group-tasks")).toBeLessThan(
      markup.indexOf('aria-label="local sessions"'),
    );
  });
});
