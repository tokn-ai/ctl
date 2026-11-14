import type { DisplayText } from "../../features/i18n/messages";
import { useLocale } from "../../features/i18n/LocaleContext";
import { useEffect, useRef, useState } from "react";
import { checkComponentUpdates } from "../../lib/tauri";
import { localizedErrorText } from "../../lib/errors";
import type { BundleChannel, ComponentUpdateCatalog } from "../../lib/types";
import { InlineFeedback } from "../feedback/InlineFeedback";

interface Props {
  visible?: boolean;
  channel?: BundleChannel;
  on_update?(channel: BundleChannel): void;
}

/** Reads publication metadata only; downloading and installing remain explicit. */
export function PublishedBundleStatus({ visible = true, channel, on_update }: Props) {
  const { t, intl } = useLocale();
  const [snapshot, setSnapshot] = useState<ComponentUpdateCatalog | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<DisplayText | null>(null);
  const generation = useRef(0);

  async function check(refresh: boolean) {
    const current = ++generation.current;
    setLoading(true);
    setSnapshot(null);
    setError(null);
    try {
      const next = await checkComponentUpdates(channel, refresh);
      if (generation.current === current) { setSnapshot(next); setError(next.error); }
    } catch (failure) {
      if (generation.current === current) setError(localizedErrorText(failure));
    } finally {
      if (generation.current === current) setLoading(false);
    }
  }

  useEffect(() => {
    if (visible) void check(false);
    return () => { generation.current++; };
  }, [visible, channel]);

  if (!visible) return null;
  const catalog = snapshot?.catalog;
  return <section className="published-bundle-status" aria-label={t("about.published_title")}>
    <div className="published-bundle-actions">
      <button type="button" disabled={loading} onClick={() => void check(true)}>{loading ? t("about.published_checking") : t("about.published_check_updates")}</button>
      {catalog && on_update ? <button type="button" onClick={() => on_update(catalog.channel)}>{t("about.published_install")}</button> : null}
    </div>
    {catalog ? <p role="status">{intl.formatMessage({ id: "about.published_bundle" }, { channel: t(`about.channel_${catalog.channel}`), bundle: catalog.bundle_id, strong: (chunks) => <strong>{chunks}</strong> })}</p> : null}
    {catalog?.targets.some((target) => !target.compatible) ? <InlineFeedback severity="warning">{t("about.published_incompatible")}</InlineFeedback> : null}
    {!channel && snapshot?.local_development ? <p>{t("about.published_development", { channel: t(`about.channel_${snapshot.default_channel}`) })}</p> : null}
    {error ? <InlineFeedback>{t("about.published_check_error", { detail: error })}</InlineFeedback> : null}
  </section>;
}
