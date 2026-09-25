import type { ManagedTask, TaskDefinition } from "../../lib/types";

export function taskState(task: ManagedTask): string {
  return task.active_run?.state ?? task.last_run?.state ?? "stopped";
}

export function sameDefinition(left: TaskDefinition, right: TaskDefinition): boolean {
  return left.name === right.name && left.program === right.program &&
    left.working_directory === right.working_directory && left.execution_mode === right.execution_mode &&
    JSON.stringify(left.arguments) === JSON.stringify(right.arguments);
}

export function validateDefinition(definition: TaskDefinition): string | null {
  if (!definition.name.trim()) return "Enter a task name.";
  if (definition.name.trim() !== definition.name || new TextEncoder().encode(definition.name).length > 64) return "Use a name of up to 64 bytes, without leading or trailing whitespace.";
  if (definition.working_directory && !/^(\/|[A-Za-z]:[\\/]|\\\\)/.test(definition.working_directory)) return "Use an absolute working directory.";
  if (!definition.program.trim()) return "Enter an executable.";
  if ([definition.name, definition.program, definition.working_directory ?? ""].some((value) => /[\x00-\x1f]/.test(value))) return "Name, executable, and directory must not contain control characters.";
  if (definition.arguments.some((value) => value.includes("\0"))) return "Arguments must not contain null characters.";
  return null;
}
