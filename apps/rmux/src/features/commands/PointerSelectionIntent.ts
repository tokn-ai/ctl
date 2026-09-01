const DEFAULT_SCROLL_DEAD_ZONE = 6;

export interface PointerPosition {
  clientX: number;
  clientY: number;
}

export class PointerSelectionIntent {
  private position: PointerPosition | null = null;
  private commandId: string | null = null;
  private scrollTravel = 0;
  private scrollSuppressed = false;

  constructor(
    private readonly scrollDeadZone = DEFAULT_SCROLL_DEAD_ZONE,
  ) {}

  move(
    position: PointerPosition,
    commandId: string | null,
  ): string | null {
    const previousPosition = this.position;
    this.position = position;

    if (!previousPosition) {
      this.commandId = commandId;
      return commandId;
    }

    if (this.scrollSuppressed) {
      this.scrollTravel += Math.hypot(
        position.clientX - previousPosition.clientX,
        position.clientY - previousPosition.clientY,
      );
      if (this.scrollTravel < this.scrollDeadZone) {
        return null;
      }
      this.scrollSuppressed = false;
      this.scrollTravel = 0;
    }

    if (commandId === this.commandId) {
      return null;
    }
    this.commandId = commandId;
    return commandId;
  }

  scrolled(commandIdAtPointer: string | null): void {
    if (!this.position) {
      return;
    }
    this.commandId = commandIdAtPointer;
    this.scrollTravel = 0;
    this.scrollSuppressed = true;
  }

  leave(): void {
    this.position = null;
    this.commandId = null;
    this.scrollTravel = 0;
    this.scrollSuppressed = false;
  }

  currentPosition(): PointerPosition | null {
    return this.position;
  }
}
