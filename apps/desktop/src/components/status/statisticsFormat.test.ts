import { describe, expect, it } from "vitest";
import { formatStatisticsBytes, formatStatisticsDuration, formatStatisticsHistory, formatStatisticsRate } from "./statisticsFormat";

describe("statistics formatting", () => {
  it("shows equal history once and distinguishes unknown full retention", () => {
    expect(formatStatisticsHistory(2048n, 2048n)).toBe("2 KiB");
    expect(formatStatisticsHistory(1024n, 4096n)).toBe("1 KiB / 4 KiB");
    expect(formatStatisticsHistory(0n, null)).toBe("0 B / —");
    expect(formatStatisticsHistory(null, null)).toBe("— / —");
  });
  it("handles u64 sizes without integer precision loss and idle rates", () => {
    expect(formatStatisticsBytes(18446744073709551615n)).toBe("15 EiB");
    expect(formatStatisticsBytes(1536n)).toBe("1.5 KiB");
    expect(formatStatisticsRate(0)).toBe("0 B/s");
    expect(formatStatisticsRate(2048)).toBe("2 KiB/s");
    expect(formatStatisticsDuration(null)).toBe("—");
    expect(formatStatisticsDuration(12.2)).toBe("12 ms");
    expect(formatStatisticsDuration(1500)).toBe("1.5 s");
  });
});
