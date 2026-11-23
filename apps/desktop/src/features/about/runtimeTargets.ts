import { translate, type Locale } from "../i18n/messages";
import type { ComponentBuildInfo, InstalledRuntimeTarget } from "../../lib/types";
import { compareParsedVersions, parseVersion } from "../../../../../scripts/shared/semver.mts";

export interface RuntimeBuildTargets {
  key: string;
  build: ComponentBuildInfo;
  targets: Map<string, { channels: string[]; current: boolean }>;
}

export function runtimeVersions(targets: InstalledRuntimeTarget[]) {
  const versions = new Map<string, Map<string, RuntimeBuildTargets>>();
  for (const { target_triple, builds } of targets) {
    for (const { build, channels, current } of builds) {
      const variants = versions.get(build.version) ?? new Map<string, RuntimeBuildTargets>();
      versions.set(build.version, variants);
      const key = JSON.stringify([build.source_revision, build.source_fingerprint, build.dirty]);
      const variant: RuntimeBuildTargets = variants.get(key) ?? { key, build, targets: new Map() };
      variants.set(key, variant);
      const previous = variant.targets.get(target_triple);
      variant.targets.set(target_triple, {
        channels: [...new Set([...(previous?.channels ?? []), ...channels])].sort(),
        current: current !== false || previous?.current === true,
      });
    }
  }
  return Array.from(versions, ([version, builds]) => ({
    version, parsed: parseVersion(version), builds: [...builds.values()],
  })).sort((a, b) => {
    // Imported runtimes can have an arbitrary printable product version.
    if (a.parsed && b.parsed) return compareParsedVersions(b.parsed, a.parsed) || a.version.localeCompare(b.version);
    return Number(!!b.parsed) - Number(!!a.parsed) || a.version.localeCompare(b.version);
  });
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
