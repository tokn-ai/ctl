export interface ProtocolOffer {
  build: number;
  version: string;
  supported_versions: string[];
}

export interface ProtocolInfo extends ProtocolOffer {
  name: string;
}

export interface ComponentInfo {
  build: { version: string; source_revision: string | null; source_fingerprint: string; dirty: boolean };
  protocols: ProtocolInfo[];
}

export const lifecycleOffer: ProtocolOffer = { build: 1, version: "1.0.1", supported_versions: ["1.0.1"] };
export const maxComponentMetadata = 16 * 1024;

function record(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function versionParts(value: unknown): [number, number, number] {
  if (typeof value !== "string" || !/^(?:[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)$/.test(value) || value.length > 17) {
    throw new Error("invalid canonical protocol version");
  }
  const parts = value.split(".").map(Number) as [number, number, number];
  if (parts.some((part) => part > 65_535)) throw new Error("protocol version exceeds its integer limit");
  return parts;
}

function compare(left: string, right: string): number {
  const a = versionParts(left);
  const b = versionParts(right);
  for (let index = 0; index < 3; index += 1) {
    if (a[index] !== b[index]) return a[index] - b[index];
  }
  return 0;
}

export function parseProtocolInfo(value: unknown): ProtocolInfo {
  if (!record(value) || Object.keys(value).sort().join() !== "build,name,supported_versions,version" ||
    typeof value.name !== "string" || !/^[a-zA-Z0-9._-]{1,128}$/.test(value.name) ||
    typeof value.build !== "number" || !Number.isInteger(value.build) || value.build < 0 || value.build > 65_535 ||
    !Array.isArray(value.supported_versions) || value.supported_versions.length === 0 || value.supported_versions.length > 128) {
    throw new Error("invalid protocol advertisement");
  }
  const latest = versionParts(value.version);
  const supported = value.supported_versions.map((version) => { versionParts(version); return version as string; });
  if (latest[2] > value.build || !supported.includes(value.version as string) || new Set(supported).size !== supported.length) {
    throw new Error("invalid published protocol support set");
  }
  const ordered = [...supported].sort(compare);
  for (let index = 0; index < ordered.length; index += 1) {
    const current = versionParts(ordered[index]);
    if (current[0] !== latest[0] || current[2] > value.build || compare(ordered[index], value.version as string) > 0 ||
      (index > 0 && current[2] <= versionParts(ordered[index - 1])[2])) {
      throw new Error("invalid published protocol support set");
    }
  }
  return { name: value.name, build: value.build, version: value.version as string, supported_versions: supported };
}

export function parseHelperProtocols(value: unknown): ProtocolInfo[] {
  if (!Array.isArray(value) || value.length > 128) throw new Error("invalid helper protocol metadata");
  const protocols = value.map(parseProtocolInfo);
  if (new Set(protocols.map((protocol) => protocol.name)).size !== protocols.length ||
    ["ctld", "ctld_lifecycle", "ctld_helper"].some((name) => !protocols.some((protocol) => protocol.name === name))) {
    throw new Error("helper metadata omits or repeats a required protocol");
  }
  return protocols;
}

export function negotiateProtocol(left: ProtocolInfo, right: ProtocolInfo): string | undefined {
  const local = parseProtocolInfo(left);
  const peer = parseProtocolInfo(right);
  if (local.name !== peer.name) return undefined;
  return local.supported_versions.filter((version) => peer.supported_versions.includes(version)).sort(compare).at(-1);
}

export function helperProtocol(protocols: ProtocolInfo[], name = "ctld"): ProtocolInfo {
  const protocol = protocols.find((item) => item.name === name);
  if (!protocol) throw new Error(`helper metadata omits ${name}`);
  return protocol;
}

export function parseComponent(value: unknown): ComponentInfo {
  const build = record(value) && record(value.build) ? value.build : undefined;
  if (!build || typeof build.version !== "string" || build.version.length === 0 || build.version.length > 256 ||
    /\p{Cc}/u.test(build.version) || (build.source_revision !== null && (typeof build.source_revision !== "string" || !/^(?:[a-fA-F0-9]{40}|[a-fA-F0-9]{64})$/.test(build.source_revision))) ||
    typeof build.source_fingerprint !== "string" || !/^[a-f0-9]{64}$/.test(build.source_fingerprint) || typeof build.dirty !== "boolean") {
    throw new Error("component reported invalid source identity or version");
  }
  const advertisements = (value as Record<string, unknown>).protocols;
  if (!Array.isArray(advertisements) || advertisements.length > 128) throw new Error("invalid component protocol metadata");
  const protocols = advertisements.map(parseProtocolInfo);
  if (new Set(protocols.map((protocol) => protocol.name)).size !== protocols.length) {
    throw new Error("component metadata repeats a protocol");
  }
  return { build: build as ComponentInfo["build"], protocols };
}

export function parseHelperComponent(stdout: string): ComponentInfo {
  if (Buffer.byteLength(stdout) > maxComponentMetadata) throw new Error("ctld reported oversized component metadata");
  const component = parseComponent(JSON.parse(stdout));
  return { build: component.build, protocols: parseHelperProtocols(component.protocols) };
}

export function verifyReleaseComponent(stdout: string, app_version: string, git_revision: string): ComponentInfo {
  const component = parseHelperComponent(stdout);
  if (component.build.version !== app_version || component.build.source_revision !== git_revision || component.build.dirty) {
    throw new Error("ctld component source identity does not match the clean release");
  }
  return component;
}

export function sameProtocols(left: ProtocolInfo[], right: ProtocolInfo[]): boolean {
  const canonical = (protocols: ProtocolInfo[]): string => JSON.stringify(protocols.map(parseProtocolInfo)
    .sort((a, b) => a.name.localeCompare(b.name))
    .map((protocol) => ({ ...protocol, supported_versions: [...protocol.supported_versions].sort(compare) })));
  return canonical(left) === canonical(right);
}
