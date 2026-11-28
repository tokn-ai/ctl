import { useEffect, useState } from "react";
import { runtimeChannelShowsRevision } from "../../features/about/runtimeTargets";
import { useLocale } from "../../features/i18n/LocaleContext";
import { errorDetails, type ErrorDetails } from "../../lib/errors";
import { getUpdaterBuild } from "../../lib/tauri";
import type { UpdaterBuildInfo } from "../../lib/types";

export function UpdaterVersion({ visible, refresh_revision }: { visible: boolean; refresh_revision: number }) {
  const { t, formatText } = useLocale();
  const [result, setResult] = useState<UpdaterBuildInfo | ErrorDetails | null>(null);
  useEffect(() => {
    if (!visible) return;
    let current = true;
    setResult(null);
    void getUpdaterBuild().then(
      (build) => { if (current) setResult(build); },
      (error) => { if (current) setResult(errorDetails(error)); },
    );
    return () => { current = false; };
  }, [visible, refresh_revision]);

  const build = result && "version" in result ? result : null;
  const channel = build?.channel;
  const channel_label = channel === "stable" || channel === "nightly" || channel === "local"
    ? t(`about.channel_${channel}`) : channel ?? t("about.channel_unknown");
  const revision = build?.source_revision;
  return <div className="about-desktop-build about-updater-version" role="group" aria-label={t("about.updater")}>
    <span>{t("about.updater")}</span>
    <strong>ctlup{build ? ` ${build.version}` : ""}</strong>
    {build && <span className="about-desktop-channel">{channel_label}</span>}
    {channel && runtimeChannelShowsRevision(channel) && <code title={revision ?? undefined}>
      {revision ? `SHA ${revision.slice(0, 8)}` : t("about.desktop_sha_unavailable")}
    </code>}
    {result === null && <span>{t("about.checking")}</span>}
    {result && "code" in result && <span title={formatText(result.message)}>
      {t(result.code === "ctlup_unavailable" ? "about.updater_not_installed" : "about.unknown_version")}
    </span>}
  </div>;
}
