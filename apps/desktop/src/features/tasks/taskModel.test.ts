import { describe, expect, it } from "vitest";
import type { TaskDefinition } from "../../lib/types";
import { definitionValidationError, TaskDefinitionValidationError } from "./taskModel";

const definition: TaskDefinition = { name: "Build", program: "cargo", arguments: ["build"], working_directory: null, execution_mode: "background" };

describe("task definition validation ownership", () => {
  it.each([
    [{ name: " Build" }, "name", " Build"],
    [{ name: "界".repeat(22) }, "name", "界".repeat(22)],
    [{ program: "" }, "program", ""],
    [{ working_directory: "relative" }, "working_directory", "relative"],
    [{ program: "bad\nprogram" }, "program", "bad\nprogram"],
    [{ arguments: ["build", "bad\0argument"] }, "arguments", "bad\0argument"],
  ] as const)("attributes %o to %s without parsing its message", (changes, field, field_value) => {
    const updated = { ...definition, ...changes };
    const failure = definitionValidationError({ ...updated, arguments: [...updated.arguments] });
    expect(failure).toBeInstanceOf(TaskDefinitionValidationError);
    expect(failure).toMatchObject({ field, field_value });
    if (field === "arguments") expect(failure?.argument_index).toBe(1);
  });

  it.each(["/work/project", "C:\\work\\project", "\\\\server\\project"])("accepts an absolute working directory %s", (working_directory) => {
    expect(definitionValidationError({ ...definition, working_directory })).toBeNull();
  });
});
