const UNITS = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];

export function formatStatisticsBytes(value: bigint | null): string {
  if (value === null) return "—";
  let unit = 0;
  let divisor = 1n;
  while (unit < UNITS.length - 1 && value >= divisor * 1024n) {
    divisor *= 1024n;
    unit += 1;
  }
  const whole = value / divisor;
  const decimal = value % divisor * 10n / divisor;
  return `${whole}${unit > 0 && whole < 10n && decimal > 0n ? `.${decimal}` : ""} ${UNITS[unit]}`;
}

export function formatStatisticsRate(value: number): string {
  return `${formatStatisticsBytes(BigInt(Number.isFinite(value) ? Math.max(0, Math.round(value)) : 0))}/s`;
}

export function formatStatisticsDuration(value: number | null): string {
  if (value === null || !Number.isFinite(value)) return "—";
  return value < 1000 ? `${Math.round(value)} ms` : `${(value / 1000).toFixed(1)} s`;
}

export function formatStatisticsHistory(downloaded: bigint | null, full: bigint | null): string {
  if (downloaded !== null && downloaded === full) return formatStatisticsBytes(full);
  return `${formatStatisticsBytes(downloaded)} / ${formatStatisticsBytes(full)}`;
}
