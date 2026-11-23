import { useEffect, useState } from "react";
import { useLocale } from "../../features/i18n/LocaleContext";
import { errorDetails, type ErrorDetails } from "../../lib/errors";
import { getUpdaterVersion } from "../../lib/tauri";

export function UpdaterVersion({ visible, refresh_revision }: { visible: boolean; refresh_revision: number }) {
  const { t, formatText } = useLocale();
  const [result, setResult] = useState<string | ErrorDetails | null>(null);
  useEffect(() => {
    if (!visible) return;
    let current = true;
    setResult(null);
    void getUpdaterVersion().then(
      (version) => { if (current) setResult(version); },
      (error) => { if (current) setResult(errorDetails(error)); },
    );
    return () => { current = false; };
  }, [visible, refresh_revision]);

  return <div className="about-desktop-build about-updater-version" role="group" aria-label={t("about.updater")}>
    <span>{t("about.updater")}</span>
    <strong>ctlup{typeof result === "string" ? ` ${result}` : ""}</strong>
    {result === null && <span>{t("about.checking")}</span>}
    {result && typeof result !== "string" && <span title={formatText(result.message)}>
      {t(result.code === "ctlup_unavailable" ? "about.updater_not_installed" : "about.unknown_version")}
    </span>}
  </div>;
}
