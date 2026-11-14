import { formatter, translate, type Locale } from "../../features/i18n/messages";
import type { RemoteAgentInstallProgress } from "../../lib/types";
import type { QuickInputMode } from "../commands/QuickInput";

function formatBytes(bytes: number, locale: Locale): string {
  const units = ["B", "KiB", "MiB", "GiB"];
  let value = Math.max(0, bytes);
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${formatter(locale).formatNumber(value, { maximumFractionDigits: unit === 0 ? 0 : 1 })} ${units[unit]}`;
}

export function remoteInstallProgressMode(
  state: RemoteAgentInstallProgress | null,
  locale: Locale = "en",
): Extract<QuickInputMode, { kind: "progress" }> {
  const phase = state?.phase ?? "detecting_platform";
  const transferring = phase === "transferring";
  const downloading = phase === "downloading_bundle";
  const file_name = state?.file_name;
  const total = state?.total_bytes ?? 0;
  const received = Math.min(total, Math.max(0, state?.transferred_bytes ?? 0));
  const has_transfer = total > 0 && (downloading || ["transferring", "extracting", "checking", "activating", "complete"].includes(phase));
  const message = file_name && ["transferring", "extracting", "checking", "downloading_bundle"].includes(phase)
    ? translate(locale, `remote_install.file_${phase}`, { filename: file_name })
    : translate(locale, `remote_install.${phase}`);
  let detail = downloading ? state?.transferred_bytes ? formatBytes(state.transferred_bytes, locale) : undefined : file_name ?? undefined;
  if (has_transfer) {
    const values = {
      received: formatBytes(received, locale),
      total: formatBytes(total, locale),
      percent: formatter(locale).formatNumber(Math.floor(received / total * 100) / 100, { style: "percent" }),
    };
    detail = transferring
      ? translate(locale, "remote_install.transfer_rate", { ...values, rate: formatBytes(state?.bytes_per_second ?? 0, locale) })
      : translate(locale, downloading ? "remote_install.transfer_amount" : "remote_install.transfer_complete", values);
  }
  return {
    kind: "progress",
    message,
    progress: {
      label: translate(locale, downloading ? "remote_install.download" : phase === "building_bundle" ? "remote_install.build" : "remote_install.transfer"),
      max: Math.max(1, total),
      value: has_transfer ? received : undefined,
    },
    detail,
  };
}
