import type { TerminalSize } from "../../lib/types";

export interface ProposedDimensions {
  columns: number;
  rows: number;
}

export interface TerminalAdapter {
  write(data: Uint8Array, callback: () => void): void;
  resize(columns: number, rows: number): void;
  dispose(): void;
  focus?(): void;
  proposeDimensions?(): ProposedDimensions | null;
}

export type TerminalAdapterFactory = (terminalSize: TerminalSize) => TerminalAdapter;

export class TerminalPresenter {
  private adapter: TerminalAdapter;
  private operationTail = Promise.resolve();

  constructor(
    private readonly factory: TerminalAdapterFactory,
    initialSize: TerminalSize,
  ) {
    this.adapter = factory(initialSize);
  }

  write(data: Uint8Array): Promise<void> {
    return this.enqueue(() => this.writeBytes(data));
  }

  restoreCheckpoint(
    terminalSize: TerminalSize,
    payload: Uint8Array,
    inputPrefix: Uint8Array,
  ): Promise<void> {
    return this.enqueue(async () => {
      this.adapter.dispose();
      this.adapter = this.factory(terminalSize);
      await this.writeBytes(payload);
      await this.writeBytes(inputPrefix);
    });
  }

  recreate(terminalSize: TerminalSize): Promise<void> {
    return this.enqueue(() => {
      this.adapter.dispose();
      this.adapter = this.factory(terminalSize);
    });
  }

  resize(terminalSize: TerminalSize): Promise<void> {
    return this.enqueue(() => {
      this.adapter.resize(terminalSize.columns, terminalSize.rows);
    });
  }

  proposeDimensions(): ProposedDimensions | null {
    return this.adapter.proposeDimensions?.() ?? null;
  }

  focus(): void {
    this.adapter.focus?.();
  }

  dispose(): void {
    this.adapter.dispose();
  }

  private enqueue(operation: () => void | Promise<void>): Promise<void> {
    const result = this.operationTail.then(operation);
    this.operationTail = result.catch(() => undefined);
    return result;
  }

  private writeBytes(data: Uint8Array): Promise<void> {
    if (data.length === 0) {
      return Promise.resolve();
    }
    return new Promise((resolve) => {
      this.adapter.write(data, resolve);
    });
  }
}
