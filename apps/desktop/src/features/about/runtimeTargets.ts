import { translate, type Locale } from "../i18n/messages";

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
