import type { TerminalSize } from "../../lib/types";

/** Reconciles the measured viewport with daemon-authoritative PTY geometry. */
export class ResizeCoordinator {
  private authoritative: TerminalSize | null = null;
  private desired: TerminalSize | null = null;
  private enabled = false;

  constructor(
    private readonly schedule: (terminalSize: TerminalSize) => void,
    private readonly cancelPending: () => void,
  ) {}

  reset(authoritative: TerminalSize | null = null): void {
    this.authoritative = authoritative;
    this.desired = null;
    this.enabled = false;
    this.cancelPending();
  }

  stop(): void {
    this.desired = null;
    this.enabled = false;
    this.cancelPending();
  }

  setEnabled(enabled: boolean): void {
    this.enabled = enabled;
    this.reconcile();
  }

  setDesired(terminalSize: TerminalSize): void {
    this.desired = terminalSize;
    this.reconcile();
  }

  setAuthoritative(terminalSize: TerminalSize): void {
    this.authoritative = terminalSize;
    this.reconcile();
  }

  private reconcile(): void {
    if (
      !this.enabled ||
      !this.desired ||
      sameGrid(this.authoritative, this.desired)
    ) {
      this.cancelPending();
      return;
    }
    this.schedule(this.desired);
  }
}

function sameGrid(left: TerminalSize | null, right: TerminalSize): boolean {
  return left?.columns === right.columns && left.rows === right.rows;
}
