// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
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
import { hostColor } from "../../features/workspace/hostColor";
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
  it("keeps empty hosts static and their actions accessible", async () => {
    const user = userEvent.setup();
    const props = renderHostConnection(undefined, { sessions: [] });
    for (const name of ["local", "Build machine"]) {
      const group = screen.getByRole("region", { name: `${name} sessions` });
      expect(group.querySelector(".host-group-toggle")).toBeNull();
      expect(group.querySelector(".host-group-children")).toBeNull();
      expect(group.querySelector(".host-group-count")).toBeNull();
      await user.click(within(group).getByRole("button", { name: `New shell on ${name}` }));
    }
    expect(screen.queryByText("No known sessions")).toBeNull();
    expect(props.onNewShell).toHaveBeenNthCalledWith(1, session.target);
    expect(props.onNewShell).toHaveBeenNthCalledWith(2, remoteHost);
    await user.click(screen.getByRole("button", { name: "Connect to Build machine" }));
    expect(props.onConnectHost).toHaveBeenCalledWith(remoteHost);
  });

  it("uses host colors and shell or actual split-layout icons", () => {
    renderHostConnection(undefined, {
      sessions: [session, { ...secondSession, target: remoteHost }],
      shellStates: new Map([[sessionKey(session), shellState]]),
      session_layouts: new Map([[sessionKey({ ...secondSession, target: remoteHost }), {
        kind: "split", axis: "horizontal", children: [
          { kind: "terminal", terminal_id: "left" },
          { kind: "terminal", terminal_id: "right" },
        ],
      }]]),
    });
    expect(screen.getByRole("img", { name: "Zsh session" })).toBeTruthy();
    expect(screen.getByRole("img", { name: "2-pane session layout" })).toBeTruthy();
    expect(screen.getByRole("img", { name: "Zsh session" }).closest("button")?.getAttribute("aria-description")).toBe("Zsh session");
    expect(screen.getByRole("button", { name: "Shell — second" }).getAttribute("aria-description")).toBe("2-pane session layout");
    for (const target of [session.target, remoteHost]) {
      const group = screen.getByRole("region", { name: `${target.kind === "local" ? "local" : "Build machine"} sessions` });
      const expected = document.createElement("span");
      if (target.kind === "local") {
        expected.style.background = hostColor(target);
        expect((group.querySelector(".host-color-dot") as HTMLElement).style.background).toBe(expected.style.background);
      } else {
        expected.style.color = hostColor(target);
        expect((group.querySelector(".host-color-dot") as HTMLElement).style.color).toBe(expected.style.color);
      }
    }
  });

  it("marks empty-host lookup warnings on the name without adding a status row", () => {
    const message = "Timed out listing sessions.";
    renderHostConnection({ state: "connected", method_names: ["Direct"], message: null }, {
      sessions: [], targetErrors: new Map([[targetKey(remoteHost), errorDetails({ code: "timeout", message })]]),
    });
    const status = screen.getByRole("status", { name: "Host connection for Build machine: Session lookup failed" });
    expect(status.textContent).toBe("Build machine");
    expect(status.getAttribute("data-tone")).toBe("warning");
    expect(status.getAttribute("data-connected")).toBe("true");
    expect(status.getAttribute("aria-description")).toContain(`Sessions: ${message}`);
    expect(document.querySelector(".host-session-error, .host-connection-status")).toBeNull();
    expect(screen.getByRole("region", { name: "Build machine sessions" }).querySelector(".host-group-toggle")).toBeNull();
  });

  it("changes color without toggling the host or its session selection", async () => {
    const user = userEvent.setup();
    const on_host_color_change = vi.fn();
    renderHostConnection(undefined, { on_host_color_change, host_colors: { build: "#91b7f2" } });
    const expand = screen.getByRole("button", { name: "Collapse Build machine" });
    const picker = screen.getByRole("button", { name: "Change color for Build machine" });
    expect(expand.contains(picker)).toBe(false);
    await user.click(picker);
    await user.click(screen.getByRole("button", { name: "Coral" }));
    expect(on_host_color_change).toHaveBeenCalledExactlyOnceWith("build", "#f18f8f");
    expect(expand.getAttribute("aria-expanded")).toBe("true");
    expect(screen.getByRole("button", { name: "Shell — first" })).toBeTruthy();
  });

  it("keeps Local as a fixed white dot even if an old preference is present", () => {
    renderHostConnection(undefined, { host_colors: { local: "#91b7f2" } });
    expect(screen.queryByRole("button", { name: "Change color for Local" })).toBeNull();
    const group = screen.getByRole("region", { name: "local sessions" });
    expect(group.querySelector<HTMLElement>(".host-color-dot")?.style.background).toBe("rgb(255, 255, 255)");
  });

  it.each([
    ["connect", "pending", "Connecting…"],
    ["disconnect", "pending", "Disconnecting…"],
    ["connect", "failed", "Connect failed"],
  ] as const)("colors the host name for %s/%s and keeps operation text in its details", (kind, state, label) => {
    renderHostConnection({
      state: "checking", method_names: [], message: null,
      operation: { kind, state, method_name: "Office", message: "Waiting for the SSH connection" },
    });
    const status = screen.getByRole("status", { name: `Host connection for Build machine: ${label}` });
    expect(status.textContent).toBe("Build machine");
    expect(status.getAttribute("data-tone")).toBe(state === "failed" ? "error" : "warning");
    expect(document.querySelector(".host-connection-status")).toBeNull();
    expect(status.getAttribute("aria-description")).toContain("Checking SSH…");
    expect(status.getAttribute("aria-description")).toContain("Office");
    expect(status.getAttribute("aria-description")).toContain("Waiting for the SSH connection");
  });

  it.each([false, true])("opens host details on keyboard focus with sessions present=%s", (has_sessions) => {
    renderHostConnection({ state: "connected", method_names: ["Office"], message: null }, {
      sessions: has_sessions ? [{ ...session, target: remoteHost }] : [],
    });
    const group = screen.getByRole("region", { name: "Build machine sessions" });
    const trigger = group.querySelector<HTMLElement>(".host-group-toggle, .host-group-label")!;
    Object.defineProperty(trigger, "getBoundingClientRect", { value: () => new DOMRect(10, 20, 180, 30) });
    Object.defineProperty(group.querySelector(".host-group-header"), "getBoundingClientRect", { value: () => new DOMRect(10, 20, 220, 30) });
    expect(screen.queryByRole("tooltip")).toBeNull();
    expect(within(group).queryByText("SSH connected")).toBeNull();
    fireEvent.focus(trigger);
    const card = screen.getByRole("tooltip");
    expect(card.textContent).toContain("SSH connected");
    expect(card.textContent).toContain("Office");
    expect(card.textContent).toContain("remote-1");
    expect(trigger.getAttribute("aria-describedby")).toBe(card.id);
    expect(trigger.hasAttribute("title")).toBe(false);
    fireEvent.keyDown(trigger, { key: "Escape" });
    expect(screen.queryByRole("tooltip")).toBeNull();
  });

  it("keeps unavailable host settings accessible while disabling new shells", async () => {
    const user = userEvent.setup();
    const target = { ...remoteHost, unavailable: "Restore this host" };
    const onHostSettings = vi.fn();
    renderHostConnection(undefined, {
      targets: [target], hosts: [{ ...hostFromTarget(target), source: "unavailable" }], onHostSettings,
    });
    expect((screen.getByRole("button", { name: "New shell on Build machine" }) as HTMLButtonElement).disabled).toBe(true);
    await user.click(screen.getByRole("button", { name: "Host settings for Build machine" }));
    expect(onHostSettings).toHaveBeenCalledWith(target);
  });


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
      expect(row.getAttribute("aria-description")).toContain(age.title);
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
    expect(within(row).getByLabelText("Attached").getAttribute("title")).toContain("80×24");
    expect(row.querySelector("time")).toBeNull();
    expect(row.querySelector("small")).toBeNull();
    expect(vi.getTimerCount()).toBe(0);
  });

  it("does not present restored placeholder dimensions or invent last-seen time", () => {
    vi.useFakeTimers();
    renderHostConnection(undefined, { sessions: [session, { ...secondSession, last_seen_at_ms: NaN }] });
    for (const name of ["first", "second"]) {
      const row = screen.getByRole("button", { name: `Shell — ${name}` });
      expect(row.querySelector("time")).toBeNull();
      expect(row.querySelector("small")?.title).not.toContain("80×24");
      expect(row.getAttribute("aria-description")).toBe("Terminal session");
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
    [{ state: "unknown", reason: "check_failed" }, "SSH reachability unknown"],
  ] as const)("shows %s reachability without granting connected controls", async (evidence, label) => {
    const user = userEvent.setup();
    const reachability: HostReachabilityObservation = { ...evidence, method_names: evidence.state === "available" ? ["Direct"] : [], message: "Reachability details", checked_at_ms: 1 };
    const props = renderHostConnection({ state: "disconnected", method_names: [], message: "Disconnected manually. Connect this host to resume.",
      manually_disconnected: true, reachability });
    const status = screen.getByRole("status", { name: "Host connection for Build machine: Disconnected manually" });
    expect(status.textContent).toBe("Build machine");
    expect(status.getAttribute("aria-description")).toContain("Reachability details");
    expect(status.getAttribute("aria-description")).toContain("Disconnected manually");
    expect(status.getAttribute("aria-description")).toContain(label);
    expect(status.getAttribute("data-tone")).toBe(evidence.state === "available" ? "normal" : "muted");
    if (evidence.state === "available") {
      expect(status.getAttribute("aria-description")).toContain("Authentication has not been checked");
      expect(status.getAttribute("data-connected")).toBe("false");
    }
    expect(screen.queryByRole("button", { name: "Disconnect host Build machine" })).toBeNull();
    await user.click(screen.getByRole("button", { name: "Connect to Build machine" }));
    expect(props.onConnectHost).toHaveBeenCalledExactlyOnceWith(remoteHost);
  });

  it("keeps a confirmed SSH connection above a failed reachability probe", () => {
    renderHostConnection({ state: "connected", method_names: ["Direct"], message: null,
      reachability: { state: "unavailable", reason: "timed_out", method_names: [], message: null, checked_at_ms: 1 } });
    const status = screen.getByRole("status", { name: "Host connection for Build machine: SSH connected" });
    expect(status.getAttribute("aria-description")).toContain("does not freshly verify remote responsiveness");
    expect(screen.getByRole("button", { name: "Disconnect host Build machine" })).toBeDefined();
  });

  it.each(["inspection", "attachment"])("keeps a timed-out %s separate from an open SSH connection", (source) => {
    const remoteSession = { ...session, target: remoteHost };
    const failure = { code: "remote_connection_timeout", message: "Timed out opening the remote ctmux service over SSH. Try again." };
    renderHostConnection({ state: "connected", method_names: ["Direct"], message: null }, {
      targetErrors: source === "inspection" ? new Map([[targetKey(remoteHost), failure]]) : new Map(),
      attachmentStates: source === "attachment" ? new Map([[sessionKey(remoteSession), {
        ...initialAttachmentState(), session: remoteSession, phase: "retry_wait",
        error_code: failure.code, message: failure.message,
      }]]) : new Map(),
    });
    const status = screen.getByRole("status", { name: `Host connection for Build machine: ${source === "inspection" ? "Session lookup failed" : "SSH connected"}` });
    if (source === "inspection") expect(status.getAttribute("aria-description")).toContain(`Sessions: ${failure.message}`);
    else expect(status.getAttribute("aria-description")).not.toContain(failure.message);
    expect(status.getAttribute("aria-description")).toContain("local SSH control connection is open");
    expect(screen.getByRole("button", { name: "Disconnect host Build machine" })).toBeDefined();
    expect(status.getAttribute("data-connected")).toBe("true");
  });

  it.each([
    ["reconnecting", null, "Reconnecting…"],
    ["error", "automatic_reconnect_timeout", "Reconnect failed"],
  ] as const)("keeps a %s attachment status on its session row", (phase, error_code, label) => {
    const remoteSession = { ...session, target: remoteHost };
    renderHostConnection({ state: "connected", method_names: ["Direct"], message: null }, {
      attachmentStates: new Map([[sessionKey(remoteSession), {
        ...initialAttachmentState(), session: remoteSession, phase, error_code,
      }]]),
    });
    expect(screen.getByRole("status", { name: "Host connection for Build machine: SSH connected" })).toBeTruthy();
    expect(screen.getByText(label)).toBeTruthy();
    expect(screen.getByRole("button", { name: "Disconnect host Build machine" })).toBeTruthy();
  });

  it("keeps SSH connected when one session attaches and another fails to reconnect", () => {
    const attached = { ...session, target: remoteHost };
    const failed = { ...attached, session_id: "failed-session", name: "second" };
    const message = "Automatic reconnect attempts stopped. Try reconnecting this session.";
    renderHostConnection({ state: "connected", method_names: ["Direct"], message: null }, {
      sessions: [attached, failed],
      attachmentStates: new Map([
        [sessionKey(attached), { ...initialAttachmentState(), session: attached, phase: "attached" }],
        [sessionKey(failed), { ...initialAttachmentState(), session: failed, phase: "error", error_code: "automatic_reconnect_timeout", message }],
      ]),
    });
    const host = screen.getByRole("status", { name: "Host connection for Build machine: SSH connected" });
    expect(host.getAttribute("data-connected")).toBe("true");
    expect(host.getAttribute("aria-description")).not.toContain(message);
    expect(screen.getByLabelText("Attached")).toBeTruthy();
    const failure = screen.getByText("Reconnect failed");
    expect(failure.closest(".session-details")?.getAttribute("title")).toContain(message);
    expect(screen.queryByText(/session needs attention/i)).toBeNull();
  });

  it("shows an available alternate route when the preferred Tailscale device is unavailable", async () => {
    const user = userEvent.setup();
    const target = { ...remoteHost, tailscale_node_id: "node-1", unavailable: TAILSCALE_UNAVAILABLE };
    const props = renderHostConnection({ state: "error", method_names: [], message: TAILSCALE_UNAVAILABLE,
      reachability: { state: "available", reason: null, method_names: ["Direct"], message: null, checked_at_ms: 1 } }, {
      targets: [target], connectableHostKeys: new Set([targetKey(target)]),
    });
    const status = screen.getByRole("status", { name: "Host connection for Build machine: Tailscale unavailable" });
    expect(status.getAttribute("aria-description")).toContain("Available methods: Direct");
    expect(status.getAttribute("aria-description")).toContain(TAILSCALE_UNAVAILABLE);
    expect(screen.queryByRole("button", { name: "Disconnect host Build machine" })).toBeNull();
    await user.click(screen.getByRole("button", { name: "Connect to Build machine" }));
    expect(props.onConnectHost).toHaveBeenCalledExactlyOnceWith(target);
  });

  it("marks virtual Tailscale provenance separately from SSH connection status and omits removal", () => {
    renderHostConnection({ state: "disconnected", method_names: [], message: null }, {
      hosts: [{ ...hostFromTarget(remoteHost), source: "tailscale" }],
    });
    expect(screen.getByText("Tailscale").title).toBe("Discovered from Tailscale; saved only when customized");
    expect(screen.getByRole("status", { name: "Host connection for Build machine: SSH disconnected" }).textContent).toBe("Build machine");
    expect(screen.queryByRole("button", { name: "Remove Build machine" })).toBeNull();
  });

  it("keeps name status available when collapsed and disconnects the host independently", async () => {
    const user = userEvent.setup();
    const props = renderHostConnection({
      state: "connected",
      method_names: ["Office network", "VPN"],
      message: null,
    });
    const status = screen.getByRole("status", {
      name: "Host connection for Build machine: SSH connected",
    });
    expect(status.textContent).toBe("Build machine");
    expect(status.getAttribute("aria-description")).toContain("Connected methods: Office network, VPN");
    expect(screen.getByRole("button", { name: "Change color for Build machine" }).getAttribute("data-connected")).toBe("true");
    expect(status.closest("button")?.getAttribute("aria-description")).toContain("SSH connected");
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
    expect(within(screen.getByRole("region", { name: "local sessions" })).queryByRole("status")).toBeNull();
    expect(screen.queryByRole("button", { name: "Disconnect host local" })).toBeNull();
  });

  it.each([
    [undefined, "Checking SSH…"],
    [{ state: "disconnected", method_names: [], message: null } as HostConnectionStatus, "SSH disconnected"],
  ])("does not infer a live connection from cached identity or running sessions (%s)", async (connection, label) => {
    const user = userEvent.setup();
    const props = renderHostConnection(connection);
    expect(screen.getByRole("status", { name: `Host connection for Build machine: ${label}` }).textContent).toBe("Build machine");
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
    expect(screen.getByRole("status", { name: `Host connection for Build machine: ${label}` }).textContent).toBe("Build machine");
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
    expect(status.textContent).toBe("Build machine");
    expect(status.getAttribute("aria-description")).toContain("Office network");
    expect(status.getAttribute("aria-description")).toContain("VPN status check failed");
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
    expect(status.textContent).toBe("Build machine");
    expect(status.getAttribute("data-connected")).toBe("true");
    expect(status.getAttribute("aria-description")).toContain("Office network");
    expect(status.getAttribute("aria-description")).toContain("Some connection methods couldn't be checked.");
    expect(status.getAttribute("aria-description")).toContain("Status unknown for: Tailscale");
    expect(status.getAttribute("aria-description")).toContain(TAILSCALE_UNAVAILABLE);
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
    expect(status.textContent).toBe("Build machine");
    expect(status.getAttribute("aria-description")).toContain(reason);
    expect(screen.queryByText(reason)).toBeNull();
    expect(status.getAttribute("aria-description")).toContain("Sessions: Session listing failed");
    expect(status.getAttribute("data-tone")).toBe("muted");
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
        onAddExisting={vi.fn()}
        onForget={vi.fn()}
      />,
    );

    expect(markup).toContain('aria-label="Terminate first"');
    expect(markup).toContain('aria-label="Add or manage sessions"');
    expect(markup).not.toContain('aria-label="Add host with gateways"');
    expect(markup).not.toContain("session-close-confirmation");
    expect(markup).not.toContain("host-form");
    expect(markup).toContain("Add or manage sessions");
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
        onAddExisting={vi.fn()}
        onForget={vi.fn()}
      />,
    );

    const fullTitle =
      "/Users/clouds/Projects/Tools/ctl/apps/desktop — cargo test -p ctmux-app";
    expect(markup).toContain(`title="${fullTitle}"`);
    expect(markup).toContain("<strong>…/desktop — …mux-app</strong>");
    expect(markup).toContain(
      `<small class="session-details" title="${session.name} · Last seen running · Session last reported running"><span class="session-detail-label">`,
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
    const onNewShell = vi.fn();
    const onConnectHost = vi.fn();
    const onPortForward = vi.fn();
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
        onNewShell={onNewShell}
        onDisconnect={onDisconnect}
        onRequestClose={vi.fn()}
        onAddHost={onAddHost}
        onHostSettings={onHostSettings}
        onConnectHost={onConnectHost}
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
    expect(screen.queryByText("No known sessions")).toBeNull();
    expect(screen.queryByRole("button", { name: "Expand build-host" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Collapse build-host" })).toBeNull();
    const connect = screen.getByRole("button", { name: "Connect to build-host" });
    expect(connect.title).toContain("Agent 0.1.0\nRemote ID: remote-1");
    await user.click(connect);
    await user.click(screen.getByRole("button", { name: "Port forwarding for build-host" }));
    expect(screen.queryByRole("button", { name: "Remove build-host" })).toBeNull();
    expect(onConnectHost).toHaveBeenCalledWith(remote);
    expect(onPortForward).toHaveBeenCalledWith(remote);
    await user.click(screen.getByRole("button", { name: "New shell on build-host" }));
    expect(onNewShell).toHaveBeenLastCalledWith(remote);
    await user.click(screen.getByRole("button", { name: "New shell on local" }));
    expect(onNewShell).toHaveBeenLastCalledWith(session.target);

    await user.click(screen.getByRole("button", { name: "Host settings for build-host" }));
    expect(onHostSettings).toHaveBeenCalledWith(remote);
    await user.click(screen.getByRole("button", { name: "Add or manage sessions" }));
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
