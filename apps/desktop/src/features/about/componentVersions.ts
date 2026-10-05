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
  if (running.version !== installed.version || running.dirty !== installed.dirty || !protocolsMatch(running.protocols, installed.protocols)) return false;
  if (running.source_revision && installed.source_revision && running.source_revision !== installed.source_revision) return false;
  if (running.source_fingerprint && installed.source_fingerprint) return running.source_fingerprint === installed.source_fingerprint;
  return running.source_revision !== null && running.source_revision === installed.source_revision && running.dirty === false;
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
  if (row.running && installed && buildsMatch(row.running, installed) && !row.legacy_protocols?.length) {
    return [{ label: row.observation === "last_observed" ? "Last observed + on disk" : "Running + on disk", info: row.running, legacy: false }];
  }
  const running_label = row.observation === "not_checked" ? "Running · not checked" : row.status === "not_running" ? "Not running" : row.observation === "last_observed" ? "Running · last observed" : "Running";
  return [
    { label: running_label, info: row.running, legacy: true },
    { label: "On disk", info: installed, legacy: false },
  ];
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
  if (rows.some((row) => row.error || row.status === "unavailable" && row.observation !== "not_checked")) statuses.push({ label: "Unavailable", status: "unavailable" });
  if (rows.some((row) => row.observation === "not_checked")) statuses.push({ label: "Not checked", status: "unknown" });
  if (statuses.length) return statuses;
  if (rows.some((row) => row.status === "newer")) return [{ label: "Newer build", status: "newer" }];
  if (rows.some((row) => row.status === "not_running")) return [{ label: "Not running", status: "not_running" }];
  if (separate.length || builds.some(({ views }) => views.some((view) => !view.info)) || rows.some((row) => row.status === "unknown" && row.observation !== "installed") || !protocols.length || protocols.some((protocol) => protocol.status === "unknown")) return [{ label: "Unverified", status: "unknown" }];
  return protocols.some((protocol) => protocol.status === "compatible") ? [{ label: "Compatible protocols", status: "outdated" }] : [{ label: "Current", status: "current" }];
}
