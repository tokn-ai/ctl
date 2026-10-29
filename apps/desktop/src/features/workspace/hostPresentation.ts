import type { ErrorDetails } from "../../lib/errors";
import type { ConnectionTarget, HostConnectionStatus } from "../../lib/types";
import { isObservationTime } from "../sessions/sessionObservation";
import { connectionUnavailableLabel } from "../targets/targets";

export interface HostPresentation {
  connected: boolean;
  tone: "normal" | "muted" | "warning" | "error";
  label: string;
  connection_label: string;
  operation_label: string | null;
  details: { label: string; value: string }[];
  description: string;
}

function operationLabel(connection: HostConnectionStatus | undefined): string | null {
  const operation = connection?.operation;
  if (operation?.kind && operation.state === "pending")
    return operation.kind === "connect" ? "Connecting…" : "Disconnecting…";
  if (operation?.kind && operation.state === "failed")
    return operation.kind === "connect" ? "Connect failed" : "Disconnect failed";
  if (connection?.state === "connecting") return "Connecting…";
  if (connection?.state === "disconnecting") return "Disconnecting…";
  return null;
}

function connectionLabel(connection: HostConnectionStatus | undefined, connected: boolean): string {
  if (connection?.manually_disconnected) return "Disconnected manually";
  if (connected) return "SSH connected";
  const observation = connection?.observation;
  if (observation?.stale) return "SSH status unknown";
  if (observation?.availability === "unavailable") return "SSH disconnected";
  if (observation) return observation.completeness === "pending" ? "Checking SSH…" : "SSH status unknown";
  if (connection?.state === "disconnected") return "SSH disconnected";
  if (connection?.state === "error") return "Connection error";
  return connection?.state === "checking" || !connection ? "Checking SSH…" : "SSH status unknown";
}

function reachabilityLabel(connection: HostConnectionStatus | undefined): string | null {
  const reachability = connection?.reachability;
  if (!reachability) return null;
  if (reachability.reason === "vpn_disconnected") return "VPN disconnected";
  return {
    available: "SSH available",
    unavailable: "SSH unavailable",
    checking: "Checking SSH…",
    not_checked: "SSH not checked",
    unknown: "SSH reachability unknown",
  }[reachability.state];
}

/** Connection, endpoint reachability, and session health are independent evidence. */
export function hostPresentation(
  target: ConnectionTarget,
  connection: HostConnectionStatus | undefined,
  session_error?: ErrorDetails,
): HostPresentation {
  const local = target.kind === "local";
  const observation = local ? undefined : connection?.observation;
  const reachability = local ? undefined : connection?.reachability;
  const operation = local ? undefined : connection?.operation;
  const unavailable = target.kind === "ssh" ? target.unavailable : undefined;
  const lookup_error = session_error?.message && session_error.message !== unavailable ? session_error : undefined;
  const connected = local || Boolean(!connection?.manually_disconnected && !observation?.stale &&
    (observation ? observation.availability === "available" : connection?.state === "connected"));
  const connection_label = local ? "Local" : connectionLabel(connection, connected);
  const operation_label = local ? null : operationLabel(connection);
  const reachability_label = local ? null : reachabilityLabel(connection);
  const pending = operation?.state === "pending" ||
    !local && (connection?.state === "connecting" || connection?.state === "disconnecting");
  const observation_uncertain = Boolean(observation && (observation.stale ||
    observation.completeness !== "complete" || observation.availability === "unknown"));
  const operation_failed = operation?.state === "failed";
  // The compatibility state also reports failed/partial status inspections as
  // "error". Those say the connection is unknown, not that SSH itself failed.
  const connection_failed = !local && connection?.state === "error" && !observation_uncertain;
  let tone: HostPresentation["tone"];
  if (operation_failed) tone = "error";
  else if (unavailable && !connected) tone = "muted";
  else if (connection_failed) tone = "error";
  else if (lookup_error || pending || observation_uncertain || unavailable && connected) tone = "warning";
  else if (connected) tone = "normal";
  else if (connection?.manually_disconnected) tone = "muted";
  else if (reachability?.state === "checking" || reachability?.state === "unknown" ||
    !connection || connection.state === "checking") tone = "warning";
  else tone = "muted";

  let label = connection_label;
  if (operation_label) label = operation_label;
  else if (unavailable && !connected && target.kind === "ssh") label = connectionUnavailableLabel(target);
  else if (connection_failed) label = "Connection error";
  else if (lookup_error) label = "Session lookup failed";
  else if (!connected && !connection?.manually_disconnected && !observation?.stale && reachability_label)
    label = reachability_label;

  const details: HostPresentation["details"] = [];
  const add = (label: string, value: string | null | undefined) => {
    if (value) details.push({ label, value });
  };
  add("Connection", connection_label);
  add("Reachability", reachability_label);
  add("Operation", operation_label);
  add("Operation method", operation?.method_name);
  add("Operation details", operation?.message);
  if (!local) {
    add("Connected methods", (observation?.method_names ?? connection?.method_names)?.join(", "));
    add("Available methods", reachability?.method_names.join(", "));
    add("Status unknown for", observation?.failed_method_names.join(", "));
    add("Route", unavailable);
    const connection_message = observation?.message ?? connection?.message;
    if (connection_message !== unavailable && connection_message !== operation?.message)
      add("Connection details", connection_message);
    if (connection?.message !== connection_message && connection?.message !== unavailable && connection?.message !== operation?.message)
      add("Connection error", connection?.message);
    add("Reachability details", reachability?.message);
    if (observation?.completeness === "partial") add("Observation", "Some connection methods couldn't be checked.");
    if (observation?.stale) add("Observation", "Connection settings changed; the previous check is no longer current.");
    if (isObservationTime(observation?.checked_at_ms))
      add("SSH connection checked", new Date(observation.checked_at_ms!).toLocaleString());
    if (isObservationTime(reachability?.checked_at_ms))
      add("SSH reachability checked", new Date(reachability.checked_at_ms!).toLocaleString());
  }
  add("Sessions", lookup_error?.message);
  add("Session error code", lookup_error?.code);
  if (target.kind === "ssh") {
    add("Address", target.destination);
    if (target.hostname && target.hostname !== target.destination) add("Hostname", target.hostname);
    add("User", target.user);
    add("Port", target.port?.toString());
    add("Agent", target.remote_info?.agent_version);
    add("Remote ID", target.remote_info?.remote_id);
    add("Bundle", target.remote_info?.bundle?.bundle_id);
    add("Revision", target.remote_info?.bundle?.git_revision);
  }

  const description = [...new Set([
    local ? "Local workspace." : connected
      ? "The local SSH control connection is open. This check does not freshly verify remote responsiveness or terminal health."
      : connection?.manually_disconnected ? "This host is paused by a manual disconnect."
      : observation?.stale ? "The previous SSH connection observation is no longer current."
      : connection_label === "SSH disconnected" ? "No open SSH control connection was observed."
      : "An open SSH control connection has not been confirmed.",
    reachability?.state === "available" ? "The endpoint answered with an SSH greeting. Authentication has not been checked." : null,
    operation_label,
    operation?.message,
    unavailable,
    observation?.completeness === "partial" ? "Some connection methods couldn't be checked." : null,
    observation?.message,
    !local ? connection?.message : null,
    lookup_error ? local ? "The local session lookup failed." : "The session lookup failed independently of the SSH connection status." : null,
    lookup_error?.message,
  ].filter(Boolean))].join(" ");
  return { connected, tone, label, connection_label, operation_label, details, description };
}
