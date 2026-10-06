import { useEffect, useRef, useState } from "react";
import { errorMessage } from "../../lib/errors";
import { getComponentBundles, selectComponentBundle } from "../../lib/tauri";
import type { ComponentBundle, ComponentBundlePhase, ComponentBundlePurpose, ComponentBundlesSnapshot } from "../../lib/types";

interface Props {
  visible: boolean;
  on_selected(): void;
}

/** Selection changes future launches and uploads; running services stay in place. */
export function ComponentBundlePanel({ visible, on_selected }: Props) {
  const [snapshot, setSnapshot] = useState<ComponentBundlesSnapshot | null>(null);
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState<{ bundle_id: string; purpose: ComponentBundlePurpose; phase: ComponentBundlePhase } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
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
      if (mounted.current && current === generation.current) setError(errorMessage(failure));
    } finally {
      if (mounted.current && current === generation.current) setLoading(false);
    }
  }

  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; generation.current++; };
  }, []);
  useEffect(() => { if (visible && !selecting.current) void refresh(); }, [visible]);

  async function select(bundle: ComponentBundle, purpose: ComponentBundlePurpose) {
    if (selecting.current) return;
    selecting.current = true;
    generation.current++;
    setLoading(false);
    setError(null);
    setNotice(null);
    setBusy({ bundle_id: bundle.bundle_id, purpose, phase: "verifying" });
    try {
      await selectComponentBundle({ bundle_id: bundle.bundle_id, target_triple: bundle.target_triple, purpose }, (phase) => {
        if (mounted.current) setBusy({ bundle_id: bundle.bundle_id, purpose, phase });
      });
      if (mounted.current) {
        setNotice(`${purpose === "local" ? "Local" : "Upload"} bundle selected. Running services and sessions were preserved. Restart separately to apply a local selection to an existing service.`);
        selected_callback.current();
        await refresh();
      }
    } catch (failure) {
      if (mounted.current) setError(errorMessage(failure));
    } finally {
      selecting.current = false;
      if (mounted.current) setBusy(null);
    }
  }

  return <section className="about-section" aria-labelledby="about-bundles">
    <div className="about-bundle-heading"><h2 id="about-bundles">Bundles</h2><button type="button" disabled={loading || busy !== null} onClick={() => void refresh()}>{loading ? "Checking bundles…" : "Refresh bundles"}</button></div>
    <p className="about-muted">Each bundle is one complete build from CI, a release, or a local build. Choose a bundle for new local services or for uploads to hosts with that target. Selections stay in place until you change them.</p>
    {error ? <p className="about-error" role="alert">Could not complete the bundle operation: {error}</p> : null}
    {snapshot?.errors.map((message) => <p className="about-error" role="alert" key={message}>{message}</p>)}
    {notice ? <p className="about-notice" role="status">{notice}</p> : null}
    {busy ? <p role="status">{busy.phase === "verifying" ? "Verifying the complete build…" : "Selecting the verified bundle…"}</p> : null}
    {snapshot?.bundles.length ? <div className="about-table-scroll"><table className="about-bundle-table"><thead><tr><th scope="col">Build</th><th scope="col">Source</th><th scope="col">Target</th><th scope="col">Use</th></tr></thead><tbody>{snapshot.bundles.map((bundle) => <tr key={`${bundle.target_triple}:${bundle.bundle_id}`}>
      <td><strong>{bundle.app_version}</strong>{bundle.dirty ? " · local changes" : ""}<small title={bundle.git_revision ?? undefined}>{bundle.git_revision?.slice(0, 12) ?? "Revision unknown"}</small><small title={bundle.bundle_id}>Bundle {bundle.bundle_id.slice(0, 12)}</small></td>
      <td>{bundle.source === "ci" ? "CI" : bundle.source === "release" ? "Release" : "Local build"}</td>
      <td><code>{bundle.target_triple}</code>{!bundle.compatible ? <small>Incompatible with this app</small> : null}</td>
      <td><div className="about-row-actions">{bundle.local_use === "selected" ? <span>Selected locally</span> : bundle.local_use === "available" ? <button type="button" disabled={busy !== null || loading} onClick={() => void select(bundle, "local")}>Use locally</button> : null}
        {bundle.upload_use === "selected" ? <span>Selected for uploads</span> : bundle.upload_use === "available" ? <button type="button" disabled={busy !== null || loading} onClick={() => void select(bundle, "upload")}>Use for uploads</button> : null}</div></td>
    </tr>)}</tbody></table></div> : loading && !snapshot ? <p role="status">Checking stored bundles…</p> : <p className="about-empty">No complete builds stored. Import one with <code>ctl components sync --from &lt;bundle-directory&gt;</code>, then refresh.</p>}
  </section>;
}
