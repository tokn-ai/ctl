const DEFAULT_MAX_QUEUED_BYTES = 64 * 1024;

export class InputPump {
  private readonly queue: Uint8Array[] = [];
  private queuedBytes = 0;
  private draining = false;

  constructor(
    private readonly send: (data: Uint8Array) => Promise<void>,
    private readonly onError: (error: unknown) => void,
    private readonly maxQueuedBytes = DEFAULT_MAX_QUEUED_BYTES,
  ) {}

  push(data: Uint8Array): boolean {
    if (data.length === 0) {
      return true;
    }
    if (this.queuedBytes + data.length > this.maxQueuedBytes) {
      return false;
    }
    this.queue.push(data.slice());
    this.queuedBytes += data.length;
    void this.drain();
    return true;
  }

  clear(): void {
    for (const data of this.queue) {
      this.queuedBytes -= data.length;
    }
    this.queue.splice(0);
  }

  private async drain(): Promise<void> {
    if (this.draining) {
      return;
    }
    this.draining = true;
    try {
      while (this.queue.length > 0) {
        const data = this.queue.shift();
        if (!data) {
          break;
        }
        try {
          await this.send(data);
        } finally {
          this.queuedBytes -= data.length;
        }
      }
    } catch (error) {
      this.clear();
      this.onError(error);
    } finally {
      this.draining = false;
    }
  }
}
