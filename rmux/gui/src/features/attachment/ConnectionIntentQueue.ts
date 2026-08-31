export interface DeferredConnectionIntent<T> {
  request: T;
  settle(): void;
}

type ConnectionIntentRunner<T> = (request: T) => Promise<void>;

/**
 * Tracks the newest attachment intent and holds it until the terminal
 * renderer is ready.
 *
 * Connection replacement follows the same latest-wins rule as
 * `LatestTaskQueue`: callers whose request was superseded are settled rather
 * than left awaiting an intent that will never run.
 */
export class ConnectionIntentQueue<T> {
  private current: T | null = null;
  private pending: DeferredConnectionIntent<T> | null = null;

  begin(request: T): void {
    this.cancelDeferred();
    this.current = request;
  }

  defer(request: T): Promise<void> {
    return new Promise<void>((settle) => {
      this.cancelDeferred();
      this.pending = { request, settle };
    });
  }

  take(): DeferredConnectionIntent<T> | null {
    const pending = this.pending;
    this.pending = null;
    return pending;
  }

  cancel(): void {
    this.current = null;
    this.cancelDeferred();
  }

  complete(request: T): boolean {
    if (this.current !== request) {
      return false;
    }
    this.current = null;
    return true;
  }

  isCurrent(request: T): boolean {
    return this.current === request;
  }

  drain(run: ConnectionIntentRunner<T>): boolean {
    const deferred = this.take();
    if (!deferred) {
      return false;
    }
    if (!this.isCurrent(deferred.request)) {
      deferred.settle();
      return false;
    }
    void run(deferred.request).finally(() => {
      this.complete(deferred.request);
      deferred.settle();
    });
    return true;
  }

  cancelIf(matches: (request: T) => boolean): boolean {
    const current = this.current;
    if (!current || !matches(current)) {
      return false;
    }
    this.current = null;
    if (this.pending?.request === current) {
      this.cancelDeferred();
    }
    return true;
  }

  private cancelDeferred(): void {
    const pending = this.pending;
    this.pending = null;
    pending?.settle();
  }
}
