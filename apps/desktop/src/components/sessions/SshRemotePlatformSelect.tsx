import { useLocale } from "../../features/i18n/LocaleContext";
import type { SshRemotePlatform } from "../../lib/types";

export function SshRemotePlatformSelect({ value, onChange, className }: {
  value?: SshRemotePlatform;
  onChange(value: SshRemotePlatform): void;
  className?: string;
}) {
  const { t } = useLocale();
  return <label className={className}>
    {t("host_platform.server")}
    <select value={value ?? "unix"} onChange={(event) => onChange(event.currentTarget.value === "windows" ? "windows" : "unix")}>
      <option value="unix">{t("host_platform.unix")}</option>
      <option value="windows">{t("host_platform.windows")}</option>
    </select>
  </label>;
}
