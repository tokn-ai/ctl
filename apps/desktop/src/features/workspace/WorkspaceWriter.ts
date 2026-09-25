import type { WorkspaceDocument } from "../../lib/types";
import { errorCode } from "../../lib/errors";

type Save<Document> = (
  revision: string | null,
  document: Document,
) => Promise<{ revision: string | null; document: Document }>;

/** Serializes writes across UI updates; the native store fences other processes. */
export class WorkspaceWriter<Document = WorkspaceDocument> {
  private revision: string | null;
  private saved: string;
  private requested: string | null = null;
  private tail: Promise<void> = Promise.resolve();
  private conflict: unknown = null;

  constructor(
    snapshot: { revision: string | null; document: Document },
    private readonly save: Save<Document>,
  ) {
    this.revision = snapshot.revision;
    this.saved = snapshot.revision ? JSON.stringify(snapshot.document) : "";
  }

  write(document: Document, retry = false): Promise<void> {
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
          if (["workspace_conflict", "hosts_conflict"].includes(errorCode(error) ?? "")) this.conflict = error;
          throw error;
        }
      });
    return this.tail;
  }
}
