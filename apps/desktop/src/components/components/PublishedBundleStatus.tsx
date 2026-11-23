import type { DisplayText } from "../../features/i18n/messages";
import { useLocale } from "../../features/i18n/LocaleContext";
import { useEffect, useRef, useState } from "react";
import { checkComponentUpdates } from "../../lib/tauri";
import { localizedErrorText } from "../../lib/errors";
import type { BundleChannel, ComponentUpdateCatalog } from "../../lib/types";
import { InlineFeedback } from "../feedback/InlineFeedback";

/** Reads publication metadata only; downloading and installing remain explicit. */
export function PublishedBundleStatus({ channel }: { channel: BundleChannel }) {
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
    void check(false);
    return () => { generation.current++; };
  }, [channel]);

  const catalog = snapshot?.catalog;
  return <section className="published-bundle-status" aria-label={t("about.published_title")}>
    <div className="published-bundle-actions">
      <button type="button" disabled={loading} onClick={() => void check(true)}>{loading ? t("about.published_checking") : t("about.published_check_updates")}</button>
    </div>
    {catalog ? <p role="status">{intl.formatMessage({ id: "about.published_bundle" }, { channel: t(`about.channel_${catalog.channel}`), bundle: catalog.bundle_id, strong: (chunks) => <strong>{chunks}</strong> })}</p> : null}
    {catalog?.targets.some((target) => !target.compatible) ? <InlineFeedback severity="warning">{t("about.published_incompatible")}</InlineFeedback> : null}
    {error ? <InlineFeedback>{t("about.published_check_error", { detail: error })}</InlineFeedback> : null}
  </section>;
}
