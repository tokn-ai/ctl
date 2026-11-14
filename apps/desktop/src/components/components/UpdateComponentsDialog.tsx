import type { DisplayText } from "../../features/i18n/messages";
import { useLocale } from "../../features/i18n/LocaleContext";
import { useEffect, useId, useRef, useState } from "react";
import { QuickInput } from "../commands/QuickInput";
import { QuickInputFrame } from "../commands/QuickInputFrame";
import { useCommandScope } from "../../features/commands/CommandContext";
import { QUICK_INPUT_IDS } from "../../features/commands/commandIds";
import { LOCAL_TARGET, targetKey, targetLabel } from "../../features/targets/targets";
import { cancelComponentUpdate, respondSshPrompt, updateComponents } from "../../lib/tauri";
import { localizedErrorText } from "../../lib/errors";
import type { BundleChannel, ComponentUpdateContext, ComponentUpdateHostResult, ComponentUpdateProgress, ComponentUpdateSource, ConnectionTarget, SshPrompt } from "../../lib/types";
import { promptMode, promptTitle } from "../sessions/sshPrompt";
import { remoteInstallProgressMode } from "../sessions/remoteInstallProgress";
import { PublishedBundleStatus } from "./PublishedBundleStatus";
import { InlineFeedback } from "../feedback/InlineFeedback";
import "./componentUpdate.css";

interface Props {
  targets: readonly ConnectionTarget[];
  context?: ComponentUpdateContext;
  on_updated?(results: ComponentUpdateHostResult[], targets: readonly ConnectionTarget[]): void;
  on_close(): void;
}

type SourceChoice = BundleChannel | Exclude<ComponentUpdateSource["kind"], "published">;

function sourceChoice(source?: ComponentUpdateSource): SourceChoice {
  return source?.kind === "published" ? source.channel : source?.kind ?? "automatic";
}

