import { formatText, translate, type DisplayText, type Locale } from "../i18n/messages";
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

function operationLabel(connection: HostConnectionStatus | undefined, locale: Locale): string | null {
  const t = (id: string) => translate(locale, id);
  const operation = connection?.operation;
  if (operation?.kind && operation.state === "pending")
    return operation.kind === "connect" ? t("host_state.connecting") : t("host_state.disconnecting");
  if (operation?.kind && operation.state === "failed")
    return operation.kind === "connect" ? t("host_state.connect_failed") : t("host_state.disconnect_failed");
  if (connection?.state === "connecting") return t("host_state.connecting");
  if (connection?.state === "disconnecting") return t("host_state.disconnecting");
  return null;
}

function connectionLabelId(connection: HostConnectionStatus | undefined, connected: boolean): string {
  if (connection?.manually_disconnected) return "host_state.manual";
  if (connected) return "host_state.connected";
  const observation = connection?.observation;
  if (observation?.stale) return "host_state.unknown";
  if (observation?.availability === "unavailable") return "host_state.disconnected";
  if (observation) return observation.completeness === "pending" ? "host_state.checking" : "host_state.unknown";
  if (connection?.state === "disconnected") return "host_state.disconnected";
  if (connection?.state === "error") return "host_state.error";
  return connection?.state === "checking" || !connection ? "host_state.checking" : "host_state.unknown";
}

function reachabilityLabel(connection: HostConnectionStatus | undefined, locale: Locale): string | null {
  const t = (id: string) => translate(locale, id);
  const reachability = connection?.reachability;
  if (!reachability) return null;
  if (reachability.reason === "vpn_disconnected") return t("host_state.vpn_disconnected");
  return {
    available: t("host_state.available"),
    unavailable: t("host_state.unavailable"),
    checking: t("host_state.checking"),
    not_checked: t("host_state.not_checked"),
    unknown: t("host_state.reachability_unknown"),
  }[reachability.state];
}

/** Connection, endpoint reachability, and session health are independent evidence. */
export function hostPresentation(
  target: ConnectionTarget,
  connection: HostConnectionStatus | undefined,
  session_error?: ErrorDetails,
  locale: Locale = "en",
): HostPresentation {
  const t = (id: string) => translate(locale, id);
  const local = target.kind === "local";
  const observation = local ? undefined : connection?.observation;
  const reachability = local ? undefined : connection?.reachability;
  const operation = local ? undefined : connection?.operation;
  const unavailable = target.kind === "ssh" ? target.unavailable : undefined;
  const lookup_error = session_error?.message && session_error.message !== unavailable ? session_error : undefined;
  const connected = local || Boolean(!connection?.manually_disconnected && !observation?.stale &&
    (observation ? observation.availability === "available" : connection?.state === "connected"));
  const connection_label_id = local ? "host_state.local" : connectionLabelId(connection, connected);
  const connection_label = t(connection_label_id);
  const operation_label = local ? null : operationLabel(connection, locale);
  const reachability_label = local ? null : reachabilityLabel(connection, locale);
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
  else if (connected || reachability?.state === "available") tone = "normal";
  else if (connection?.manually_disconnected) tone = "muted";
  else if (reachability?.state === "checking" || reachability?.state === "unknown" ||
    !connection || connection.state === "checking") tone = "warning";
  else tone = "muted";

  let label = connection_label;
  if (operation_label) label = operation_label;
  else if (unavailable && !connected && target.kind === "ssh") label = connectionUnavailableLabel(target, locale);
  else if (connection_failed) label = t("host_state.error");
  else if (lookup_error) label = t("host_state.lookup_failed");
  else if (!connected && !connection?.manually_disconnected && !observation?.stale && reachability_label)
    label = reachability_label;

  const display = (value: DisplayText) => formatText(locale, value);
  const details: HostPresentation["details"] = [];
  const add = (label: string, value: DisplayText | null | undefined) => {
    if (value) details.push({ label, value: display(value) });
  };
  add(t("host_state.connection"), connection_label);
  add(t("host_state.reachability"), reachability_label);
  add(t("host_state.operation"), operation_label);
  add(t("host_state.operation_method"), operation?.method_name);
  add(t("host_state.operation_details"), operation?.message);
  if (!local) {
    add(t("host_state.connected_methods"), (observation?.method_names ?? connection?.method_names)?.map(display).join(", "));
    add(t("host_state.available_methods"), reachability?.method_names.join(", "));
    add(t("host_state.unknown_methods"), observation?.failed_method_names.map(display).join(", "));
    add(t("host_state.route"), unavailable);
    const connection_message = observation?.message ?? connection?.message;
    if (connection_message !== unavailable && connection_message !== operation?.message)
      add(t("host_state.connection_details"), connection_message);
    if (connection?.message !== connection_message && connection?.message !== unavailable && connection?.message !== operation?.message)
      add(t("host_state.error"), connection?.message);
    add(t("host_state.reachability_details"), reachability?.message);
    if (observation?.completeness === "partial") add(t("host_state.observation"), t("host_state.partial"));
    if (observation?.stale) add(t("host_state.observation"), t("host_state.stale"));
    if (isObservationTime(observation?.checked_at_ms))
      add(t("host_state.connection_checked"), new Date(observation.checked_at_ms!).toLocaleString(locale));
    if (isObservationTime(reachability?.checked_at_ms))
      add(t("host_state.reachability_checked"), new Date(reachability.checked_at_ms!).toLocaleString(locale));
  }
  add(t("host_state.sessions"), lookup_error?.message);
  add(t("host_state.session_code"), lookup_error?.code);
  if (target.kind === "ssh") {
    add(t("host_state.address"), target.destination);
    if (target.hostname && target.hostname !== target.destination) add(t("host_state.hostname"), target.hostname);
    add(t("host_state.user"), target.user);
    add(t("host_state.port"), target.port?.toString());
    add(t("host_state.agent"), target.remote_info?.agent_version);
    add(t("host_state.remote_id"), target.remote_info?.remote_id);
    add(t("host_state.bundle"), target.remote_info?.bundle?.bundle_id);
    add(t("host_state.revision"), target.remote_info?.bundle?.git_revision);
  }

  const description = [...new Set([
    local ? t("host_state.local_hint") : connected
      ? t("host_state.connected_hint")
      : connection?.manually_disconnected ? t("host_state.manual_hint")
      : observation?.stale ? t("host_state.stale_hint")
      : connection_label_id === "host_state.disconnected" ? t("host_state.disconnected_hint")
      : t("host_state.unconfirmed_hint"),
    reachability?.state === "available" ? t("host_state.available_hint") : null,
    operation_label,
    operation?.message,
    unavailable,
    observation?.completeness === "partial" ? t("host_state.partial") : null,
    observation?.message,
    !local ? connection?.message : null,
    lookup_error ? local ? t("host_state.local_lookup_hint") : t("host_state.remote_lookup_hint") : null,
    lookup_error?.message,
  ].flatMap((text) => text ? [display(text)] : []))].join(" ");
  return { connected, tone, label, connection_label, operation_label, details, description };
}
