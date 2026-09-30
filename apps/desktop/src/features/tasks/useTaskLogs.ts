import { useEffect, useRef, useState } from "react";
import { cancelTaskLogs, watchTaskLogs } from "../../lib/tauri";
import { errorMessage } from "../../lib/errors";
import type { ManagedTask } from "../../lib/types";
export interface LogLine { stream: "stdout" | "stderr"; text: string }
const MAX_CHARACTERS = 512 * 1024;
export function useTaskLogs(task: ManagedTask | null) {
  const [lines, setLines] = useState<LogLine[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [truncated, setTruncated] = useState(false);
  const [retry, setRetry] = useState(0);
  const decoders = useRef({ stdout: new TextDecoder(), stderr: new TextDecoder() });
  const run = task?.active_run ?? task?.last_run;
  useEffect(() => {
    setLines([]); setError(null); setTruncated(false);
    decoders.current = { stdout: new TextDecoder(), stderr: new TextDecoder() };
    if (!task || !run || (run.definition ?? task.definition).execution_mode !== "background") return;
    let disposed = false;
    let subscription: string | null = null;
    const append = (stream: "stdout" | "stderr", text: string) => {
      if (!text || disposed) return;
      setLines((previous) => {
        const next = [...previous];
        const last = next[next.length - 1];
        if (last?.stream === stream) next[next.length - 1] = { stream, text: last.text + text };
        else next.push({ stream, text });
        let length = next.reduce((total, line) => total + line.text.length, 0);
        while (length > MAX_CHARACTERS && next.length) {
          const removed = Math.min(length - MAX_CHARACTERS, next[0].text.length);
          next[0] = { ...next[0], text: next[0].text.slice(removed) }; length -= removed;
          if (!next[0].text) next.shift();
        }
        return next;
      });
    };
    void watchTaskLogs(task.task_id, null, (event) => {
      if (disposed) return;
      if (event.event_type === "log" && event.run_id === run.run_id) append(event.stream, decoders.current[event.stream].decode(new Uint8Array(event.data), { stream: true }));
      if (event.event_type === "error") setError(event.message);
      if (event.event_type === "finished") { append("stdout", decoders.current.stdout.decode()); append("stderr", decoders.current.stderr.decode()); }
    }).then((id) => { if (disposed) void cancelTaskLogs(id).catch(() => undefined); else subscription = id; }).catch((failure: unknown) => { if (!disposed) setError(errorMessage(failure)); });
    return () => { disposed = true; if (subscription) void cancelTaskLogs(subscription).catch(() => undefined); };
  }, [task?.task_id, run?.run_id, retry]);
  return { lines, error, truncated, clear: () => { setLines([]); setTruncated(false); }, retry: () => setRetry((value) => value + 1) };
}
