export interface LayoutLeaseCommand {
  attachment_id: string;
  generation: number;
  acquire: boolean;
}

export function shouldStopResizeAfterLeaseStatus(
  resizeDesired: boolean,
  layoutOwned: boolean,
  expectedIntent: boolean | null,
): boolean {
  return resizeDesired && !layoutOwned && expectedIntent !== false;
}

/** Serializes layout lease changes and coalesces waiting work to latest intent. */
export class LayoutLeasePump {
  private pending: LayoutLeaseCommand | null = null;
  private running = false;
  private readonly expectedResponses: LayoutLeaseCommand[] = [];

  constructor(
    private readonly send: (command: LayoutLeaseCommand) => Promise<void>,
    private readonly onError: (
      error: unknown,
      command: LayoutLeaseCommand,
    ) => void,
  ) {}

  schedule(command: LayoutLeaseCommand): void {
    this.pending = command;
    void this.drain();
  }

  reset(): void {
    this.pending = null;
    this.expectedResponses.splice(0);
  }

  takeExpectedResponse(
    attachmentId: string,
    generation: number,
  ): boolean | null {
    const index = this.expectedResponses.findIndex(
      (command) =>
        command.attachment_id === attachmentId &&
        command.generation === generation,
    );
    if (index === -1) {
      return null;
    }
    return this.expectedResponses.splice(index, 1)[0].acquire;
  }

  hasScheduledIntent(
    attachmentId: string,
    generation: number,
    acquire: boolean,
  ): boolean {
    const matches = (command: LayoutLeaseCommand) =>
      command.attachment_id === attachmentId &&
      command.generation === generation &&
      command.acquire === acquire;
    return (
      (this.pending !== null && matches(this.pending)) ||
      this.expectedResponses.some(matches)
    );
  }

  private async drain(): Promise<void> {
    if (this.running) {
      return;
    }
    this.running = true;
    try {
      while (this.pending) {
        const command = this.pending;
        this.pending = null;
        this.expectedResponses.push(command);
        try {
          await this.send(command);
        } catch (error) {
          const index = this.expectedResponses.indexOf(command);
          if (index !== -1) {
            this.expectedResponses.splice(index, 1);
          }
          this.onError(error, command);
        }
      }
    } finally {
      this.running = false;
    }
  }
}
