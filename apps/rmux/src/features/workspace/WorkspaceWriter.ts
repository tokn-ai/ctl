import type { WorkspaceDocument, WorkspaceSnapshot } from "../../lib/types";
import { errorCode } from "../../lib/errors";

type Save = (
  revision: string | null,
  document: WorkspaceDocument,
) => Promise<WorkspaceSnapshot>;

/** Serializes writes across UI updates; the native store fences other processes. */
export class WorkspaceWriter {
  private revision: string | null;
  private saved: string;
  private requested: string | null = null;
  private tail: Promise<void> = Promise.resolve();
  private conflict: unknown = null;

  constructor(
    snapshot: WorkspaceSnapshot,
    private readonly save: Save,
  ) {
    this.revision = snapshot.revision;
    this.saved = snapshot.revision ? JSON.stringify(snapshot.document) : "";
  }

  write(document: WorkspaceDocument, retry = false): Promise<void> {
    const encoded = JSON.stringify(document);
    if (!retry && encoded === this.requested) return this.tail;
    this.requested = encoded;
    this.tail = this.tail
      .catch(() => undefined)
      .then(async () => {
        if (this.conflict) throw this.conflict;
        if (encoded === this.saved) return;
        try {
          const snapshot = await this.save(this.revision, document);
          this.revision = snapshot.revision;
          this.saved = encoded;
        } catch (error) {
          if (errorCode(error) === "workspace_conflict") this.conflict = error;
          throw error;
        }
      });
    return this.tail;
  }
}
