import { errorMessage } from "../../lib/errors";

/** The workspace surface owns persistence failures across task operations. */
export class TaskWorkspaceSaveError extends Error {
  constructor(failure: unknown) {
    super(errorMessage(failure));
    this.name = "TaskWorkspaceSaveError";
  }
}

export async function persistTaskWorkspace(workspace: { persist(): Promise<void> }): Promise<void> {
  try { await workspace.persist(); }
  catch (failure) { throw new TaskWorkspaceSaveError(failure); }
}
