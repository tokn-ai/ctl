import type { TaskWorkspace } from "../../features/tasks/useTaskWorkspace";
import type { SavedTaskDefinition } from "../../lib/types";
import { useState } from "react";
export function TaskEditor({ model, saved }: { model: TaskWorkspace; saved?: SavedTaskDefinition }) {
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [instanceName, setInstanceName] = useState<string | null>(null);
  const draft = model.draft;
  if (!draft) return <div className="task-empty">This definition is no longer saved.</div>;
  const definition = draft.definition;
  return <div className="task-editor">
    <header className="task-page-heading"><div><span className="task-eyebrow">WORKSPACE DEFINITION</span><h1>{saved?.definition.name ?? "New task"}</h1><p>Save a command to run again. Each run is managed independently of this editor.</p></div><span className="task-badge">{draft.dirty ? "Unsaved changes" : "Saved"}</span></header>
    <form onSubmit={(event) => { event.preventDefault(); void model.save(); }}>
      <fieldset disabled={model.busy}>
        <label>Name<input autoFocus value={definition.name} placeholder="e.g. API server" onChange={(event) => model.edit({ ...definition, name: event.target.value })} /></label>
        <div className="task-mode-picker" role="group" aria-label="Execution mode">{(["background", "interactive"] as const).map((mode) => <button type="button" aria-pressed={definition.execution_mode === mode} className={definition.execution_mode === mode ? "chosen" : ""} key={mode} onClick={() => model.edit({ ...definition, execution_mode: mode })}><strong>{mode === "background" ? "Background" : "Interactive"}</strong><span>{mode === "background" ? "Services and jobs with log output" : "Commands that need a terminal"}</span></button>)}</div>
        <label>Executable<input value={definition.program} placeholder={definition.execution_mode === "interactive" ? "bash" : "cargo"} onChange={(event) => model.edit({ ...definition, program: event.target.value })} /></label>
        <div className="task-field-label">Arguments <small>Each row is one argument; no shell quoting.</small></div>
        {definition.arguments.map((argument, index) => <div className="task-argument" key={index}><input aria-label={`Argument ${index + 1}`} value={argument} onChange={(event) => model.edit({ ...definition, arguments: definition.arguments.map((value, position) => position === index ? event.target.value : value) })} /><button type="button" aria-label={`Remove argument ${index + 1}`} onClick={() => model.edit({ ...definition, arguments: definition.arguments.filter((_, position) => index !== position) })}>×</button></div>)}
        <button className="task-text-button" type="button" onClick={() => model.edit({ ...definition, arguments: [...definition.arguments, ""] })}>+ Add argument</button>
        <label>Working directory<input value={definition.working_directory ?? ""} placeholder="Absolute path (optional)" onChange={(event) => model.edit({ ...definition, working_directory: event.target.value || null })} /><small>Use an absolute path for project commands.</small></label>
        <div className="task-form-actions"><button className="task-primary" type="submit">Save definition</button><button type="button" onClick={() => void model.save(true)}>Save and run</button><span>Local host</span></div>
      </fieldset>
    </form>
    {saved ? <div className="task-secondary-actions"><button disabled={model.busy || draft.dirty} onClick={() => void model.run(saved)}>Run saved definition</button><button disabled={model.busy || draft.dirty} onClick={() => setInstanceName(`${saved.definition.name} (2)`)}>Run another instance…</button><button className="task-danger" onClick={() => setConfirmDelete(true)}>Delete definition…</button></div> : null}
    {instanceName !== null && saved ? <div className="task-confirm" role="dialog" aria-label="Run another instance"><label>New task name<input autoFocus value={instanceName} onChange={(event) => setInstanceName(event.target.value)} /></label><button disabled={!instanceName.trim() || model.busy} onClick={() => { void model.run(saved, true, instanceName); setInstanceName(null); }}>Create and run</button><button onClick={() => setInstanceName(null)}>Cancel</button></div> : null}
    {confirmDelete && saved ? <div className="task-confirm" role="dialog" aria-label="Delete definition"><p>Delete “{saved.definition.name}” from this workspace? Its managed tasks will remain.</p><button onClick={() => void model.forgetDefinition(saved.definition_id)}>Delete definition</button><button onClick={() => setConfirmDelete(false)}>Cancel</button></div> : null}
  </div>;
}
