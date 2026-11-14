import type { ManagedTask, TaskDefinition, TaskRun } from "../../lib/types";
import { LocalizedError, type MessageValues } from "../i18n/messages";

export function taskState(task: ManagedTask): TaskRun["state"] {
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
export class TaskDefinitionValidationError extends LocalizedError {
  constructor(
    readonly field: TaskDefinitionField,
    readonly field_value: string,
    message_id: string,
    readonly argument_index?: number,
    values?: MessageValues,
  ) {
    super(message_id, values);
    this.name = "TaskDefinitionValidationError";
  }
}

export function definitionValidationError(definition: TaskDefinition): TaskDefinitionValidationError | null {
  if (!definition.name.trim()) return new TaskDefinitionValidationError("name", definition.name, "tasks.enter_name");
  if (definition.name.trim() !== definition.name || new TextEncoder().encode(definition.name).length > 64)
    return new TaskDefinitionValidationError("name", definition.name, "tasks.invalid_name");
  if (definition.working_directory && !/^(\/|[A-Za-z]:[\\/]|\\\\)/.test(definition.working_directory))
    return new TaskDefinitionValidationError("working_directory", definition.working_directory, "tasks.absolute_directory");
  if (!definition.program.trim()) return new TaskDefinitionValidationError("program", definition.program, "tasks.enter_executable");
  const fields = ["name", "program", "working_directory"] as const;
  for (const field of fields) {
    const value = definition[field] ?? "";
    if (/[\x00-\x1f]/.test(value)) return new TaskDefinitionValidationError(field, value, "tasks.no_control_characters");
  }
  const argument_index = definition.arguments.findIndex((value) => value.includes("\0"));
  if (argument_index !== -1) return new TaskDefinitionValidationError("arguments", definition.arguments[argument_index], "tasks.no_argument_null", argument_index);
  return null;
}
