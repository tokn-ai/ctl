import { maxComponentMetadata, parseComponent, type ComponentInfo, type ProtocolInfo } from "./protocol-contract.mts";

const machines = {
  "x86_64-unknown-linux-musl": 62,
  "aarch64-unknown-linux-musl": 183,
} as const;

export type LinuxBundleTarget = keyof typeof machines;

export function isLinuxBundleTarget(value: string): value is LinuxBundleTarget {
  return Object.hasOwn(machines, value);
}

// Keep the byte record encoding in sync with ctl-core/src/component/embedded.rs.
const marker = Buffer.from("\0tokn-ctl-component\0\x01", "ascii");
const decoder = new TextDecoder("utf-8", { fatal: true });

function staticElfData(bytes: Buffer, target: LinuxBundleTarget): Buffer[] {
  if (bytes.length < 64 || bytes.readUInt32BE(0) !== 0x7f454c46 ||
    bytes[4] !== 2 || bytes[5] !== 1 || bytes[6] !== 1 ||
    (bytes[7] !== 0 && bytes[7] !== 3) ||
    ![2, 3].includes(bytes.readUInt16LE(16)) || bytes.readUInt16LE(18) !== machines[target]) {
    throw new Error(`expected a Linux ELF executable for ${target}`);
  }
  const header_offset = bytes.readBigUInt64LE(32);
  const header_size = bytes.readUInt16LE(54);
  const header_count = bytes.readUInt16LE(56);
  if (header_offset < 64n || header_size !== 56 || header_count === 0 ||
    header_offset + BigInt(header_size * header_count) > BigInt(bytes.length)) {
    throw new Error("invalid ELF program headers");
  }
  const segments: Buffer[] = [];
  for (let index = 0; index < header_count; index += 1) {
    const header = Number(header_offset) + index * header_size;
    const type = bytes.readUInt32LE(header);
    if (type === 3) throw new Error("Linux musl executable has a dynamic interpreter");
    if (type !== 1) continue;
    const offset = bytes.readBigUInt64LE(header + 8);
    const end = offset + bytes.readBigUInt64LE(header + 32);
    if (end > BigInt(bytes.length)) throw new Error("invalid ELF load segment");
    // Look only in linked, read-only data. Ignore debug data and bytes appended
    // after linking that the executable cannot use as its metadata record.
    if ((bytes.readUInt32LE(header + 4) & 6) === 4) {
      segments.push(bytes.subarray(Number(offset), Number(end)));
    }
  }
  return segments;
}

class MetadataReader {
  private offset = 0;
  private readonly bytes: Buffer;

  constructor(bytes: Buffer) {
    this.bytes = bytes;
  }

  private take(size: number): Buffer {
    if (this.offset + size > this.bytes.length) throw new Error("truncated embedded component metadata");
    const result = this.bytes.subarray(this.offset, this.offset + size);
    this.offset += size;
    return result;
  }

  byte(): number { return this.take(1)[0]; }
  word(): number { return this.take(2).readUInt16LE(); }
  text(): string { return decoder.decode(this.take(this.word())); }
  version(): string { return [this.word(), this.word(), this.word()].join("."); }

  count(): number {
    const count = this.word();
    if (count > 128) throw new Error("too many embedded component contracts or versions");
    return count;
  }

  contract(): ProtocolInfo {
    const name = this.text();
    const build = this.word();
    const version = this.version();
    const supported_versions = Array.from({ length: this.count() }, () => this.version());
    return { name, build, version, supported_versions };
  }

  finish(): void {
    if (this.offset !== this.bytes.length) throw new Error("unexpected trailing embedded component metadata");
  }
}

/** Verifies the target independently of the metadata, then reads the linked record. */
export function readElfComponent(bytes: Buffer, target: LinuxBundleTarget, executable: string): ComponentInfo {
  try {
    let metadata: Buffer | undefined;
    for (const data of staticElfData(bytes, target)) {
      const start = data.indexOf(marker);
      if (start < 0) continue;
      if (metadata || data.indexOf(marker, start + marker.length) !== -1) {
        throw new Error("expected exactly one embedded component record");
      }
      const length_offset = start + marker.length;
      if (length_offset + 4 > data.length) throw new Error("truncated embedded component record");
      const size = data.readUInt32LE(length_offset);
      const offset = length_offset + 4;
      if (size === 0 || size > maxComponentMetadata || offset + size > data.length) {
        throw new Error("invalid embedded component record size");
      }
      metadata = data.subarray(offset, offset + size);
    }
    if (!metadata) throw new Error("no linked component metadata found");
    const record = new MetadataReader(metadata);
    if (record.text() !== executable || record.text() !== target) {
      throw new Error("embedded metadata names a different executable or target");
    }
    const version = record.text();
    const source_revision = record.text() || null;
    const source_fingerprint = record.text();
    const dirty = record.byte();
    if (dirty !== 0 && dirty !== 1) throw new Error("invalid embedded dirty flag");
    const protocols = Array.from({ length: record.count() }, () => record.contract());
    record.finish();
    return parseComponent({ build: { version, source_revision, source_fingerprint, dirty: dirty === 1 }, protocols });
  } catch (cause) {
    throw new Error(`${executable}: ${(cause as Error).message}`, { cause });
  }
}