export function UpdateComponentsDialog({ targets, context, on_updated, on_close }: Props) {
  const { t, locale, formatText } = useLocale();
  const available = new Map([LOCAL_TARGET, ...targets, ...(context?.targets ?? [])]
    .filter((target) => target.kind !== "ssh" || target.remote_platform !== "windows")
    .map((target) => [targetKey(target), target]));
  const [selected, setSelected] = useState(() => new Set((context?.targets ?? [LOCAL_TARGET])
    .filter((target) => target.kind !== "ssh" || (target.remote_platform !== "windows" && !target.unavailable))
    .map(targetKey)));
  const initial_source = context?.source;
  const [source_choice, setSourceChoice] = useState<SourceChoice>(() => sourceChoice(initial_source));
  const published = source_choice === "nightly" || source_choice === "stable";
  const provided = source_choice === "provided";
  const [path, setPath] = useState(initial_source?.kind === "provided" ? initial_source.path : "");
  const [local_build, setLocalBuild] = useState(initial_source?.kind === "provided" && initial_source.local_build);
  const [ctld_package, setCtldPackage] = useState(initial_source?.kind === "provided" ? initial_source.ctld_package ?? "" : "");
  const [running, setRunning] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [error, setError] = useState<DisplayText | null>(null);
  const [validation, setValidation] = useState<{ field: "hosts" | "path"; message_id: string } | null>(null);
  const validation_id = useId();
  const [progress, setProgress] = useState<ComponentUpdateProgress | null>(null);
  const [prompt, setPrompt] = useState<SshPrompt | null>(null);
  const [results, setResults] = useState<ComponentUpdateHostResult[] | null>(null);
  const [submitted_targets, setSubmittedTargets] = useState<ConnectionTarget[]>([]);
  const attempt = useRef<string | null>(null);
  const host_index = useRef(0);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; if (attempt.current) void cancelComponentUpdate(attempt.current).catch(() => undefined); };
  }, []);

  function clearValidation(field: "hosts" | "path") {
    setValidation((current) => current?.field === field ? null : current);
  }

  function selectHost(key: string, checked: boolean) {
    clearValidation("hosts");
    setSelected((current) => {
      const next = new Set(current);
      if (checked) next.add(key);
      else next.delete(key);
      return next;
    });
  }

  function dismiss() {
    if (!running) { on_close(); return; }
    if (stopping || !attempt.current) return;
    setStopping(true);
    void cancelComponentUpdate(attempt.current).catch((failure) => {
      if (mounted.current) { setStopping(false); setError(localizedErrorText(failure)); }
    });
  }

  async function start() {
    if (attempt.current) return;
    setError(null);
    setValidation(null);
    const chosen = [...available.values()].filter((target) => selected.has(targetKey(target)));
    if (!chosen.length || chosen.some((target) => target.kind === "ssh" && target.unavailable)) {
      setValidation({ field: "hosts", message_id: "about.update_invalid_hosts" }); return;
    }
    if (provided && !path.trim()) { setValidation({ field: "path", message_id: "about.update_invalid_path" }); return; }
    const source: ComponentUpdateSource = provided
      ? { kind: "provided", path: path.trim(), local_build, ctld_package: local_build && ctld_package.trim() ? ctld_package.trim() : null }
      : published
        ? { kind: "published", channel: source_choice }
        : { kind: source_choice };
    const attempt_id = crypto.randomUUID();
    attempt.current = attempt_id;
    host_index.current = 0;
    setSubmittedTargets(chosen);
    setRunning(true); setStopping(false); setError(null); setResults(null); setProgress(null);
    try {
      const updated = await updateComponents({ targets: chosen, attempt_id, options: { package: "full_bundle", source } },
        (next) => { if (mounted.current && attempt.current === attempt_id) setPrompt(next); },
        (next) => { if (mounted.current && attempt.current === attempt_id) { host_index.current = next.host_index; setProgress(next); } },
      );
      if (!mounted.current) return;
      setResults(updated);
      on_updated?.(updated, chosen);
    } catch (failure) { if (mounted.current) setError(localizedErrorText(failure)); }
    finally {
      attempt.current = null;
      if (mounted.current) { setRunning(false); setStopping(false); setPrompt(null); }
    }
  }

  useCommandScope({ commands: [
    { id: QUICK_INPUT_IDS.cancel, category: t("about.update_dialog_category"), title: t("about.update_cancel"), enabled: !stopping, run: dismiss },
    { id: QUICK_INPUT_IDS.accept, category: t("about.update_dialog_category"), title: t("about.update_title"), enabled: !running && results === null, run: start },
  ] });

  if (prompt && running) return <QuickInput key={prompt.prompt_id} title={promptTitle(prompt, locale)} description={prompt.message} warning={prompt.warning} mode={promptMode(prompt, locale)} error={error ? formatText(error) : null} cancel_disabled={stopping} onCancel={dismiss} onSubmit={async (response) => {
    const current = attempt.current;
    if (!current) return;
    try { await respondSshPrompt(`${current}:${host_index.current}`, prompt.prompt_id, prompt.kind === "confirm" ? "yes" : response); setPrompt(null); }
    catch (failure) { setError(localizedErrorText(failure)); }
  }} />;

  const install_progress = progress?.progress ? remoteInstallProgressMode(progress.progress, locale) : null;
  return <QuickInputFrame title={t("about.update_title")} className="component-update-dialog" onDismiss={dismiss}>
    <header><h2>{t("about.update_title")}</h2><p>{t("about.update_intro")}</p></header>
    {running ? <div role="status" className="component-update-progress">
      <strong>{stopping ? t("about.update_stopping") : t("about.updating_host", { host: targetLabel(submitted_targets[progress?.host_index ?? 0], locale) })}</strong>
      <p>{install_progress?.message ?? t("about.update_preparing")}</p>
      {install_progress?.detail ? <p>{install_progress.detail}</p> : null}
      {install_progress?.progress ? <progress max={install_progress.progress.max} value={install_progress.progress.value} aria-label={install_progress.progress.label} /> : null}
      <p>{t("about.updating_host_count", { index: (progress?.host_index ?? 0) + 1, total: submitted_targets.length })}</p>
    </div> : results ? <div className="component-update-results">
      <p role="status">{t("about.updated_count", { count: results.filter((result) => result.state === "complete").length, total: submitted_targets.length })}</p>
      <ul>{results.map((result) => <li key={result.host_index}><strong>{targetLabel(submitted_targets[result.host_index], locale)}</strong><span>{result.state === "complete" ? t("about.update_installed") : result.state === "cancelled" ? t("about.update_cancelled") : t("about.update_failed")}</span>{result.result ? <p>{t("about.updated_bundle", { bundle: result.result.bundle_id, target: result.result.target_triple })}</p> : null}{result.error ? <InlineFeedback severity={result.state === "cancelled" ? "warning" : "error"}>{result.error}</InlineFeedback> : null}</li>)}</ul>
    </div> : <form id="component-update-form" onSubmit={(event) => { event.preventDefault(); void start(); }}>
      <fieldset aria-invalid={validation?.field === "hosts" || undefined} aria-describedby={validation?.field === "hosts" ? validation_id : undefined}>
        <legend>{t("about.update_hosts")}</legend>
        <div className="component-update-hosts">{[...available.values()].map((target) => <label key={targetKey(target)}>
          <input type="checkbox" checked={selected.has(targetKey(target))} disabled={target.kind === "ssh" && !!target.unavailable} onChange={(event) => selectHost(targetKey(target), event.target.checked)} />
          <span>{targetLabel(target, locale)}{target.kind === "ssh" && target.unavailable ? <small>{t("about.update_connection_unavailable")}</small> : null}</span>
        </label>)}</div>
        {validation?.field === "hosts" ? <div id={validation_id}><InlineFeedback>{t(validation.message_id)}</InlineFeedback></div> : null}
      </fieldset>
      <label className="component-update-field">{t("about.build_source")}<select value={source_choice} onChange={(event) => { clearValidation("path"); setSourceChoice(event.target.value as SourceChoice); }}>
        <option value="automatic">{t("about.build_automatic")}</option>
        <option value="nightly">{t("about.build_nightly")}</option>
        <option value="stable">{t("about.build_stable")}</option>
        {import.meta.env.DEV || source_choice === "working_tree" ? <option value="working_tree">{t("about.build_checkout")}</option> : null}
        <option value="selected">{t("about.build_offline")}</option>
        <option value="provided">{t("about.build_provided")}</option>
      </select></label>
      {provided ? <div className="component-update-source">
        <label className="component-update-field">{t("about.build_archive")}<input value={path} aria-invalid={validation?.field === "path" || undefined} aria-describedby={validation?.field === "path" ? validation_id : undefined} onChange={(event) => { clearValidation("path"); setPath(event.target.value); }} placeholder="/path/to/build" /></label>
        {validation?.field === "path" ? <div id={validation_id}><InlineFeedback>{t(validation.message_id)}</InlineFeedback></div> : null}
        <label><input type="checkbox" checked={local_build} onChange={(event) => setLocalBuild(event.target.checked)} /><span>{t("about.build_native")}<small>{t("about.build_native_help")}</small></span></label>
        {local_build ? <label className="component-update-field">{t("about.build_signed_ctld")}<input value={ctld_package} onChange={(event) => setCtldPackage(event.target.value)} placeholder="/path/to/ctld-package" /></label> : <p>{t("about.build_provided_help")}</p>}
      </div> : <>
        <p className="component-update-hint">{source_choice === "working_tree" ? t("about.build_checkout_help") : source_choice === "selected" ? t("about.build_offline_help") : source_choice === "automatic" ? t("about.build_automatic_help") : t("about.build_channel_help")}</p>
        {source_choice !== "selected" && source_choice !== "working_tree" ? <PublishedBundleStatus channel={published ? source_choice : undefined} /> : null}
      </>}
    </form>}
    {error ? <InlineFeedback>{formatText(error)}</InlineFeedback> : null}
    <footer>
      {results?.some((result) => result.state === "failed") ? <button type="button" onClick={() => { setSelected(new Set(results.filter((result) => result.state === "failed").map((result) => targetKey(submitted_targets[result.host_index])))); setResults(null); setError(null); }}>{t("about.update_retry")}</button> : null}
      <button type="button" onClick={dismiss} disabled={stopping}>{running ? t("about.update_stop") : results ? t("about.update_done") : t("about.update_cancel")}</button>
      {!running && !results ? <button type="submit" form="component-update-form" disabled={!selected.size}>{t("about.update_confirm")}</button> : null}
    </footer>
  </QuickInputFrame>;
}
