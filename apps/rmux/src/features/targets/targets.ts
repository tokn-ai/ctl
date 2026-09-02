import type {
  ConnectionTarget,
  SessionSummary,
  SshConnectionTarget,
  SshConfigHost,
  SshHostDefinition,
} from "../../lib/types";

export const LOCAL_TARGET: ConnectionTarget = Object.freeze({ kind: "local" });

const STORAGE_KEY = "rmux.remote_hosts";
const STORAGE_SCHEMA_VERSION = 2;

interface StoredRemoteHostsV1 {
  schema_version: 1;
  ssh_destinations: string[];
}

interface StoredSshHost {
  destination: string;
  hostname?: string;
  user?: string;
  port?: number;
  identity_file?: string;
}

interface StoredRemoteHostsV2 {
  schema_version: 2;
  ssh_hosts: StoredSshHost[];
}

export function targetKey(target: ConnectionTarget): string {
  return target.kind === "local"
    ? "local"
    : target.host_id
      ? `host:${target.host_id}`
      : `ssh:${target.destination}`;
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
  if (
    !normalized ||
    [...normalized].some((character) => isControl(character))
  ) {
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
    if (isStoredRemoteHostsV1(parsed)) {
      return uniqueDestinations(parsed.ssh_destinations).map((destination) => ({
        kind: "ssh",
        destination,
      }));
    }
    if (!isStoredRemoteHostsV2(parsed)) {
      return [];
    }
    return uniqueSshTargets(
      parsed.ssh_hosts.flatMap((host) => {
        const target = normalizeSshTarget({ kind: "ssh", ...host });
        return target ? [target] : [];
      }),
    );
  } catch {
    return [];
  }
}

/** Only called after successful migration to the native workspace file. */
export function clearLegacyRemoteTargets(storage: Storage | null): void {
  try {
    storage?.removeItem(STORAGE_KEY);
  } catch {
    /* The durable native copy is already saved. */
  }
}

/** Migration must not silently discard malformed or future legacy data. */
export function readLegacyRemoteTargets(
  storage: Storage | null,
): ConnectionTarget[] {
  const encoded = storage?.getItem(STORAGE_KEY);
  if (!encoded) return [];
  const value: unknown = JSON.parse(encoded);
  if (!isStoredRemoteHostsV1(value) && !isStoredRemoteHostsV2(value)) {
    throw new Error(
      "The previous host settings could not be migrated. Their local-storage copy has been preserved.",
    );
  }
  const valid = isStoredRemoteHostsV1(value)
    ? value.ssh_destinations.every(
        (destination) => normalizeSshDestination(destination) !== null,
      )
    : value.ssh_hosts.every(
        (host) => normalizeSshTarget({ kind: "ssh", ...host }) !== null,
      );
  if (!valid) {
    throw new Error(
      "Previous host settings contain invalid entries. Their local-storage copy has been preserved.",
    );
  }
  return loadRemoteTargets(storage);
}

export function appLocalSshTarget(
  definition: SshHostDefinition,
): SshConnectionTarget | null {
  return normalizeSshTarget({
    kind: "ssh",
    destination: definition.alias,
    hostname: definition.hostname,
    ...(definition.user ? { user: definition.user } : {}),
    ...(definition.port !== null ? { port: definition.port } : {}),
    ...(definition.identity_file
      ? { identity_file: definition.identity_file }
      : {}),
  });
}

export function configuredSshTarget(
  destination: string,
): SshConnectionTarget | null {
  const normalized = normalizeSshDestination(destination);
  return normalized ? { kind: "ssh", destination: normalized } : null;
}

function normalizeSshTarget(
  target: SshConnectionTarget,
): SshConnectionTarget | null {
  const destination = normalizeSshDestination(target.destination);
  const hostname = normalizeOptionalToken(target.hostname);
  const user = normalizeOptionalToken(target.user);
  const identityFile = normalizeOptionalValue(target.identity_file);
  const port = target.port;
  if (
    !destination ||
    (target.hostname !== undefined && !hostname) ||
    (target.user !== undefined && !user) ||
    (target.identity_file !== undefined && !identityFile) ||
    (port !== undefined &&
      (!Number.isInteger(port) || port < 1 || port > 65_535))
  ) {
    return null;
  }
  return {
    kind: "ssh",
    destination,
    ...(hostname ? { hostname } : {}),
    ...(user ? { user } : {}),
    ...(port !== undefined ? { port } : {}),
    ...(identityFile ? { identity_file: identityFile } : {}),
  };
}

function normalizeOptionalToken(value: string | undefined): string | null {
  const normalized = normalizeOptionalValue(value);
  if (
    !normalized ||
    [...normalized].some((character) => /\s/u.test(character))
  ) {
    return null;
  }
  return normalized;
}

function normalizeOptionalValue(value: string | undefined): string | null {
  if (value === undefined) {
    return null;
  }
  const normalized = value.trim();
  return normalized &&
    ![...normalized].some((character) => isControl(character))
    ? normalized
    : null;
}

function uniqueSshTargets(
  targets: readonly SshConnectionTarget[],
): SshConnectionTarget[] {
  const seen = new Set<string>();
  return targets.filter((target) => {
    if (seen.has(target.destination)) {
      return false;
    }
    seen.add(target.destination);
    return true;
  });
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

function isStoredRemoteHostsV1(value: unknown): value is StoredRemoteHostsV1 {
  if (!value || typeof value !== "object") {
    return false;
  }
  const candidate = value as Partial<StoredRemoteHostsV1>;
  return (
    candidate.schema_version === 1 &&
    Array.isArray(candidate.ssh_destinations) &&
    candidate.ssh_destinations.every(
      (destination) => typeof destination === "string",
    )
  );
}

function isStoredRemoteHostsV2(value: unknown): value is StoredRemoteHostsV2 {
  if (!value || typeof value !== "object") {
    return false;
  }
  const candidate = value as Partial<StoredRemoteHostsV2>;
  return (
    candidate.schema_version === STORAGE_SCHEMA_VERSION &&
    Array.isArray(candidate.ssh_hosts) &&
    candidate.ssh_hosts.every(isStoredSshHost)
  );
}

function isStoredSshHost(value: unknown): value is StoredSshHost {
  if (!value || typeof value !== "object") {
    return false;
  }
  const candidate = value as Partial<StoredSshHost>;
  return (
    typeof candidate.destination === "string" &&
    optionalString(candidate.hostname) &&
    optionalString(candidate.user) &&
    (candidate.port === undefined || typeof candidate.port === "number") &&
    optionalString(candidate.identity_file)
  );
}

function optionalString(value: unknown): boolean {
  return value === undefined || typeof value === "string";
}

export function browserStorage(): Storage | null {
  try {
    return typeof window === "undefined" ? null : window.localStorage;
  } catch {
    return null;
  }
}
