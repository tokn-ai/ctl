// @vitest-environment jsdom
import { act, cleanup, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";
import type {
  ConnectionTarget,
  HostConnectionStatus,
  HostReachabilityObservation,
  ManagedTask,
  SessionSummary,
  ShellStateSummary,
} from "../../lib/types";
import { sessionKey, targetKey } from "../../features/targets/targets";
import { hostFromTarget } from "../../features/workspace/workspaceModel";
import { TAILSCALE_UNAVAILABLE } from "../../features/workspace/tailscaleProjection";
import { errorDetails } from "../../lib/errors";
import { initialAttachmentState } from "../../features/attachment/attachmentState";
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
  cwd: "/Users/clouds/Projects/Tools/ctl/apps/desktop",
  running_command: "cargo test -p ctmux-app",
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

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("SessionSidebar", () => {
  it.each(["idle", "connecting", "reconnecting", "retry_wait", "disconnected", "error", "ended"] as const)(
    "shows compact observed age alongside the %s connection state", (phase) => {
      vi.useFakeTimers();
      vi.setSystemTime("2026-10-01T12:00:00.000Z");
      const known = { ...session, last_seen_at_ms: Date.parse("2026-10-01T10:00:00.000Z") };
      renderHostConnection(undefined, {
        sessions: [known],
        attachmentStates: new Map([[sessionKey(known), { ...initialAttachmentState(), phase, session: known }]]),
      });
      const row = screen.getByRole("button", { name: "Shell — first" });
      const age = within(row).getByText("2h ago");
      expect(age.tagName).toBe("TIME");
      expect(age.closest(".session-age")?.parentElement).toBe(row.querySelector("small"));
      expect(age.closest(".session-detail-label")).toBeNull();
      expect(age.getAttribute("datetime")).toBe("2026-10-01T10:00:00.000Z");
      expect(age.title).toBe("Last observed by this app: 2026-10-01T10:00:00.000Z");
      expect(row.getAttribute("aria-description")).toBe(age.title);
      expect(row.querySelector(".session-status")?.getAttribute("data-status")).toBe(phase);
      expect(row.querySelector("small")?.title).toContain("80×24");
      expect(row.textContent).not.toContain("80×24");
      expect(row.textContent).not.toContain("Seen 2h ago");
      expect(row.textContent).not.toContain("Last seen 2h ago");
    },
  );

  it("keeps attached metadata without an age or relative-time timer", () => {
    vi.useFakeTimers();
    vi.setSystemTime("2026-10-01T12:00:00.000Z");
    const known = { ...session, last_seen_at_ms: Date.parse("2026-10-01T10:00:00.000Z") };
    renderHostConnection(undefined, {
      sessions: [known],
      attachmentStates: new Map([[sessionKey(known), { ...initialAttachmentState(), phase: "attached", session: known }]]),
    });
    const row = screen.getByRole("button", { name: "Shell — first" });
    expect(within(row).getByText("Attached")).toBeTruthy();
    expect(row.querySelector("time")).toBeNull();
    expect(row.querySelector("small")?.title).toContain("80×24");
    expect(vi.getTimerCount()).toBe(0);
  });

  it("does not present restored placeholder dimensions or invent last-seen time", () => {
    vi.useFakeTimers();
    renderHostConnection(undefined, { sessions: [session, { ...secondSession, last_seen_at_ms: NaN }] });
    for (const name of ["first", "second"]) {
      const row = screen.getByRole("button", { name: `Shell — ${name}` });
      expect(row.querySelector("time")).toBeNull();
      expect(row.querySelector("small")?.title).not.toContain("80×24");
      expect(row.getAttribute("aria-description")).toBeNull();
    }
    expect(vi.getTimerCount()).toBe(0);
  });

  it("preserves unknown size provenance even when a last-seen timestamp is saved", () => {
    vi.useFakeTimers();
    vi.setSystemTime("2026-10-01T12:00:00.000Z");
    renderHostConnection(undefined, { sessions: [{
      ...session, terminal_size_known: false, last_seen_at_ms: Date.parse("2026-10-01T10:00:00.000Z"),
    }] });
    const row = screen.getByRole("button", { name: "Shell — first" });
    expect(within(row).getByText("2h ago")).toBeTruthy();
    expect(row.querySelector("small")?.title).not.toContain("80×24");
  });

  it("refreshes every unattached age with one sidebar timer and releases it on close", () => {
    vi.useFakeTimers();
    vi.setSystemTime("2026-10-01T12:00:00.000Z");
    renderHostConnection(undefined, { sessions: [
      { ...session, last_seen_at_ms: Date.parse("2026-10-01T11:00:30.000Z") },
      { ...secondSession, last_seen_at_ms: Date.parse("2026-10-01T10:00:30.000Z") },
    ] });
    expect(screen.getByText("59m ago")).toBeTruthy();
    expect(screen.getByText("1h ago")).toBeTruthy();
    expect(vi.getTimerCount()).toBe(1);
    act(() => vi.advanceTimersByTime(60_000));
    expect(screen.queryByText("59m ago")).toBeNull();
    expect(screen.getByText("1h ago")).toBeTruthy();
    expect(screen.getByText("2h ago")).toBeTruthy();
    cleanup();
    expect(vi.getTimerCount()).toBe(0);
  });

  it.each([
    [{ state: "available", reason: null }, "SSH available"],
    [{ state: "unavailable", reason: "connection_refused" }, "SSH unavailable"],
    [{ state: "not_checked", reason: "vpn_disconnected" }, "VPN disconnected"],
    [{ state: "not_checked", reason: "route_requires_connection" }, "SSH not checked"],
    [{ state: "unknown", reason: "check_failed" }, "SSH status unknown"],
  ] as const)("shows %s reachability without granting connected controls", async (evidence, label) => {
    const user = userEvent.setup();
    const reachability: HostReachabilityObservation = { ...evidence, method_names: evidence.state === "available" ? ["Direct"] : [], message: "Reachability details", checked_at_ms: 1 };
    const props = renderHostConnection({ state: "disconnected", method_names: [], message: "Disconnected manually. Connect this host to resume.",
      manually_disconnected: true, reachability });
    const status = screen.getByRole("status", { name: `Host connection for Build machine: ${label}` });
    expect(status.textContent).toBe(label);
    expect(status.title).toContain("Reachability details");
    expect(status.title).toContain("Disconnected manually");
    if (evidence.state === "available") {
      expect(status.title).toContain("Authentication has not been checked");
      expect(status.getAttribute("data-state")).toBe("available");
    }
    expect(screen.queryByRole("button", { name: "Disconnect host Build machine" })).toBeNull();
    await user.click(screen.getByRole("button", { name: "Connect to Build machine" }));
    expect(props.onConnectHost).toHaveBeenCalledExactlyOnceWith(remoteHost);
  });

  it("keeps a confirmed SSH connection above a failed reachability probe", () => {
    renderHostConnection({ state: "connected", method_names: ["Direct"], message: null,
      reachability: { state: "unavailable", reason: "timed_out", method_names: [], message: null, checked_at_ms: 1 } });
    const status = screen.getByRole("status", { name: "Host connection for Build machine: SSH connected" });
    expect(status.title).toContain("does not freshly verify remote responsiveness");
    expect(screen.getByRole("button", { name: "Disconnect host Build machine" })).toBeDefined();
  });

  it.each(["inspection", "attachment"])("shows a timed-out %s despite an open SSH master", (source) => {
    const remoteSession = { ...session, target: remoteHost };
    const failure = { code: "remote_connection_timeout", message: "Timed out opening the remote ctmux service over SSH. Try again." };
    renderHostConnection({ state: "connected", method_names: ["Direct"], message: null }, {
      targetErrors: source === "inspection" ? new Map([[targetKey(remoteHost), failure]]) : new Map(),
      attachmentStates: source === "attachment" ? new Map([[sessionKey(remoteSession), {
        ...initialAttachmentState(), session: remoteSession, phase: "retry_wait",
        error_code: failure.code, message: failure.message,
      }]]) : new Map(),
    });
    const status = screen.getByRole("status", { name: "Host connection for Build machine: Remote connection timed out" });
    expect(status.title).toContain(failure.message);
    expect(status.title).toContain("local SSH control connection is open");
    expect(screen.getByRole("button", { name: "Disconnect host Build machine" })).toBeDefined();
    expect(screen.queryByRole("status", { name: "Host connection for Build machine: SSH connected" })).toBeNull();
  });

  it.each([
    ["reconnecting", null, "Session reconnecting…"],
    ["error", "automatic_reconnect_timeout", "Session reconnect failed"],
  ] as const)("does not turn a %s attachment into a healthy host badge", (phase, error_code, label) => {
    const remoteSession = { ...session, target: remoteHost };
    renderHostConnection({ state: "connected", method_names: ["Direct"], message: null }, {
      attachmentStates: new Map([[sessionKey(remoteSession), {
        ...initialAttachmentState(), session: remoteSession, phase, error_code,
      }]]),
    });
    expect(screen.getByRole("status", { name: `Host connection for Build machine: ${label}` })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Disconnect host Build machine" })).toBeTruthy();
  });

  it("shows an available alternate route when the preferred Tailscale device is unavailable", async () => {
    const user = userEvent.setup();
    const target = { ...remoteHost, tailscale_node_id: "node-1", unavailable: TAILSCALE_UNAVAILABLE };
    const props = renderHostConnection({ state: "error", method_names: [], message: TAILSCALE_UNAVAILABLE,
      reachability: { state: "available", reason: null, method_names: ["Direct"], message: null, checked_at_ms: 1 } }, {
      targets: [target], connectableHostKeys: new Set([targetKey(target)]),
    });
    const status = screen.getByRole("status", { name: "Host connection for Build machine: SSH available" });
    expect(status.title).toContain("Available methods: Direct");
    expect(status.title).toContain(TAILSCALE_UNAVAILABLE);
    expect(screen.queryByRole("button", { name: "Disconnect host Build machine" })).toBeNull();
    await user.click(screen.getByRole("button", { name: "Connect to Build machine" }));
    expect(props.onConnectHost).toHaveBeenCalledExactlyOnceWith(target);
  });

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
      name: "Host connection for Build machine: SSH connected",
    });
    expect(status.textContent).toBe("SSH connected");
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

  it("retains connected SSH and its controls when the preferred Tailscale route is unavailable", async () => {
    const user = userEvent.setup();
    const target = { ...remoteHost, tailscale_node_id: "node-1", unavailable: TAILSCALE_UNAVAILABLE };
    const props = renderHostConnection({
      state: "connected",
      method_names: ["Office network"],
      message: TAILSCALE_UNAVAILABLE,
      observation: {
        availability: "available", completeness: "partial", method_names: ["Office network"],
        failed_method_names: ["Tailscale"], message: TAILSCALE_UNAVAILABLE,
        checked_at_ms: 1, stale: false,
      },
    }, {
      targets: [target],
      targetErrors: new Map([[targetKey(target), errorDetails(TAILSCALE_UNAVAILABLE)]]),
    });
    const status = screen.getByRole("status", { name: "Host connection for Build machine: SSH connected" });
    expect(status.textContent).toBe("SSH connected");
    expect(status.getAttribute("data-state")).toBe("connected");
    expect(status.title).toContain("Office network");
    expect(status.title).toContain("Some connection methods couldn't be checked.");
    expect(status.title).toContain("Status unknown for: Tailscale");
    expect(status.title).toContain(TAILSCALE_UNAVAILABLE);
    expect(screen.queryByText(TAILSCALE_UNAVAILABLE)).toBeNull();
    await user.click(screen.getByRole("button", { name: "Disconnect host Build machine" }));
    expect(props.onDisconnectHost).toHaveBeenCalledExactlyOnceWith(target);
  });

  it("offers reconnect after a connection error without an active method", async () => {
    const user = userEvent.setup();
    const props = renderHostConnection({ state: "error", method_names: [], message: "SSH connection lost" });
    expect(screen.queryByRole("button", { name: "Disconnect host Build machine" })).toBeNull();
    await user.click(screen.getByRole("button", { name: "Connect to Build machine" }));
    expect(props.onConnectHost).toHaveBeenCalledExactlyOnceWith(remoteHost);
  });

  it.each([
    { binding: {}, reason: "SSH config alias build-host is missing", label: "Unavailable" },
    { binding: { tailscale_node_id: "node-1" }, reason: TAILSCALE_UNAVAILABLE, label: "Tailscale unavailable" },
  ])("keeps an unavailable route disabled with its reason in the $label tooltip", async ({ binding, reason, label }) => {
    const user = userEvent.setup();
    const unavailable = { ...remoteHost, ...binding, unavailable: reason };
    const props = renderHostConnection(undefined, {
      targets: [unavailable],
      targetErrors: new Map([[targetKey(unavailable), errorDetails("Session listing failed")]]),
    });
    const status = screen.getByRole("status", { name: `Host connection for Build machine: ${label}` });
    expect(status.textContent).toBe(label);
    expect(status.title).toContain(reason);
    expect(screen.queryByText(reason)).toBeNull();
    expect(status.title).toContain("Session listing failed");
    expect(screen.queryByText("Session listing failed")).toBeNull();
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
      "/Users/clouds/Projects/Tools/ctl/apps/desktop — cargo test -p ctmux-app";
    expect(markup).toContain(`title="${fullTitle}"`);
    expect(markup).toContain("<strong>…/desktop — …mux-app</strong>");
    expect(markup).toContain(
      `<small class="session-details" title="${session.name} · Last seen running · Session last reported running"><span class="session-detail-label">${session.name}<span aria-hidden="true"> · </span>`,
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
          instance_id: "ctmux-1",
          ctmux_socket: "/tmp/ctmux.sock",
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
