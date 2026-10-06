import { useEffect, useRef, useState } from "react";
import { QuickInput } from "../commands/QuickInput";
import { QuickInputFrame } from "../commands/QuickInputFrame";
import { useCommandScope } from "../../features/commands/CommandContext";
import { QUICK_INPUT_IDS } from "../../features/commands/commandIds";
import { LOCAL_TARGET, targetKey, targetLabel } from "../../features/targets/targets";
import { cancelComponentUpdate, respondSshPrompt, updateComponents } from "../../lib/tauri";
import { errorMessage } from "../../lib/errors";
import type { ComponentUpdateContext, ComponentUpdateHostResult, ComponentUpdateProgress, ComponentUpdateSource, ConnectionTarget, SshPrompt } from "../../lib/types";
import { promptMode, promptTitle } from "../sessions/sshPrompt";
import { remoteInstallProgressMode } from "../sessions/remoteInstallProgress";
import "./componentUpdate.css";

interface Props {
  targets: readonly ConnectionTarget[];
  context?: ComponentUpdateContext;
  on_updated?(results: ComponentUpdateHostResult[], targets: readonly ConnectionTarget[]): void;
  on_close(): void;
}

export function UpdateComponentsDialog({ targets, context, on_updated, on_close }: Props) {
  const available = new Map([LOCAL_TARGET, ...targets, ...(context?.targets ?? [])].map((target) => [targetKey(target), target]));
  const [selected, setSelected] = useState(() => new Set((context?.targets ?? [LOCAL_TARGET]).filter((target) => target.kind !== "ssh" || !target.unavailable).map(targetKey)));
  const [update_package, setPackage] = useState(context?.package ?? "full_bundle");
  const initial_source = context?.source;
  const [provided, setProvided] = useState(initial_source?.kind === "provided");
  const [path, setPath] = useState(initial_source?.kind === "provided" ? initial_source.path : "");
  const [local_build, setLocalBuild] = useState(initial_source?.kind === "provided" && initial_source.local_build);
  const [ctld_package, setCtldPackage] = useState(initial_source?.kind === "provided" ? initial_source.ctld_package ?? "" : "");
  const [running, setRunning] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [error, setError] = useState<string | null>(null);
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

  function dismiss() {
    if (!running) { on_close(); return; }
    if (stopping || !attempt.current) return;
    setStopping(true);
    void cancelComponentUpdate(attempt.current).catch((failure) => {
      if (mounted.current) { setStopping(false); setError(errorMessage(failure)); }
    });
  }

  async function start() {
    if (attempt.current) return;
    const chosen = [...available.values()].filter((target) => selected.has(targetKey(target)));
    if (!chosen.length || chosen.some((target) => target.kind === "ssh" && target.unavailable)) {
      setError("Choose at least one available host."); return;
    }
    if (provided && !path.trim()) { setError("Enter a build directory or archive path."); return; }
    const source: ComponentUpdateSource = provided
      ? { kind: "provided", path: path.trim(), local_build, ctld_package: local_build && ctld_package.trim() ? ctld_package.trim() : null }
      : { kind: "selected" };
    const attempt_id = crypto.randomUUID();
    attempt.current = attempt_id;
    host_index.current = 0;
    setSubmittedTargets(chosen);
    setRunning(true); setStopping(false); setError(null); setResults(null); setProgress(null);
    try {
      const updated = await updateComponents({ targets: chosen, attempt_id, options: { package: update_package, source } },
        (next) => { if (mounted.current && attempt.current === attempt_id) setPrompt(next); },
        (next) => { if (mounted.current && attempt.current === attempt_id) { host_index.current = next.host_index; setProgress(next); } },
      );
      if (!mounted.current) return;
      setResults(updated);
      on_updated?.(updated, chosen);
    } catch (failure) { if (mounted.current) setError(errorMessage(failure)); }
    finally {
      attempt.current = null;
      if (mounted.current) { setRunning(false); setStopping(false); setPrompt(null); }
    }
  }

  useCommandScope({ commands: [
    { id: QUICK_INPUT_IDS.cancel, category: "Dialog", title: "Cancel", enabled: !stopping, run: dismiss },
    { id: QUICK_INPUT_IDS.accept, category: "Dialog", title: "Update components", enabled: !running && results === null, run: start },
  ] });

  if (prompt && running) return <QuickInput key={prompt.prompt_id} title={promptTitle(prompt)} description={prompt.message} warning={prompt.warning} mode={promptMode(prompt)} error={error} cancel_disabled={stopping} onCancel={dismiss} onSubmit={async (response) => {
    const current = attempt.current;
    if (!current) return;
    try { await respondSshPrompt(`${current}:${host_index.current}`, prompt.prompt_id, prompt.kind === "confirm" ? "yes" : response); setPrompt(null); }
    catch (failure) { setError(errorMessage(failure)); }
  }} />;

  const install_progress = progress?.progress ? remoteInstallProgressMode(progress.progress) : null;
  return <QuickInputFrame title="Update components" className="component-update-dialog" onDismiss={dismiss}>
    <header><h2>Update components</h2><p>Install on selected hosts. Running sessions are kept; restart or reconnect separately.</p></header>
    {running ? <div role="status" className="component-update-progress">
      <strong>{stopping ? "Stopping update…" : `Updating ${targetLabel(submitted_targets[progress?.host_index ?? 0])}`}</strong>
      <p>{install_progress?.message ?? "Verifying the build and connection…"}</p>
      {install_progress?.detail ? <p>{install_progress.detail}</p> : null}
      {install_progress?.progress ? <progress max={install_progress.progress.max} value={install_progress.progress.value} aria-label={install_progress.progress.label} /> : null}
      <p>Host {(progress?.host_index ?? 0) + 1} of {submitted_targets.length}</p>
    </div> : results ? <div className="component-update-results">
      <p role="status">{results.filter((result) => result.state === "complete").length} of {submitted_targets.length} hosts updated. Running services were preserved.</p>
      <ul>{results.map((result) => <li key={result.host_index}><strong>{targetLabel(submitted_targets[result.host_index])}</strong><span>{result.state === "complete" ? "Installed" : result.state === "cancelled" ? "Cancelled" : "Failed"}</span>{result.error ? <p role="alert">{result.error}</p> : null}</li>)}</ul>
    </div> : <form id="component-update-form" onSubmit={(event) => { event.preventDefault(); void start(); }}>
      <fieldset><legend>Hosts</legend><div className="component-update-hosts">{[...available.values()].map((target) => <label key={targetKey(target)}><input type="checkbox" checked={selected.has(targetKey(target))} disabled={target.kind === "ssh" && !!target.unavailable} onChange={(event) => setSelected((current) => { const next = new Set(current); if (event.target.checked) next.add(targetKey(target)); else next.delete(targetKey(target)); return next; })} /><span>{targetLabel(target)}{target.kind === "ssh" && target.unavailable ? <small>Connection unavailable</small> : null}</span></label>)}</div></fieldset>
      <fieldset><legend>Package</legend>
        <label><input type="radio" name="update-package" checked={update_package === "ctl_agent"} onChange={() => setPackage("ctl_agent")} /><span>ctl-agent only<small>Keep the installed daemons.</small></span></label>
        <label><input type="radio" name="update-package" checked={update_package === "full_bundle"} onChange={() => setPackage("full_bundle")} /><span>Full bundle<small>ctl-agent, ctld, ctmuxd, and ctl-taskd from one build.</small></span></label>
      </fieldset>
      <label className="component-update-field">Build source<select value={provided ? "provided" : "selected"} onChange={(event) => setProvided(event.target.value === "provided")}><option value="selected">Selected build for each target</option><option value="provided">Provided build files</option></select></label>
      {provided ? <div className="component-update-source">
        <label className="component-update-field">Build directory or archive<input value={path} onChange={(event) => setPath(event.target.value)} placeholder="/path/to/build" /></label>
        <label><input type="checkbox" checked={local_build} onChange={(event) => setLocalBuild(event.target.checked)} /><span>Native local build<small>A directory containing all four binaries for this computer's platform.</small></span></label>
        {local_build ? <label className="component-update-field">Signed ctld package directory (macOS full bundle)<input value={ctld_package} onChange={(event) => setCtldPackage(event.target.value)} placeholder="/path/to/ctld-package" /></label> : <p>Use a complete bundle directory, or a verified archive with its bundle-set.json beside it.</p>}
      </div> : <p className="component-update-hint">Uses your pinned selection. Included builds are used when a target has no selection. Local macOS full bundles need a signed ctld package.</p>}
    </form>}
    {error ? <p role="alert" className="quick-input-error">{error}</p> : null}
    <footer>
      {results?.some((result) => result.state === "failed") ? <button type="button" onClick={() => { setSelected(new Set(results.filter((result) => result.state === "failed").map((result) => targetKey(submitted_targets[result.host_index])))); setResults(null); setError(null); }}>Retry failed hosts…</button> : null}
      <button type="button" onClick={dismiss} disabled={stopping}>{running ? "Stop update" : results ? "Done" : "Cancel"}</button>
      {!running && !results ? <button type="submit" form="component-update-form" disabled={!selected.size}>Update</button> : null}
    </footer>
  </QuickInputFrame>;
}
