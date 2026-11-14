import { useLocale } from "../../features/i18n/LocaleContext";
import { useEffect, useRef, useState } from "react";
import type { TaskWorkspace } from "../../features/tasks/useTaskWorkspace";
import type { SavedTaskDefinition } from "../../lib/types";
import { sameDefinition, taskState } from "../../features/tasks/taskModel";
import { useTaskLogs } from "../../features/tasks/useTaskLogs";
import { InlineFeedback } from "../feedback/InlineFeedback";
export function TaskDetail({ model, saved }: { model: TaskWorkspace; saved?: SavedTaskDefinition }) {
  const { t, locale } = useLocale();
  const task = model.activeTask;
  const interactive = task?.definition.execution_mode === "interactive";
  const logs = useTaskLogs(interactive ? null : task);
  const [follow, setFollow] = useState(true);
  const [confirmRemove, setConfirmRemove] = useState(false);
  const [copyMessage, setCopyMessage] = useState<"copied" | "copy_unavailable" | null>(null);
  const output = useRef<HTMLPreElement>(null);
  useEffect(() => { if (follow && output.current) output.current.scrollTop = output.current.scrollHeight; }, [logs.lines, follow]);
  if (!task) return <div className="task-empty"><span className="task-eyebrow">{t("tasks.managed_task_eyebrow")}</span><h1>{model.active?.kind === "task" && model.active.host_id !== "local" ? t("tasks.remote_unavailable") : model.connection_error ? t("tasks.status_unavailable") : t("tasks.no_longer_registered")}</h1><p>{t("tasks.reference_preserved")}</p>{model.active?.kind === "task" && model.active.host_id === "local" ? <><button onClick={() => void model.refresh()}>{t("tasks.retry")}</button>{saved ? <button onClick={() => void model.recreate(model.active!.kind === "task" ? model.active!.task_id : "")}>{t("tasks.recreate_from_definition")}</button> : null}</> : null}</div>;
  const run = task.active_run ?? task.last_run;
  const changed = saved && !sameDefinition(saved.definition, task.definition);
  return <section className={`task-detail ${interactive ? "interactive" : ""}`}>
    <header className="task-detail-header"><div><span className="task-eyebrow">{t("tasks.mode_eyebrow", { mode: task.definition.execution_mode })}</span><h1>{task.definition.name} <span className={`task-badge ${taskState(task)}`}>{t(`tasks.state_${taskState(task)}`)}</span></h1></div><div className="task-detail-actions"><button className="task-primary" disabled={model.busy || !!task.active_run} onClick={() => void model.action(task, "start_task")}>{changed ? t("tasks.start_registered") : t("tasks.start")}</button><button disabled={model.busy || !task.active_run} onClick={() => void model.action(task, "stop_task")}>{t("tasks.stop")}</button><button disabled={model.busy} onClick={() => void model.action(task, "restart_task")}>{t("tasks.restart")}</button></div></header>
    <div className="task-command"><code>{[task.definition.program, ...task.definition.arguments.map((arg) => JSON.stringify(arg))].join(" ")}</code><span>{task.definition.working_directory ?? t("tasks.default_directory")}</span></div>
    {model.active_definition_error ? <InlineFeedback className="task-feedback" action={{ label: t("tasks.retry_definition"), on_click: () => void model.refreshActiveDefinition() }}>{t("tasks.definition_refresh_error", { error: model.active_definition_error })}</InlineFeedback> : null}
    {model.terminal_error ? <InlineFeedback className="task-feedback" action={{ label: t("tasks.retry_terminal"), on_click: model.retry_terminal }}>{model.terminal_error}</InlineFeedback> : null}
    {changed ? <div className="task-change-notice">{t("tasks.definition_changed")}<button disabled={!!task.active_run || model.busy} title={task.active_run ? t("tasks.stop_before_apply") : undefined} onClick={() => void model.apply(task, saved)}>{t("tasks.apply_definition")}</button></div> : null}
    <div className="task-run-meta"><span>{run ? t("tasks.run_time", { active: String(!!task.active_run), time: new Date(run.started_at_ms).toLocaleString(locale), ended: String(!!run.ended_at_ms), code: run.exit_code ?? t("tasks.unknown_exit") }) : t("tasks.no_runs")}</span><button onClick={() => void model.saveAsDefinition(task)}>{t("tasks.save_as_definition")}</button><button disabled={!!task.active_run || model.busy} onClick={() => setConfirmRemove(true)}>{t("tasks.remove_task")}</button></div>
    {confirmRemove ? <div className="task-confirm" role="dialog" aria-label={t("tasks.remove_task_dialog")}><p>{t("tasks.remove_confirm", { name: task.definition.name })}</p><button onClick={() => void model.action(task, "remove_task")}>{t("tasks.remove_task_dialog")}</button><button onClick={() => setConfirmRemove(false)}>{t("tasks.cancel")}</button></div> : null}
    {interactive ? !task.active_run ? <p className="task-output-empty">{t("tasks.start_terminal_hint")}</p> : <p className="task-terminal-caption">{t("tasks.terminal_connected")}</p> : <>
      <div className="task-log-toolbar"><strong>{t("tasks.output")}</strong><label><input type="checkbox" checked={follow} onChange={(event) => setFollow(event.target.checked)} />{t("tasks.follow")}</label><button onClick={() => { if (!navigator.clipboard?.writeText) { setCopyMessage("copy_unavailable"); return; } void navigator.clipboard.writeText(logs.lines.map((line) => line.text).join("")).then(() => setCopyMessage("copied"), () => setCopyMessage("copy_unavailable")); }}>{t("tasks.copy")}</button><button onClick={logs.clear}>{t("tasks.clear_view")}</button><span role="status">{copyMessage ? t(`tasks.${copyMessage}`) : null}</span></div>
      {logs.error ? <InlineFeedback className="task-feedback" action={{ label: t("tasks.retry_logs"), on_click: logs.retry }}>{logs.error}</InlineFeedback> : null}
      <pre className="task-log-output" aria-label={t("tasks.output_label")} ref={output}>{logs.lines.length ? logs.lines.map((line, index) => <span className={line.stream} key={index}>{line.text}</span>) : <span className="task-output-empty">{run ? t("tasks.wait_output") : t("tasks.start_output_hint")}</span>}</pre>
      <footer className="task-log-footer">stdout <span>·</span> <b>stderr</b> <span>·</span>{t("tasks.output_retention")}</footer>
    </>}
  </section>;
}
