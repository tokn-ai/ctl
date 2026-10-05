import type { ComponentProtocolVersion, ComponentVersionInfo, ComponentVersionRow } from "../../lib/types";

export interface ComponentHostGroup {
  key: string;
  label: string;
  host_id: string | null;
  rows: ComponentVersionRow[];
}

export type ProtocolStatus = "current" | "compatible" | "incompatible" | "unknown";

export interface ProtocolLine {
  name: string;
  version: string;
  status: ProtocolStatus;
  detail: string;
}

export interface ComponentBuildView {
  label: string;
  info: ComponentVersionInfo | null;
  legacy: boolean;
}

export function groupComponentHosts(rows: readonly ComponentVersionRow[]): ComponentHostGroup[] {
  const groups = new Map<string, ComponentHostGroup>();
  for (const row of rows) {
    const key = row.location === "local" ? "local" : row.host_key ?? (row.host_id ? `saved:${row.host_id}` : row.component_id);
    let group = groups.get(key);
    if (!group) {
      group = { key, label: row.location === "local" ? "This computer" : row.host_name ?? row.host_id ?? row.label, host_id: row.host_id, rows: [] };
      groups.set(key, group);
    }
    group.rows.push(row);
  }
  return [...groups.values()].sort((left, right) => left.key === "local" ? -1 : right.key === "local" ? 1 : left.label.localeCompare(right.label));
}

function protocolsMatch(left: readonly ComponentProtocolVersion[], right: readonly ComponentProtocolVersion[]): boolean {
  return left.length === right.length && left.every((protocol) => right.some((candidate) =>
    candidate.name === protocol.name && candidate.version === protocol.version && candidate.build === protocol.build
    && candidate.supported_versions.length === protocol.supported_versions.length
    && protocol.supported_versions.every((version) => candidate.supported_versions.includes(version))));
}

function buildsMatch(running: ComponentVersionInfo, installed: ComponentVersionInfo): boolean {
  return running.version === installed.version && running.dirty === installed.dirty
    && running.source_revision === installed.source_revision && running.source_fingerprint === installed.source_fingerprint
    && protocolsMatch(running.protocols, installed.protocols);
}

function buildsDiffer(running: ComponentVersionInfo, installed: ComponentVersionInfo): boolean {
  const fields = ["version", "source_revision", "source_fingerprint", "dirty"] as const;
  return fields.some((field) => running[field] !== null && installed[field] !== null && running[field] !== installed[field])
    || running.protocols.some((protocol) => installed.protocols.some((candidate) => candidate.name === protocol.name && !protocolsMatch([protocol], [candidate])));
}

export function componentBuildViews(row: ComponentVersionRow): ComponentBuildView[] {
  // Older local snapshots use available as the installed executable. Remote
  // available may instead describe this app, so it must never become "On disk".
  const installed = row.installed === undefined && row.location === "local" ? row.available : row.installed ?? null;
  if (row.observation === "installed") return [{ label: "On disk · on demand", info: installed, legacy: false }];
  if (!row.running?.version || row.status === "not_running" || row.observation === "not_checked") {
    return [{ label: "On disk", info: installed, legacy: false }];
  }
  if (row.running && installed && buildsMatch(row.running, installed) && !row.legacy_protocols?.length) {
    return [{ label: row.observation === "last_observed" ? "Last observed + on disk" : "Running + on disk", info: row.running, legacy: false }];
  }
  const running_label = row.observation === "last_observed" ? "Running · last observed" : "Running";
  return [
    { label: running_label, info: row.running, legacy: true },
    { label: "On disk", info: installed, legacy: false },
  ];
}

export interface ComponentHostFailure {
  message: string;
  detail: string;
  component_ids: ReadonlySet<string>;
}

const inspection_prerequisites = new Set(["remote_component_inspection_unsupported", "ssh_authentication_required", "ssh_host_disconnected", "remote_identity_unverified"]);

function hostConnected(rows: readonly ComponentVersionRow[]): boolean | null {
  if (rows.some((row) => row.connected === true || row.observation === "last_observed")) return true;
  return rows.some((row) => row.connected === false) ? false : null;
}

/** A failed SSH inspection belongs to the host, not each uninspected binary. */
export function componentHostFailure(rows: readonly ComponentVersionRow[]): ComponentHostFailure | null {
  const errors = rows.filter((row) => row.location === "remote" && row.error);
  const shared = errors.filter((row) => row.observation === "not_checked" || errors.filter((candidate) => candidate.error === row.error).length > 1);
  if (!shared.length) return null;
  const codes = new Set(shared.map((row) => row.error_code));
  const message = codes.has("remote_component_inspection_unsupported") ? "Update the agent to check components."
    : codes.has("remote_identity_unverified") ? "Check host to verify its account."
    : hostConnected(rows) !== true && (codes.has("ssh_authentication_required") || codes.has("ssh_host_disconnected")) ? "Connect this host to check components."
    : "Could not check components. Try Check host.";
  return {
    message,
    detail: [...new Set(shared.map((row) => row.error))].join("\n"),
    component_ids: new Set(shared.map((row) => row.component_id)),
  };
}

