import { translate, type DisplayText, type Locale } from "../../features/i18n/messages";
import { useLocale } from "../../features/i18n/LocaleContext";
import { useEffect, useRef, useState } from "react";
import { localizedErrorText } from "../../lib/errors";
import { getComponentBundles, selectComponentBundle } from "../../lib/tauri";
import type { ComponentBundle, ComponentBundlePhase, ComponentBundlesSnapshot } from "../../lib/types";
import { InlineFeedback } from "../feedback/InlineFeedback";

interface Props {
  visible: boolean;
  on_selected(): void;
}

function targetLabel(target: string, locale: Locale): string {
  const labels: Record<string, string> = {
    "aarch64-apple-darwin": translate(locale, "about.bundle_macos_arm"),
    "x86_64-apple-darwin": "macOS · Intel",
    "aarch64-unknown-linux-musl": "Linux · ARM64 · musl",
    "x86_64-unknown-linux-musl": "Linux · x86_64 · musl",
    "aarch64-unknown-linux-gnu": "Linux · ARM64 · glibc",
    "x86_64-unknown-linux-gnu": "Linux · x86_64 · glibc",
  };
  return labels[target] ?? target;
}

function BundleUse({ bundle, disabled, on_select }: {
  bundle: ComponentBundle;
  disabled: boolean;
  on_select(): void;
}) {
  const { t } = useLocale();
  const use = bundle.local_use;
  const reason = bundle.local_unavailable_reason;
  return <div className="about-bundle-use">
    {use === "selected" ? <span className="about-version-status about-status-current">{t("about.active")}</span>
      : use === "available" ? <button type="button" disabled={disabled} onClick={on_select}>{t("about.install")}</button>
      : <span className="about-muted">{reason ?? t("about.unavailable")}</span>}
    {reason && use !== "unavailable" ? <small className="about-error-text">{reason}</small> : null}
  </div>;
}

/** Installation changes future launches; running services stay in place. */
export function ComponentBundlePanel({ visible, on_selected }: Props) {
  const { t, locale } = useLocale();
  const [snapshot, setSnapshot] = useState<ComponentBundlesSnapshot | null>(null);
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState<{ bundle_id: string; phase: ComponentBundlePhase } | null>(null);
  const [error, setError] = useState<{ operation: "refresh" | "install"; message: DisplayText } | null>(null);
  const [installed, setInstalled] = useState(false);
  const mounted = useRef(false);
  const generation = useRef(0);
  const selecting = useRef(false);
  const selected_callback = useRef(on_selected);
  selected_callback.current = on_selected;

  async function refresh() {
    const current = ++generation.current;
    setLoading(true);
    try {
      const next = await getComponentBundles();
      if (mounted.current && current === generation.current) { setSnapshot(next); setError(null); }
    } catch (failure) {
      if (mounted.current && current === generation.current) setError({ operation: "refresh", message: localizedErrorText(failure) });
    } finally {
      if (mounted.current && current === generation.current) setLoading(false);
    }
  }

  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; generation.current++; };
  }, []);
  useEffect(() => { if (visible && !selecting.current) void refresh(); }, [visible]);

  async function select(bundle: ComponentBundle) {
    if (selecting.current) return;
    selecting.current = true;
    generation.current++;
    setLoading(false);
    setError(null);
    setInstalled(false);
    setBusy({ bundle_id: bundle.bundle_id, phase: "verifying" });
    try {
      await selectComponentBundle({ bundle_id: bundle.bundle_id, target_triple: bundle.target_triple, purpose: "local" }, (phase) => {
        if (mounted.current) setBusy({ bundle_id: bundle.bundle_id, phase });
      });
      if (mounted.current) {
        setInstalled(true);
        selected_callback.current();
        await refresh();
      }
    } catch (failure) {
      if (mounted.current) setError({ operation: "install", message: localizedErrorText(failure) });
    } finally {
      selecting.current = false;
      if (mounted.current) setBusy(null);
    }
  }

  return <section className="about-section" aria-labelledby="about-bundles">
    <div className="about-bundle-heading"><h2 id="about-bundles">{t("about.bundles")}</h2><button type="button" disabled={loading || busy !== null} onClick={() => void refresh()}>{loading ? t("about.checking_builds") : t("about.refresh_builds")}</button></div>
    <p className="about-muted">{t("about.bundles_description")}</p>
    {error ? <InlineFeedback>{t(error.operation === "refresh" ? "about.bundle_refresh_failed" : "about.bundle_install_failed", { detail: error.message })}</InlineFeedback> : null}
    {snapshot?.errors.map((message) => <InlineFeedback key={message}>{message}</InlineFeedback>)}
    {installed ? <InlineFeedback severity="success">{t("about.bundle_installed")}</InlineFeedback> : null}
    {busy ? <p role="status">{busy.phase === "verifying" ? t("about.bundle_verify") : t("about.bundle_activate")}</p> : null}
    {snapshot?.bundles.length ? <div className="about-table-scroll"><table className="about-bundle-table" aria-label={t("about.bundle_table")}>
      <colgroup><col className="about-bundle-build-column" /><col className="about-bundle-target-column" /><col className="about-bundle-use-column" /></colgroup>
      <thead><tr><th scope="col">{t("about.bundle_build")}</th><th scope="col">{t("about.bundle_target")}</th><th scope="col">{t("about.bundle_installation")}</th></tr></thead><tbody>{snapshot.bundles.map((bundle) => <tr key={`${bundle.target_triple}:${bundle.bundle_id}`}>
        <th scope="row"><strong>{bundle.app_version}</strong>{bundle.dirty ? t("about.bundle_local_changes") : ""}
          <div className="about-bundle-origin"><span>{bundle.source === "ci" ? "CI" : bundle.source === "release" ? t("about.bundle_release") : t("about.bundle_local")}</span>{bundle.included ? <span>{t("about.bundle_included")}</span> : null}</div>
          <small title={bundle.git_revision ?? undefined}>{t("about.revision_named", { revision: bundle.git_revision?.slice(0, 12) ?? t("about.bundle_revision_unknown") })}</small><small title={bundle.bundle_id}>{t("about.bundle_named", { bundle: bundle.bundle_id.slice(0, 12) })}</small>
        </th>
        <td><span title={bundle.target_triple}>{targetLabel(bundle.target_triple, locale)}</span>{!bundle.compatible ? <small className="about-error-text">{t("about.bundle_incompatible")}</small> : null}</td>
        <td><BundleUse bundle={bundle} disabled={busy !== null || loading} on_select={() => void select(bundle)} /></td>
      </tr>)}</tbody></table></div> : loading && !snapshot ? <p role="status">{t("about.bundle_checking_stored")}</p> : error && !snapshot ? <p className="about-muted">{t("about.bundle_availability_failed")}</p> : <p className="about-empty">{t("about.bundle_empty")}</p>}
  </section>;
}
