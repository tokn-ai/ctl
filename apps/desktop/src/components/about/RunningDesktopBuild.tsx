import { useEffect, useState } from "react";
import { useLocale } from "../../features/i18n/LocaleContext";
import { getDesktopBuild } from "../../lib/tauri";
import type { ComponentVersionInfo, DesktopBuildInfo } from "../../lib/types";

interface Props {
  visible: boolean;
  fallback?: ComponentVersionInfo | null;
}

export function RunningDesktopBuild({ visible, fallback }: Props) {
  const { t } = useLocale();
  const [build, setBuild] = useState<DesktopBuildInfo | null>();
  useEffect(() => {
    if (!visible) return;
    let active = true;
    void getDesktopBuild().then(
      (value) => { if (active) setBuild(value); },
      () => { if (active) setBuild(null); },
    );
    return () => { active = false; };
  }, [visible]);

  if (build === undefined && !fallback) return <p className="about-muted">{t("about.checking_app")}</p>;
  const version = build?.version ?? fallback?.version;
  const channel = build?.channel;
  const revision = build ? build.source_revision : fallback?.source_revision;
  return <div className="about-desktop-build" role="group" aria-label={t("about.desktop")}>
    <span>{t("about.desktop")}</span>
    <strong>ctmux {version ?? t("about.unknown_version")}</strong>
    <span className="about-desktop-channel">{channel ? t(`about.channel_${channel}`) : t("about.channel_unknown")}</span>
    {channel !== "stable" && (revision || channel) && <code title={revision || undefined}>
      {revision ? `SHA ${revision.slice(0, 8)}` : t("about.desktop_sha_unavailable")}
    </code>}
  </div>;
}
