import { describe, expect, it } from "vitest";
import type { HostConnectionObservation, HostConnectionStatus, SshConnectionTarget } from "../../lib/types";
import { hostPresentation } from "./hostPresentation";

const target: SshConnectionTarget = {
  kind: "ssh", host_id: "builder", destination: "build", hostname: "build.example.test", user: "alice", port: 2222,
  remote_info: { remote_id: "remote-id", agent_version: "0.1.1" },
};

const observation: HostConnectionObservation = {
  availability: "available", completeness: "complete", method_names: ["Direct"], failed_method_names: [],
  checked_at_ms: 1000, message: null, stale: false,
};

function status(overrides: Partial<HostConnectionStatus> = {}): HostConnectionStatus {
  return { state: "connected", method_names: ["Direct"], message: null, observation, ...overrides };
}

describe("host presentation", () => {
  it("treats local as available and never assigns it SSH metadata", () => {
    const normal = hostPresentation({ kind: "local" }, undefined);
    expect(normal).toMatchObject({ connected: true, tone: "normal", label: "Local", connection_label: "Local", operation_label: null });
    const failed = hostPresentation({ kind: "local" }, status({ state: "error" }), { code: "lookup_failed", message: "Local daemon unavailable" });
    expect(failed).toMatchObject({ connected: true, tone: "warning", label: "Session lookup failed" });
    expect(JSON.stringify(failed)).not.toContain("SSH");
  });

  it("uses current open-master evidence and keeps identity and check details available", () => {
    const result = hostPresentation(target, status());
    expect(result).toMatchObject({ connected: true, tone: "normal", label: "SSH connected", connection_label: "SSH connected" });
    expect(result.description).toContain("does not freshly verify remote responsiveness or terminal health");
    expect(result.details).toEqual(expect.arrayContaining([
      { label: "Connected methods", value: "Direct" },
      { label: "SSH connection checked", value: new Date(1000).toLocaleString() },
      { label: "Remote ID", value: "remote-id" },
      { label: "Agent", value: "0.1.1" },
      { label: "Address", value: "build" },
      { label: "Hostname", value: "build.example.test" },
      { label: "User", value: "alice" },
      { label: "Port", value: "2222" },
    ]));
  });

  it("does not infer an open connection from a reachable endpoint", () => {
    const result = hostPresentation(target, status({
      state: "disconnected", method_names: [], observation: { ...observation, availability: "unavailable", method_names: [] },
      reachability: { state: "available", reason: null, method_names: ["VPN"], message: null, checked_at_ms: 2000 },
    }));
    expect(result).toMatchObject({ connected: false, tone: "muted", label: "SSH available", connection_label: "SSH disconnected" });
    expect(result.description).toContain("Authentication has not been checked");
    expect(result.details).toContainEqual({ label: "SSH reachability checked", value: new Date(2000).toLocaleString() });
  });

  it("keeps partial open-master evidence filled while warning about unknown methods", () => {
    const result = hostPresentation(target, status({ state: "error", observation: {
      ...observation, completeness: "partial", failed_method_names: ["VPN"], message: "VPN broker did not respond",
    } }));
    expect(result).toMatchObject({ connected: true, tone: "warning", label: "SSH connected", connection_label: "SSH connected" });
    expect(result.details).toContainEqual({ label: "Status unknown for", value: "VPN" });
    expect(result.details).toContainEqual({ label: "Connection details", value: "VPN broker did not respond" });
    expect(result.description).toContain("Some connection methods couldn't be checked.");
    expect(result.description).toContain("VPN broker did not respond");
  });

  it("treats failed status inspection as uncertainty rather than a failed SSH connection", () => {
    const result = hostPresentation(target, status({ state: "error", observation: {
      ...observation, availability: "unknown", completeness: "failed", method_names: [], message: "Broker IPC timed out",
    } }));
    expect(result).toMatchObject({ connected: false, tone: "warning", label: "SSH status unknown" });
  });

  it("never fills stale or manually paused connection evidence", () => {
    expect(hostPresentation(target, status({ observation: { ...observation, stale: true } })))
      .toMatchObject({ connected: false, tone: "warning", label: "SSH status unknown" });
    expect(hostPresentation(target, status({ manually_disconnected: true })))
      .toMatchObject({ connected: false, tone: "muted", label: "Disconnected manually" });
  });

  it("keeps an unavailable route muted and a current alternate connection warning", () => {
    const unavailable = { ...target, unavailable: "Tailscale device is unavailable", tailscale_node_id: "node" };
    const matching_error = { code: null, message: unavailable.unavailable };
    expect(hostPresentation(unavailable, { state: "error", method_names: [], message: unavailable.unavailable }, matching_error))
      .toMatchObject({ connected: false, tone: "muted", label: "Tailscale unavailable" });
    const alternate = hostPresentation(unavailable, status({ observation: { ...observation, completeness: "partial" } }), matching_error);
    expect(alternate).toMatchObject({ connected: true, tone: "warning", label: "SSH connected" });
    expect(alternate.details.filter((item) => item.value === unavailable.unavailable)).toEqual([{ label: "Route", value: unavailable.unavailable }]);
  });

  it.each(["connect", "disconnect"] as const)("makes a failed %s operation red even when the route is unavailable", (kind) => {
    const result = hostPresentation({ ...target, unavailable: "Missing route" }, status({
      state: "error", observation: { ...observation, availability: "unavailable" },
      operation: { kind, state: "failed", method_name: "VPN", message: "Operation was rejected" },
    }));
    expect(result).toMatchObject({ connected: false, tone: "error", label: kind === "connect" ? "Connect failed" : "Disconnect failed" });
    expect(result.operation_label).toBe(result.label);
    expect(result.details).toContainEqual({ label: "Operation method", value: "VPN" });
    expect(result.details).toContainEqual({ label: "Operation details", value: "Operation was rejected" });
  });

  it("makes an explicit connection error red without mistaking method names for a live master", () => {
    const result = hostPresentation(target, { state: "error", method_names: ["Direct"], message: "SSH connection lost" });
    expect(result).toMatchObject({ connected: false, tone: "error", label: "Connection error" });
    expect(result.details).toContainEqual({ label: "Connection details", value: "SSH connection lost" });
  });

  it("preserves a healthy master while a new operation is pending", () => {
    expect(hostPresentation(target, status({
      operation: { kind: "connect", state: "pending", method_name: "VPN", message: null },
    }))).toMatchObject({ connected: true, tone: "warning", label: "Connecting…", connection_label: "SSH connected" });
  });

  it("keeps session failures separate from SSH connection health", () => {
    const result = hostPresentation(target, status(), { code: "session_inspection_timeout", message: "Known-session refresh timed out" });
    expect(result).toMatchObject({ connected: true, tone: "warning", label: "Session lookup failed", connection_label: "SSH connected" });
    expect(result.details).toContainEqual({ label: "Sessions", value: "Known-session refresh timed out" });
    expect(result.details).toContainEqual({ label: "Session error code", value: "session_inspection_timeout" });
  });

  it("uses warning for pending or unknown evidence and muted for ordinary disconnection", () => {
    expect(hostPresentation(target, undefined)).toMatchObject({ connected: false, tone: "warning" });
    expect(hostPresentation(target, { state: "disconnected", method_names: [], message: null }))
      .toMatchObject({ connected: false, tone: "muted", label: "SSH disconnected" });
    expect(hostPresentation(target, status({
      state: "disconnected", observation: { ...observation, availability: "unavailable", method_names: [] },
      reachability: { state: "checking", reason: null, method_names: [], message: null, checked_at_ms: null },
    }))).toMatchObject({ connected: false, tone: "warning", label: "Checking SSH…" });
  });

  it("accepts legacy connected state only when a richer observation does not contradict it", () => {
    expect(hostPresentation(target, { state: "connected", method_names: [], message: null }))
      .toMatchObject({ connected: true, tone: "normal" });
    expect(hostPresentation(target, status({ observation: { ...observation, availability: "unavailable", method_names: [] } })))
      .toMatchObject({ connected: false, tone: "muted" });
  });
});
