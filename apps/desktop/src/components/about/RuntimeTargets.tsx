import { useEffect, useRef, useState } from "react";
import { runtimeChannelShowsRevision, runtimeTargetLabel, runtimeVersions, type RuntimeBuildTargets } from "../../features/about/runtimeTargets";
import { useLocale } from "../../features/i18n/LocaleContext";
import type { DisplayText } from "../../features/i18n/messages";
import { errorCode, localizedErrorText } from "../../lib/errors";
import { cancelComponentUpdate, checkComponentUpdates, fetchAboutRuntimeTarget } from "../../lib/tauri";
import type { AboutRuntimeStatus, BundleChannel, InstalledRuntimeTarget, PublishedBundleCatalog } from "../../lib/types";
import { InlineFeedback } from "../feedback/InlineFeedback";
import { Icon } from "../ui/Icon";
import "./runtimeTargets.css";

interface Props {
  inventory: AboutRuntimeStatus | null;
  loading: boolean;
  error: DisplayText | null;
  default_channel?: BundleChannel;
  on_refresh(): void;
}

function Channels({ channels }: { channels: string[] }) {
  const { t } = useLocale();
  return channels.length ? channels.map((channel) => <span className="about-build-channel" key={channel}>{channel}</span>)
    : <span className="about-muted">{t("about.channel_unknown")}</span>;
}

function shortId(id: string, related: (string | null)[]) {
  let length = 8;
  for (const other of related) {
    if (!other || other === id) continue;
    while (length < id.length && other.startsWith(id.slice(0, length))) length++;
  }
  return id.slice(0, length);
}

function BuildRow({ variant, siblings }: { variant: RuntimeBuildTargets; siblings: RuntimeBuildTargets[] }) {
  const { t, locale } = useLocale();
  const targets = [...variant.targets.entries()].sort(([a], [b]) => a.localeCompare(b));
  const { build } = variant;
  // Channels and catalog heads belong to individual targets. Only place the
  // channel next to the shared build when every listed target agrees.
  const channels = targets[0][1].channels;
  const shared = targets.every(([, target]) => target.channels.length === channels.length &&
    target.channels.every((channel, index) => channel === channels[index]));
  const hasCurrent = targets.some(([, target]) => target.current);
  const showRevision = siblings.length > 1 || !shared || channels.some(runtimeChannelShowsRevision);
  const revisions = siblings.map((peer) => peer.build.source_revision);
  const fingerprints = siblings.filter((peer) => peer.build.source_revision === build.source_revision)
    .map((peer) => peer.build.source_fingerprint);
  return <div className="runtime-version-build">
    <div className="runtime-build-identity">
      {shared && <Channels channels={channels} />}
      {showRevision && <code title={build.source_revision ?? undefined}>
        {build.source_revision ? shortId(build.source_revision, revisions) : t("about.bundle_revision_unknown")}
      </code>}
      {fingerprints.some((fingerprint) => fingerprint !== build.source_fingerprint) && <small title={build.source_fingerprint}>
        {t("about.build_compact", { build: shortId(build.source_fingerprint, fingerprints) })}
      </small>}
      {build.dirty && <small>{t("about.modified")}</small>}
    </div>
    <div className="runtime-build-platforms">{targets.map(([triple, target]) => <div key={triple}>
      <strong>{runtimeTargetLabel(triple, locale)}</strong>
      {!shared && <Channels channels={target.channels} />}
      {hasCurrent && !target.current && <small>{t("about.targets_retained")}</small>}
      <code className="runtime-target-triple">{triple}</code>
    </div>)}</div>
  </div>;
}

function VersionRow({ version, builds }: { version: string; builds: RuntimeBuildTargets[] }) {
  const { t } = useLocale();
  const current = builds.filter((build) => [...build.targets.values()].some((target) => target.current));
  const retained = builds.filter((build) => !current.includes(build));
  const rows = (items: RuntimeBuildTargets[]) => items.map((variant) => <BuildRow key={variant.key} variant={variant} siblings={builds} />);
  return <li>
    <div className="runtime-version-heading"><h3>{version}</h3>{current.length === 0 && <small>{t("about.targets_retained")}</small>}</div>
    {rows(current.length ? current : retained)}
    {current.length > 0 && retained.length > 0 && <details>
      <summary>{t("about.targets_retained_versions", { count: retained.length })}</summary>{rows(retained)}
    </details>}
  </li>;
}

function fetchableTargets(catalog: PublishedBundleCatalog, installed: InstalledRuntimeTarget[]) {
  return catalog.targets.filter(({ target_triple, compatible }) =>
    compatible && (!catalog.publication_id || catalog.publication_id !== installed
      .find((entry) => entry.target_triple === target_triple)?.current_publications?.[catalog.channel]));
}

