import type { TerminalSize } from "../../lib/types";

const DEFAULT_DEBOUNCE_MILLISECONDS = 80;

export interface PendingResize {
  attachment_id: string;
  generation: number;
  terminal_size: TerminalSize;
}

/** Debounces resize bursts, keeps only the latest grid, and serializes sends. */
export class ResizePump {
  private pending: PendingResize | null = null;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private inFlight: PendingResize | null = null;

  constructor(
    private readonly send: (resize: PendingResize) => Promise<void>,
    private readonly onError: (error: unknown, resize: PendingResize) => void,
    private readonly debounceMilliseconds = DEFAULT_DEBOUNCE_MILLISECONDS,
  ) {}

  schedule(resize: PendingResize): void {
    if (sameResize(this.inFlight, resize)) {
      this.clear();
      return;
    }
    if (sameResize(this.pending, resize)) {
      return;
    }
    this.pending = resize;
    if (!this.inFlight) {
      this.arm();
    }
  }

  clear(): void {
    this.pending = null;
    if (this.timer !== null) {
      clearTimeout(this.timer);
      this.timer = null;
    }
  }

  private arm(): void {
    if (this.timer !== null) {
      clearTimeout(this.timer);
    }
    this.timer = setTimeout(() => {
      this.timer = null;
      void this.flush();
    }, this.debounceMilliseconds);
  }

  private async flush(): Promise<void> {
    if (this.inFlight || !this.pending) {
      return;
    }
    const resize = this.pending;
    this.pending = null;
    this.inFlight = resize;
    try {
      await this.send(resize);
    } catch (error) {
      this.onError(error, resize);
    } finally {
      this.inFlight = null;
      if (this.pending) {
        this.arm();
      }
    }
  }
}

function sameResize(
  left: PendingResize | null,
  right: PendingResize,
): boolean {
  return (
    left?.attachment_id === right.attachment_id &&
    left.generation === right.generation &&
    left.terminal_size.columns === right.terminal_size.columns &&
    left.terminal_size.rows === right.terminal_size.rows
  );
}
