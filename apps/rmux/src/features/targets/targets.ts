import type {
  ConnectionTarget,
  SessionSummary,
  SshConfigHost,
} from "../../lib/types";

export const LOCAL_TARGET: ConnectionTarget = Object.freeze({ kind: "local" });

const STORAGE_KEY = "rmux.remote_hosts";
const STORAGE_SCHEMA_VERSION = 1;

interface StoredRemoteHosts {
  schema_version: 1;
  ssh_destinations: string[];
}

export function targetKey(target: ConnectionTarget): string {
  return target.kind === "local" ? "local" : `ssh:${target.destination}`;
}

export function targetLabel(target: ConnectionTarget): string {
  return target.kind === "local" ? "local" : target.destination;
}

export function sameTarget(
  left: ConnectionTarget,
  right: ConnectionTarget,
): boolean {
  return targetKey(left) === targetKey(right);
}

export function sessionKey(
  session: Pick<SessionSummary, "target" | "session_id">,
): string {
  return JSON.stringify([targetKey(session.target), session.session_id]);
}

export function targetKeyFromSessionKey(identity: string): string | null {
  try {
    const parsed: unknown = JSON.parse(identity);
    return Array.isArray(parsed) && typeof parsed[0] === "string"
      ? parsed[0]
      : null;
  } catch {
    return null;
  }
}

export function sameSession(
  left: Pick<SessionSummary, "target" | "session_id"> | null | undefined,
  right: Pick<SessionSummary, "target" | "session_id"> | null | undefined,
): boolean {
  return Boolean(left && right && sessionKey(left) === sessionKey(right));
}

export function normalizeSshDestination(destination: string): string | null {
  const normalized = destination.trim();
  if (!normalized || [...normalized].some((character) => isControl(character))) {
    return null;
  }
  return normalized;
}

function isControl(character: string): boolean {
  const codePoint = character.codePointAt(0) ?? 0;
  return codePoint <= 0x1f || (codePoint >= 0x7f && codePoint <= 0x9f);
}

export function loadRemoteTargets(storage: Storage | null): ConnectionTarget[] {
  if (!storage) {
    return [];
  }
  try {
    const encoded = storage.getItem(STORAGE_KEY);
    if (!encoded) {
      return [];
    }
    const parsed: unknown = JSON.parse(encoded);
    if (!isStoredRemoteHosts(parsed)) {
      return [];
    }
    return uniqueDestinations(parsed.ssh_destinations).map((destination) => ({
      kind: "ssh",
      destination,
    }));
  } catch {
    return [];
  }
}

export function saveRemoteTargets(
  storage: Storage | null,
  targets: readonly ConnectionTarget[],
): void {
  if (!storage) {
    return;
  }
  const stored: StoredRemoteHosts = {
    schema_version: STORAGE_SCHEMA_VERSION,
    ssh_destinations: uniqueDestinations(
      targets.flatMap((target) =>
        target.kind === "ssh" ? [target.destination] : [],
      ),
    ),
  };
  try {
    storage.setItem(STORAGE_KEY, JSON.stringify(stored));
  } catch {
    // A privacy-restricted WebView may deny persistence. Host use remains
    // valid for the current app lifetime, so storage failure is non-fatal.
  }
}

export function inactiveSshConfigDestinations(
  hosts: readonly SshConfigHost[],
  targets: readonly ConnectionTarget[],
): string[] {
  const activeDestinations = new Set(
    targets.flatMap((target) =>
      target.kind === "ssh" ? [target.destination] : [],
    ),
  );
  return uniqueDestinations(hosts.map((host) => host.destination)).filter(
    (destination) => !activeDestinations.has(destination),
  );
}

function uniqueDestinations(destinations: readonly string[]): string[] {
  return [
    ...new Set(
      destinations.flatMap((destination) => {
        const normalized = normalizeSshDestination(destination);
        return normalized ? [normalized] : [];
      }),
    ),
  ];
}

function isStoredRemoteHosts(value: unknown): value is StoredRemoteHosts {
  if (!value || typeof value !== "object") {
    return false;
  }
  const candidate = value as Partial<StoredRemoteHosts>;
  return (
    candidate.schema_version === STORAGE_SCHEMA_VERSION &&
    Array.isArray(candidate.ssh_destinations) &&
    candidate.ssh_destinations.every(
      (destination) => typeof destination === "string",
    )
  );
}

export function browserStorage(): Storage | null {
  try {
    return typeof window === "undefined" ? null : window.localStorage;
  } catch {
    return null;
  }
}
