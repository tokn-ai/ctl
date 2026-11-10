import type { RemoteAgentInstallProgress } from "../../lib/types";
import type { QuickInputMode } from "../commands/QuickInput";

const STAGES: Record<RemoteAgentInstallProgress["phase"], string> = {
  detecting_platform: "Detecting remote operating system and architecture…",
  preparing_bundle: "Preparing component bundles…",
  downloading_bundle: "Downloading components…",
  building_bundle: "Building components…",
  checking_bundle: "Verifying components…",
  verifying_bundle: "Verifying the bundled archive checksum…",
  connecting: "Opening the SSH transfer…",
  transferring: "Sending",
  extracting: "Extracting",
  checking: "Checking",
  activating: "Activating the installed components…",
  complete: "Installation complete. Running services were preserved.",
};

function formatBytes(bytes: number): string {
  const units = ["B", "KiB", "MiB", "GiB"];
  let value = Math.max(0, bytes);
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${new Intl.NumberFormat("en", { maximumFractionDigits: unit === 0 ? 0 : 1 }).format(value)} ${units[unit]}`;
}

export function remoteInstallProgressMode(
  state: RemoteAgentInstallProgress | null,
): Extract<QuickInputMode, { kind: "progress" }> {
  const phase = state?.phase ?? "detecting_platform";
  const transferring = phase === "transferring";
  const downloading = phase === "downloading_bundle";
  const file_name = state?.file_name;
  const total = state?.total_bytes ?? 0;
  const received = Math.min(total, Math.max(0, state?.transferred_bytes ?? 0));
  const has_transfer = total > 0 && (downloading || ["transferring", "extracting", "checking", "activating", "complete"].includes(phase));
  const message = file_name && ["transferring", "extracting", "checking"].includes(phase)
    ? `${STAGES[phase]} ${file_name}…`
    : downloading && file_name ? `Downloading ${file_name}…` : STAGES[phase];
  return {
    kind: "progress",
    message,
    progress: {
      label: downloading ? "Component download" : phase === "building_bundle" ? "Component build" : "Remote component transfer",
      max: Math.max(1, total),
      value: has_transfer ? received : undefined,
    },
    detail: has_transfer
      ? `${formatBytes(received)} / ${formatBytes(total)} · ${Math.floor(received / total * 100)}%${transferring ? ` · ${formatBytes(state?.bytes_per_second ?? 0)}/s` : downloading ? "" : " · Transfer complete"}`
      : downloading ? state?.transferred_bytes ? formatBytes(state.transferred_bytes) : undefined : file_name ?? undefined,
  };
}
