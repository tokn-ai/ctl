import { translate, type Locale } from "../i18n/messages";
import type { InstalledRuntimeBuild, InstalledRuntimeTarget } from "../../lib/types";
import { compareParsedVersions, parseVersion } from "../../../../../scripts/shared/semver.mts";

export interface StoredRuntimeBuild {
  key: string;
  label: string | null;
  legacy_id: string | null;
  version: string;
  targets: Map<string, InstalledRuntimeBuild>;
}

export function storedRuntimeBuilds(targets: InstalledRuntimeTarget[]): StoredRuntimeBuild[] {
  const rows = new Map<string, StoredRuntimeBuild>();
  for (const { target_triple, builds } of targets) {
    for (const record of builds) {
      const { build, build_label, bundle_id } = record;
      // A readable store path is <build label>/<target>. Older snapshots only
      // identify a payload within a target, so keep those records target-scoped.
      const key = build_label ?? JSON.stringify([
        target_triple, bundle_id, build.version, build.source_revision, build.source_fingerprint, build.dirty,
      ]);
      const row: StoredRuntimeBuild = rows.get(key) ?? {
        key, label: build_label ?? null, legacy_id: build_label ? null : bundle_id ?? build.source_fingerprint,
        version: build.version, targets: new Map(),
      };
      rows.set(key, row);
      row.targets.set(target_triple, record);
    }
  }
  const versioned = Array.from(rows.values(), (row) => ({
    row, version: parseVersion(row.version),
    timestamp: row.label?.match(/^[^-]+-(\d{8}T\d{6}Z)-/)?.[1] ?? "",
  }));
  versioned.sort((a, b) => {
    const order = a.version && b.version ? compareParsedVersions(b.version, a.version) : Number(!!b.version) - Number(!!a.version);
    return order || a.row.version.localeCompare(b.row.version) || b.timestamp.localeCompare(a.timestamp) || a.row.key.localeCompare(b.row.key);
  });
  return versioned.map(({ row }) => row);
}

export function runtimeChannelShowsRevision(channel: string): boolean {
  return channel === "nightly" || channel === "local" || channel === "dev" || channel.startsWith("pr-");
}

export function runtimeTargetLabel(target: string, locale: Locale): string {
  const labels: Record<string, string> = {
    "aarch64-apple-darwin": translate(locale, "about.bundle_macos_arm"),
    "x86_64-apple-darwin": "macOS · Intel",
    "aarch64-unknown-linux-musl": "Linux · ARM64 · musl",
    "x86_64-unknown-linux-musl": "Linux · x86_64 · musl",
    "aarch64-unknown-linux-gnu": "Linux · ARM64 · glibc",
    "x86_64-unknown-linux-gnu": "Linux · x86_64 · glibc",
    "x86_64-pc-windows-msvc": "Windows · x64",
    "aarch64-pc-windows-msvc": "Windows · ARM64",
  };
  return labels[target] ?? target;
}
