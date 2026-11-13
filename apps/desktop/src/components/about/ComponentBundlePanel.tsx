import { useEffect, useRef, useState } from "react";
import { errorMessage } from "../../lib/errors";
import { getComponentBundles, selectComponentBundle } from "../../lib/tauri";
import type { ComponentBundle, ComponentBundlePhase, ComponentBundlesSnapshot } from "../../lib/types";
import { InlineFeedback } from "../feedback/InlineFeedback";

interface Props {
  visible: boolean;
  on_selected(): void;
}

function targetLabel(target: string): string {
  const labels: Record<string, string> = {
    "aarch64-apple-darwin": "macOS · Apple silicon",
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
  const use = bundle.local_use;
  const reason = bundle.local_unavailable_reason;
  return <div className="about-bundle-use">
    {use === "selected" ? <span className="about-version-status about-status-current">Active</span>
      : use === "available" ? <button type="button" disabled={disabled} onClick={on_select}>Install</button>
      : <span className="about-muted">{reason ?? "Unavailable"}</span>}
    {reason && use !== "unavailable" ? <small className="about-error-text">{reason}</small> : null}
  </div>;
}

/** Installation changes future launches; running services stay in place. */
export function ComponentBundlePanel({ visible, on_selected }: Props) {
  const [snapshot, setSnapshot] = useState<ComponentBundlesSnapshot | null>(null);
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState<{ bundle_id: string; phase: ComponentBundlePhase } | null>(null);
  const [error, setError] = useState<{ operation: "refresh" | "install"; message: string } | null>(null);
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
      if (mounted.current && current === generation.current) setError({ operation: "refresh", message: errorMessage(failure) });
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
    setNotice(null);
    setBusy({ bundle_id: bundle.bundle_id, phase: "verifying" });
    try {
      await selectComponentBundle({ bundle_id: bundle.bundle_id, target_triple: bundle.target_triple, purpose: "local" }, (phase) => {
        if (mounted.current) setBusy({ bundle_id: bundle.bundle_id, phase });
      });
      if (mounted.current) {
        setNotice("Installed. Running services and sessions were preserved. Restart separately to use the new build.");
        selected_callback.current();
        await refresh();
      }
    } catch (failure) {
      if (mounted.current) setError({ operation: "install", message: errorMessage(failure) });
    } finally {
      selecting.current = false;
      if (mounted.current) setBusy(null);
    }
  }

  return <section className="about-section" aria-labelledby="about-bundles">
    <div className="about-bundle-heading"><h2 id="about-bundles">Builds</h2><button type="button" disabled={loading || busy !== null} onClick={() => void refresh()}>{loading ? "Checking builds…" : "Refresh"}</button></div>
    <p className="about-muted">One active build for this computer. Other platforms are cached for remote installation.</p>
    {error ? <InlineFeedback>{error.operation === "refresh" ? "Could not refresh builds" : "Could not install the build"}: {error.message}</InlineFeedback> : null}
    {snapshot?.errors.map((message) => <InlineFeedback key={message}>{message}</InlineFeedback>)}
    {notice ? <InlineFeedback severity="success">{notice}</InlineFeedback> : null}
    {busy ? <p role="status">{busy.phase === "verifying" ? "Verifying the complete build…" : "Activating the verified build…"}</p> : null}
    {snapshot?.bundles.length ? <div className="about-table-scroll"><table className="about-bundle-table" aria-label="Component bundles">
      <colgroup><col className="about-bundle-build-column" /><col className="about-bundle-target-column" /><col className="about-bundle-use-column" /></colgroup>
      <thead><tr><th scope="col">Build</th><th scope="col">Target</th><th scope="col">Installation</th></tr></thead><tbody>{snapshot.bundles.map((bundle) => <tr key={`${bundle.target_triple}:${bundle.bundle_id}`}>
        <th scope="row"><strong>{bundle.app_version}</strong>{bundle.dirty ? " · local changes" : ""}
          <div className="about-bundle-origin"><span>{bundle.source === "ci" ? "CI" : bundle.source === "release" ? "Release" : "Local build"}</span>{bundle.included ? <span>Included with app</span> : null}</div>
          <small title={bundle.git_revision ?? undefined}>Revision {bundle.git_revision?.slice(0, 12) ?? "unknown"}</small><small title={bundle.bundle_id}>Bundle {bundle.bundle_id.slice(0, 12)}</small>
        </th>
        <td><span title={bundle.target_triple}>{targetLabel(bundle.target_triple)}</span>{!bundle.compatible ? <small className="about-error-text">Incompatible with this app</small> : null}</td>
        <td><BundleUse bundle={bundle} disabled={busy !== null || loading} on_select={() => void select(bundle)} /></td>
      </tr>)}</tbody></table></div> : loading && !snapshot ? <p role="status">Checking included and stored builds…</p> : error && !snapshot ? <p className="about-muted">Bundle availability could not be checked.</p> : <p className="about-empty">No included or stored complete builds are available.</p>}
  </section>;
}