function AddTarget({ installed, default_channel, on_refresh, on_close, on_installed }: {
  installed: InstalledRuntimeTarget[];
  default_channel: BundleChannel;
  on_refresh(): void;
  on_close(): void;
  on_installed(target: string): void;
}) {
  const { t, formatText, locale } = useLocale();
  const [channel, setChannel] = useState(default_channel);
  const [reload, setReload] = useState(0);
  const [publication, setPublication] = useState<PublishedBundleCatalog | null>(null);
  const [catalogError, setCatalogError] = useState<DisplayText | null>(null);
  const [selected, setSelected] = useState("");
  const [fetchError, setFetchError] = useState<DisplayText | null>(null);
  const [cancelled, setCancelled] = useState(false);
  const [pending, setPending] = useState(false);
  const [stopping, setStopping] = useState(false);
  const attempt = useRef<string | null>(null);

  useEffect(() => {
    let current = true;
    setPublication(null);
    setCatalogError(null);
    void checkComponentUpdates(channel, reload > 0).then(({ catalog, error }) => {
      if (!current) return;
      if (error || catalog?.channel !== channel) setCatalogError(error ?? { id: "about.targets_publication_unavailable" });
      else setPublication(catalog);
    }).catch((error) => {
      if (current) setCatalogError(localizedErrorText(error));
    });
    return () => { current = false; };
  }, [channel, reload]);

  useEffect(() => () => {
    const id = attempt.current;
    attempt.current = null;
    if (id) void cancelComponentUpdate(id).catch(() => undefined);
  }, []);

  const targets = publication?.channel === channel ? fetchableTargets(publication, installed) : [];
  const target = targets.find((item) => item.target_triple === selected) ?? targets[0];
  async function install() {
    if (!target || attempt.current) return;
    const triple = target.target_triple;
    const id = crypto.randomUUID();
    attempt.current = id;
    setPending(true);
    setCancelled(false);
    setFetchError(null);
    try {
      await fetchAboutRuntimeTarget(triple, channel, id);
    } catch (error) {
      if (attempt.current !== id) return;
      if (errorCode(error) === "runtime_operation_cancelled") setCancelled(true);
      else setFetchError(localizedErrorText(error));
      setPending(false);
      setStopping(false);
      attempt.current = null;
      on_refresh();
      return;
    }
    if (attempt.current === id) {
      attempt.current = null;
      on_refresh();
      on_installed(triple);
    }
  }

  async function stop() {
    const id = attempt.current;
    if (!id || stopping) return;
    setStopping(true);
    try {
      await cancelComponentUpdate(id);
    } catch (error) {
      if (attempt.current === id) {
        setFetchError(localizedErrorText(error));
        setStopping(false);
      }
    }
  }

  return <form className="runtime-target-picker" onSubmit={(event) => { event.preventDefault(); void install(); }}>
    <p className="about-muted">{t("about.targets_install_hint")}</p>
    <div className="runtime-target-fields">
      <label>{t("about.service_channel")}<select value={channel} disabled={pending}
        onChange={(event) => { setChannel(event.target.value as BundleChannel); setSelected(""); setFetchError(null); }}>
        <option value="stable">{t("about.channel_stable")}</option><option value="nightly">{t("about.channel_nightly")}</option>
      </select></label>
      <label>{t("about.bundle_target")}<select value={target?.target_triple ?? ""} disabled={pending || !targets.length}
        onChange={(event) => { setSelected(event.target.value); setFetchError(null); }}>
        {targets.map(({ target_triple }) => <option key={target_triple} value={target_triple}>{runtimeTargetLabel(target_triple, locale)}</option>)}
      </select></label>
      <button type="submit" disabled={!target || pending}>{t(pending ? "about.targets_installing" : "about.targets_fetch")}</button>
      {pending
        ? <button type="button" onClick={() => void stop()} disabled={stopping}>{t(stopping ? "about.update_stopping" : "about.update_stop")}</button>
        : <button type="button" onClick={on_close}>{t("about.update_cancel")}</button>}
    </div>
    {catalogError ? <InlineFeedback action={{ label: t("about.refresh_builds"), on_click: () => setReload((v) => v + 1) }}>{formatText(catalogError)}</InlineFeedback>
      : !publication ? <p role="status">{t("about.published_checking")}</p>
      : targets.length === 0 ? <p role="status">{t("about.targets_no_available")}</p> : null}
    {target && <code className="runtime-target-triple">{target.target_triple}</code>}
    {fetchError && <InlineFeedback>{formatText(fetchError)}</InlineFeedback>}
    {cancelled && <p role="status">{t("about.update_cancelled")}</p>}
  </form>;
}

export function RuntimeTargets({ inventory, loading, error, default_channel = "stable", on_refresh }: Props) {
  const { t, formatText, locale } = useLocale();
  const [open, setOpen] = useState(false);
  const [added, setAdded] = useState<string | null>(null);
  const targets = inventory?.installed_targets;
  const versions = runtimeVersions(targets ?? []);
  return <section className="about-section runtime-targets" aria-label={t("about.targets_title")}>
    <div className="runtime-target-heading">
      <h2>{t("about.targets_title")}</h2>
      {!open && <button type="button" onClick={() => { setAdded(null); setOpen(true); }}><Icon name="plus" size={14} />{t("about.targets_add")}</button>}
    </div>
    {versions.length ? <ul className="runtime-target-list">{versions.map(({ version, builds }) => <VersionRow key={version} version={version} builds={builds} />)}</ul>
      : !error && <p className="about-muted" role="status">{t(loading ? "about.checking_builds" : targets ? "about.targets_none" : "about.not_checked")}</p>}
    {error && <InlineFeedback action={{ label: t("about.refresh_builds"), on_click: on_refresh, disabled: loading }}>{formatText(error)}</InlineFeedback>}
    {open && <AddTarget installed={targets ?? []} default_channel={default_channel} on_refresh={on_refresh} on_close={() => setOpen(false)}
      on_installed={(target) => { setOpen(false); setAdded(target); }} />}
    {added && <InlineFeedback severity="success">{t("about.targets_installed", { target: runtimeTargetLabel(added, locale) })}</InlineFeedback>}
  </section>;
}