export function componentProtocolLines(row: ComponentVersionRow, view: ComponentBuildView): ProtocolLine[] {
  const actual = view.info?.protocols ?? [];
  const required = row.required_protocols ?? row.available?.protocols ?? [];
  const legacy = view.legacy ? row.legacy_protocols ?? [] : [];
  const names = [...new Set([...actual.map((protocol) => protocol.name), ...legacy.map((protocol) => protocol.name), ...required.map((protocol) => protocol.name)])];
  return names.map((name) => {
    const protocol = actual.find((candidate) => candidate.name === name);
    const expected = required.find((candidate) => candidate.name === name);
    const numeric = legacy.find((candidate) => candidate.name === name);
    const requirement = expected ? `Requires ${expected.supported_versions.join(" or ")}.` : "This app does not advertise a requirement for this protocol.";
    if (!protocol) return {
      name, version: numeric ? `legacy ${numeric.version}` : "Not reported", status: "unknown",
      detail: numeric ? `Historical numeric protocol ${numeric.version}; published-contract compatibility is unverified. ${requirement}` : `Protocol not reported. ${requirement}`,
    };
    const compatible = expected?.supported_versions.some((version) => protocol.supported_versions.includes(version));
    const status = !expected ? "unknown" : !compatible ? "incompatible" : protocol.version === expected.version ? "current" : "compatible";
    return { name, version: protocol.version, status, detail: `Advertises ${protocol.version}; build ${protocol.build}; supports ${protocol.supported_versions.join(", ")}. ${requirement}` };
  });
}

export function componentHostStatuses(rows: readonly ComponentVersionRow[]): { label: string; status: string }[] {
  const statuses: { label: string; status: string }[] = [];
  const builds = rows.map((row) => ({ row, views: componentBuildViews(row) }));
  const separate = builds.filter(({ views }) => views.length === 2 && views[0].info && views[1].info);
  const protocols = builds.flatMap(({ row, views }) => views.filter((view) => view.info || view.legacy && row.legacy_protocols?.length).flatMap((view) => componentProtocolLines(row, view)));
  if (rows.some((row) => row.status === "incompatible") || protocols.some((protocol) => protocol.status === "incompatible")) statuses.push({ label: "Protocol mismatch", status: "incompatible" });
  if (rows.some((row) => row.status === "outdated")) statuses.push({ label: "Outdated", status: "outdated" });
  if (rows.some((row) => row.status === "different_build")) statuses.push({ label: "Different build", status: "different_build" });
  if (rows.some((row) => row.restart_required)) statuses.push({ label: "Restart required", status: "different_build" });
  else if (separate.some(({ views }) => buildsDiffer(views[0].info!, views[1].info!))) statuses.push({ label: "Running differs", status: "different_build" });
  if (rows.some((row) => row.error_code === "remote_component_inspection_unsupported")) statuses.push({ label: "Update agent", status: "outdated" });
  if (rows.some((row) => row.error_code === "remote_identity_unverified")) statuses.push({ label: "Verify account", status: "unknown" });
  if (rows.some((row) => row.observation === "not_checked" && row.error && !inspection_prerequisites.has(row.error_code ?? ""))) statuses.push({ label: "Inspection failed", status: "unavailable" });
  if (rows.some((row) => row.observation !== "not_checked" && (row.error || row.status === "unavailable"))) statuses.push({ label: "Unavailable", status: "unavailable" });
  if (rows.some((row) => row.observation === "not_checked")) statuses.push({ label: "Not checked", status: "unknown" });
  const connected = hostConnected(rows);
  const withConnection = (summary: typeof statuses) => connected === null ? summary
    : [{ label: connected ? "Connected" : "Not connected", status: connected ? "current" : "unknown" }, ...summary];
  if (statuses.length) return withConnection(statuses);
  if (rows.some((row) => row.status === "newer")) return withConnection([{ label: "Newer build", status: "newer" }]);
  if (rows.some((row) => row.status === "not_running")) return withConnection([{ label: "Not running", status: "not_running" }]);
  if (separate.length || builds.some(({ views }) => views.some((view) => !view.info)) || rows.some((row) => row.status === "unknown" && row.observation !== "installed") || !protocols.length || protocols.some((protocol) => protocol.status === "unknown")) return withConnection([{ label: "Unverified", status: "unknown" }]);
  return withConnection(protocols.some((protocol) => protocol.status === "compatible") ? [{ label: "Compatible protocols", status: "outdated" }] : [{ label: "Current", status: "current" }]);
}
