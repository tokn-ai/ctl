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
  return definitionValidationError(definition)?.message ?? null;
}

export type TaskDefinitionField = "command_line" | "name" | "program" | "working_directory" | "arguments";

/** Local validation carries its owner; backend messages are never parsed for fields. */
export class TaskDefinitionValidationError extends Error {
  constructor(
    readonly field: TaskDefinitionField,
    readonly field_value: string,
    message: string,
    readonly argument_index?: number,
  ) {
    super(message);
    this.name = "TaskDefinitionValidationError";
  }
}

export function definitionValidationError(definition: TaskDefinition): TaskDefinitionValidationError | null {
  if (!definition.name.trim()) return new TaskDefinitionValidationError("name", definition.name, "Enter a task name.");
  if (definition.name.trim() !== definition.name || new TextEncoder().encode(definition.name).length > 64)
    return new TaskDefinitionValidationError("name", definition.name, "Use a name of up to 64 bytes, without leading or trailing whitespace.");
  if (definition.working_directory && !/^(\/|[A-Za-z]:[\\/]|\\\\)/.test(definition.working_directory))
    return new TaskDefinitionValidationError("working_directory", definition.working_directory, "Use an absolute working directory.");
  if (!definition.program.trim()) return new TaskDefinitionValidationError("program", definition.program, "Enter an executable.");
  const fields = ["name", "program", "working_directory"] as const;
  for (const field of fields) {
    const value = definition[field] ?? "";
    if (/[\x00-\x1f]/.test(value)) return new TaskDefinitionValidationError(field, value, "Name, executable, and directory must not contain control characters.");
  }
  const argument_index = definition.arguments.findIndex((value) => value.includes("\0"));
  if (argument_index !== -1) return new TaskDefinitionValidationError("arguments", definition.arguments[argument_index], "Arguments must not contain null characters.", argument_index);
  return null;
}
