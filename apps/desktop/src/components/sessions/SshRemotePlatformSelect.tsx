import type { SshRemotePlatform } from "../../lib/types";

export function SshRemotePlatformSelect({ value, onChange, className }: {
  value?: SshRemotePlatform;
  onChange(value: SshRemotePlatform): void;
  className?: string;
}) {
  return <label className={className}>
    Server platform
    <select value={value ?? "unix"} onChange={(event) => onChange(event.currentTarget.value === "windows" ? "windows" : "unix")}>
      <option value="unix">Unix (macOS, Linux)</option>
      <option value="windows">Windows</option>
    </select>
  </label>;
}
