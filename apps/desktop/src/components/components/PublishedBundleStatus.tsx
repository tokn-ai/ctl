import { useEffect, useRef, useState } from "react";
import { checkComponentUpdates } from "../../lib/tauri";
import { errorMessage } from "../../lib/errors";
import type { BundleChannel, ComponentUpdateCatalog } from "../../lib/types";

interface Props {
  visible?: boolean;
  channel?: BundleChannel;
  on_update?(channel: BundleChannel): void;
}

const channel_names: Record<BundleChannel, string> = {
  nightly: "nightly",
  stable: "stable",
};

/** Reads publication metadata only; downloading and installing remain explicit. */
export function PublishedBundleStatus({ visible = true, channel, on_update }: Props) {
  const [snapshot, setSnapshot] = useState<ComponentUpdateCatalog | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
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
      if (generation.current === current) setError(errorMessage(failure));
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
  return <section className="published-bundle-status" aria-label="Published component bundles">
    <div className="published-bundle-actions">
      <button type="button" disabled={loading} onClick={() => void check(true)}>{loading ? "Checking published bundles…" : "Check for bundle updates"}</button>
      {catalog && on_update ? <button type="button" onClick={() => on_update(catalog.channel)}>Install published bundle…</button> : null}
    </div>
    {catalog ? <p role="status">Published {channel_names[catalog.channel]} bundle: <strong>{catalog.bundle_id}</strong>.</p> : null}
    {catalog?.targets.some((target) => !target.compatible) ? <p>Some published targets require a newer client. Update the client before installing incompatible components.</p> : null}
    {!channel && snapshot?.local_development ? <p>Linux x64 uploads build this working tree locally. Other remote targets use the {channel_names[snapshot.default_channel]} channel.</p> : null}
    {error ? <p role="alert">Could not check published bundles: {error}</p> : null}
  </section>;
}
