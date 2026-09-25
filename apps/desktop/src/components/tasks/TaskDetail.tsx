import { useEffect, useRef, useState } from "react";
import type { TaskWorkspace } from "../../features/tasks/useTaskWorkspace";
import type { SavedTaskDefinition } from "../../lib/types";
import { sameDefinition, taskState } from "../../features/tasks/taskModel";
import { useTaskLogs } from "../../features/tasks/useTaskLogs";
export function TaskDetail({ model, saved }: { model: TaskWorkspace; saved?: SavedTaskDefinition }) {
  const task = model.activeTask;
  const interactive = task?.definition.execution_mode === "interactive";
  const logs = useTaskLogs(interactive ? null : task);
  const [follow, setFollow] = useState(true);
  const [confirmRemove, setConfirmRemove] = useState(false);
  const [copyMessage, setCopyMessage] = useState("");
  const output = useRef<HTMLPreElement>(null);
  useEffect(() => { if (follow && output.current) output.current.scrollTop = output.current.scrollHeight; }, [logs.lines, follow]);
  if (!task) return <div className="task-empty"><span className="task-eyebrow">MANAGED TASK</span><h1>{model.active?.kind === "task" && model.active.host_id !== "local" ? "Remote tasks are not available yet" : model.connection_error ? "Task status unavailable" : "Task is no longer registered"}</h1><p>{model.error ?? "The workspace reference is preserved. No process has been started."}</p>{model.active?.kind === "task" && model.active.host_id === "local" ? <><button onClick={() => void model.refresh()}>Retry</button>{saved ? <button onClick={() => void model.recreate(model.active!.kind === "task" ? model.active!.task_id : "")}>Recreate from saved definition</button> : null}</> : null}</div>;
  const run = task.active_run ?? task.last_run;
  const changed = saved && !sameDefinition(saved.definition, task.definition);
  return <section className={`task-detail ${interactive ? "interactive" : ""}`}>
    <header className="task-detail-header"><div><span className="task-eyebrow">LOCAL · {interactive ? "INTERACTIVE" : "BACKGROUND"}</span><h1>{task.definition.name} <span className={`task-badge ${taskState(task)}`}>{taskState(task)}</span></h1></div><div className="task-detail-actions"><button className="task-primary" disabled={model.busy || !!task.active_run} onClick={() => void model.action(task, "start_task")}>{changed ? "Start registered command" : "Start"}</button><button disabled={model.busy || !task.active_run} onClick={() => void model.action(task, "stop_task")}>Stop</button><button disabled={model.busy} onClick={() => void model.action(task, "restart_task")}>Restart</button></div></header>
    <div className="task-command"><code>{[task.definition.program, ...task.definition.arguments.map((arg) => JSON.stringify(arg))].join(" ")}</code><span>{task.definition.working_directory ?? "Default working directory"}</span></div>
    {changed ? <div className="task-change-notice">Saved definition has changes.<button disabled={!!task.active_run || model.busy} title={task.active_run ? "Stop the task before applying changes" : undefined} onClick={() => void model.apply(task, saved)}>Apply saved definition</button></div> : null}
    <div className="task-run-meta"><span>{run ? `${task.active_run ? "Started" : "Last run"} ${new Date(run.started_at_ms).toLocaleString()}` : "No runs yet"}{run?.ended_at_ms ? ` · Exit ${run.exit_code ?? "unknown"}` : ""}</span><button onClick={() => void model.saveAsDefinition(task)}>Save as definition</button><button disabled={!!task.active_run || model.busy} onClick={() => setConfirmRemove(true)}>Remove task…</button></div>
    {confirmRemove ? <div className="task-confirm" role="dialog" aria-label="Remove task"><p>Remove “{task.definition.name}” from taskd? Saved definitions remain available.</p><button onClick={() => void model.action(task, "remove_task")}>Remove task</button><button onClick={() => setConfirmRemove(false)}>Cancel</button></div> : null}
    {interactive ? !task.active_run ? <p className="task-output-empty">Start this task to open its terminal here.</p> : <p className="task-terminal-caption">Terminal connected through rmux · Closing this tab leaves the task running.</p> : <>
      <div className="task-log-toolbar"><strong>Output</strong><label><input type="checkbox" checked={follow} onChange={(event) => setFollow(event.target.checked)} /> Follow</label><button onClick={() => { void navigator.clipboard.writeText(logs.lines.map((line) => line.text).join("")).then(() => setCopyMessage("Copied"), () => setCopyMessage("Copy unavailable")); }}>Copy</button><button onClick={logs.clear}>Clear view</button><span role="status">{copyMessage}</span></div>
      {logs.error ? <div className="task-inline-error" role="alert">{logs.error}<button onClick={logs.retry}>Retry logs</button></div> : null}
      <pre className="task-log-output" aria-label="Task output" ref={output}>{logs.lines.length ? logs.lines.map((line, index) => <span className={line.stream} key={index}>{line.text}</span>) : <span className="task-output-empty">{run ? "Waiting for output. Logs are available for the current taskd lifetime." : "Start this task to see its output."}</span>}</pre>
      <footer className="task-log-footer">stdout <span>·</span> <b>stderr</b> <span>·</span> Latest 512K characters in this view · Output resets for each run</footer>
    </>}
  </section>;
}
