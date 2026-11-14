import { translate, type Locale } from "../../features/i18n/messages";

const UNITS = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];

export function formatStatisticsBytes(value: bigint | null, locale: Locale = "en"): string {
  if (value === null) return "—";
  let unit = 0;
  let divisor = 1n;
  while (unit < UNITS.length - 1 && value >= divisor * 1024n) {
    divisor *= 1024n;
    unit += 1;
  }
  const whole = value / divisor;
  const decimal = value % divisor * 10n / divisor;
  const format = new Intl.NumberFormat(locale, { useGrouping: false });
  const fraction = unit > 0 && whole < 10n && decimal > 0n;
  const separator = fraction ? format.formatToParts(1.1).find((part) => part.type === "decimal")?.value ?? "." : "";
  const count = fraction ? `${format.format(whole)}${separator}${format.format(decimal)}` : format.format(whole);
  return translate(locale, "statistics.bytes", { count, unit: UNITS[unit] });
}

export function formatStatisticsRate(value: number, locale: Locale = "en"): string {
  const bytes = formatStatisticsBytes(BigInt(Number.isFinite(value) ? Math.max(0, Math.round(value)) : 0), locale);
  return translate(locale, "statistics.rate", { bytes });
}

export function formatStatisticsDuration(value: number | null, locale: Locale = "en"): string {
  if (value === null || !Number.isFinite(value)) return "—";
  if (value < 1000) return translate(locale, "statistics.duration_ms", { value: Math.round(value) });
  const seconds = new Intl.NumberFormat(locale, { useGrouping: false, minimumFractionDigits: 1, maximumFractionDigits: 1 }).format(value / 1000);
  return translate(locale, "statistics.duration_seconds", { seconds });
}

export function formatStatisticsHistory(downloaded: bigint | null, full: bigint | null, locale: Locale = "en"): string {
  if (downloaded !== null && downloaded === full) return formatStatisticsBytes(full, locale);
  return translate(locale, "statistics.history_ratio", {
    downloaded: formatStatisticsBytes(downloaded, locale), retained: formatStatisticsBytes(full, locale),
  });
}
