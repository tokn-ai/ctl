interface PendingTask {
  run(): Promise<void>;
  onError(error: unknown): void;
  complete(): void;
}

/** Runs one task at a time and keeps only the newest task still waiting. */
export class LatestTaskQueue {
  private pending: PendingTask | null = null;
  private running = false;

  submit(run: () => Promise<void>, onError: (error: unknown) => void): Promise<void> {
    return new Promise<void>((complete) => {
      this.pending?.complete();
      this.pending = { run, onError, complete };
      void this.drain();
    });
  }

  cancelPending(): void {
    this.pending?.complete();
    this.pending = null;
  }

  private async drain(): Promise<void> {
    if (this.running) {
      return;
    }
    this.running = true;
    try {
      while (this.pending) {
        const task = this.pending;
        this.pending = null;
        try {
          await task.run();
        } catch (error) {
          task.onError(error);
        } finally {
          task.complete();
        }
      }
    } finally {
      this.running = false;
    }
  }
}
