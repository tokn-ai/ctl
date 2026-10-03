import type { TerminalHistoryRow, TerminalSize } from "../../lib/types";

const HISTORY_BATCH_BYTES = 16 * 1024;
const RECENT_HISTORY_BYTES = 16 * 1024;
const MAX_REPLAY_BYTES = 1024 * 1024;
const MAX_REPLAY_CHUNKS = 256;
const MAX_HISTORY_ROWS = 10_000;
const encoder = new TextEncoder();
const home = encoder.encode("\u001b[0m\u001b[H");

export interface ProposedDimensions {
  columns: number;
  rows: number;
}

export interface TerminalAdapter {
  copyLines?(): string[];
  copyPrimaryRows?(): TerminalHistoryRow[];
  activate?(scrollback_offset?: number): void;
  viewportOffset?(): number;
  write(data: Uint8Array, callback: () => void): void;
  resize(columns: number, rows: number): void;
  dispose(): void;
  focus?(): void;
  proposeDimensions?(): ProposedDimensions | null;
  cellDimensions?(): { width: number; height: number } | null;
}

export interface TerminalAdapterOptions {
  background?: boolean;
  scrollback_limit?: number;
}

export type TerminalAdapterFactory = (terminalSize: TerminalSize, options?: TerminalAdapterOptions) => TerminalAdapter;

export interface HistoryPresentation {
  terminal_size: TerminalSize;
  rows: TerminalHistoryRow[];
  payload: Uint8Array;
  input_prefix: Uint8Array;
  sequence: string;
  snapshot_id: string;
  scrollback_limit: string;
}

interface ReplayChunk {
  start: bigint;
  end: bigint;
  data: Uint8Array;
}

interface HistoryStage {
  adapter: TerminalAdapter;
  sequence: bigint;
  generation: number;
  cancelled: boolean;
}

// Cached rows contain literal cell text, never terminal commands. Reject
// controls rather than letting a malformed snapshot mutate the staged parser.
function literalRow(text: string): string {
  if (/[\u0000-\u001f\u007f-\u009f]/.test(text)) throw new Error("Invalid control character in terminal history");
  return text;
}

function recentHistory(lines: string[]): string[] {
  let bytes = 0;
  let first = lines.length;
  while (first > 0 && lines.length - first < 64) {
    const line = lines[first - 1];
    if (/[\u0000-\u001f\u007f-\u009f]/.test(line)) break;
    const size = encoder.encode(line).length + 2;
    if (bytes + size > RECENT_HISTORY_BYTES) break;
    bytes += size;
    first -= 1;
  }
  return lines.slice(first);
}

function sameGeometry(left: TerminalSize, right: TerminalSize): boolean {
  return left.columns === right.columns && left.rows === right.rows;
}

export class TerminalPresenter {
  copyLines(): Promise<string[]> { return this.operationTail.then(() => this.adapter.copyLines?.() ?? []); }
  cellDimensions() { return this.adapter.cellDimensions?.() ?? null; }
  private adapter: TerminalAdapter;
  private operationTail = Promise.resolve();
  private disposed = false;
  private generation = 0;
  private terminalSize: TerminalSize;
  private sequence: bigint | null = null;
  private checkpointSequence: bigint | null = null;
  private snapshotId: string | null = null;
  private replay: ReplayChunk[] = [];
  private replayBytes = 0;
  private stage: HistoryStage | null = null;

  constructor(private readonly factory: TerminalAdapterFactory, initialSize: TerminalSize) {
    this.terminalSize = initialSize;
    this.adapter = factory(initialSize);
  }

  write(data: Uint8Array, sequence_end?: string, sequence_start?: string): Promise<void> {
    return this.enqueue(async () => {
      await this.writeBytes(this.adapter, data);
      this.recordReplay(data, sequence_end, sequence_start);
    });
  }

  restoreCheckpoint(
    terminalSize: TerminalSize,
    historyLines: string[],
    payload: Uint8Array,
    inputPrefix: Uint8Array,
    sequence = "0",
    snapshot_id: string | null = null,
  ): Promise<void> {
    this.cancelHistory();
    const generation = this.generation;
    return this.enqueue(async () => {
      this.adapter.dispose();
      this.terminalSize = terminalSize;
      this.adapter = this.factory(terminalSize);
      this.sequence = BigInt(sequence);
      this.checkpointSequence = this.sequence;
      this.snapshotId = generation === this.generation ? snapshot_id : null;
      const recent = recentHistory(historyLines);
      if (recent.length) {
        await this.writeBytes(this.adapter, encoder.encode(`${recent.map(literalRow).join("\r\n")}\r\n${"\r\n".repeat(Math.max(0, terminalSize.rows - 1))}`));
      }
      await this.writeBytes(this.adapter, home);
      await this.writeBytes(this.adapter, payload);
      await this.writeBytes(this.adapter, inputPrefix);
      // Older daemons and disk previews supply normalized history directly.
      // Their bulk history uses the same off-view staging path.
      if (generation === this.generation && recent.length < historyLines.length && snapshot_id === null) {
        void this.startHistory({ terminal_size: terminalSize, rows: historyLines.map((text) => ({ text, wrapped: false })), payload,
          input_prefix: inputPrefix, sequence, snapshot_id: "", scrollback_limit: String(MAX_HISTORY_ROWS) }).catch(() => false);
      }
    });
  }

  syncHistory(snapshot: HistoryPresentation): Promise<boolean> {
    if (this.snapshotId === null || snapshot.snapshot_id !== this.snapshotId) return Promise.resolve(false);
    return this.startHistory(snapshot);
  }

  cancelHistory(): void {
    this.generation += 1;
    if (this.stage) this.stage.cancelled = true;
    this.stage = null;
    this.snapshotId = null;
    this.replay = [];
    this.replayBytes = 0;
    this.checkpointSequence = this.sequence;
  }

  recreate(terminalSize: TerminalSize): Promise<void> {
    this.cancelHistory();
    return this.enqueue(() => {
      this.adapter.dispose();
      this.terminalSize = terminalSize;
      this.adapter = this.factory(terminalSize);
      this.sequence = null;
      this.checkpointSequence = null;
    });
  }

  resize(terminalSize: TerminalSize): Promise<void> {
    this.cancelHistory();
    return this.enqueue(() => {
      this.terminalSize = terminalSize;
      this.adapter.resize(terminalSize.columns, terminalSize.rows);
    });
  }

  proposeDimensions(): ProposedDimensions | null { return this.adapter.proposeDimensions?.() ?? null; }
  focus(): void { this.adapter.focus?.(); }

  dispose(): void {
    if (this.disposed) return;
    this.cancelHistory();
    this.disposed = true;
    // Let each parser finish its current write before disposing it, or its
    // callback may never run. Cancelled stages dispose themselves afterwards.
    void this.operationTail.then(() => this.adapter.dispose());
  }

  private enqueue(operation: () => void | Promise<void>): Promise<void> {
    const result = this.operationTail.then(() => {
      if (!this.disposed) return operation();
    });
    this.operationTail = result.catch(() => undefined);
    return result;
  }

  private writeBytes(adapter: TerminalAdapter, data: Uint8Array): Promise<void> {
    return data.length ? new Promise((resolve) => adapter.write(data, resolve)) : Promise.resolve();
  }

  private recordReplay(data: Uint8Array, sequence_end?: string, sequence_start?: string): void {
    if (sequence_end === undefined) {
      this.cancelHistory();
      this.sequence = null;
      return;
    }
    const end = BigInt(sequence_end);
    const start = sequence_start === undefined ? end - BigInt(data.length) : BigInt(sequence_start);
    if (start !== this.sequence || end - start !== BigInt(data.length)) {
      this.cancelHistory();
      this.sequence = end;
      this.checkpointSequence = end;
      return;
    }
    this.sequence = end;
    if (!data.length) return;
    this.replay.push({ start, end, data: data.slice() });
    this.replayBytes += data.length;
    while (this.replayBytes > MAX_REPLAY_BYTES || this.replay.length > MAX_REPLAY_CHUNKS) {
      this.replayBytes -= this.replay.shift()!.data.length;
    }
    if (this.stage && this.replay[0] && this.stage.sequence < this.replay[0].start) {
      this.stage.cancelled = true;
      this.stage = null;
    }
  }

  private replayAfter(sequence: bigint): ReplayChunk[] | null {
    if (sequence === this.sequence) return [];
    const first = this.replay.findIndex((chunk) => chunk.start <= sequence && chunk.end > sequence);
    if (first < 0) return null;
    const chunks = this.replay.slice(first);
    const initial = chunks[0];
    chunks[0] = { ...initial, start: sequence, data: initial.data.subarray(Number(sequence - initial.start)) };
    return chunks;
  }

  private currentStage(stage: HistoryStage): boolean {
    return !this.disposed && !stage.cancelled && this.stage === stage && stage.generation === this.generation;
  }

  private async startHistory(snapshot: HistoryPresentation): Promise<boolean> {
    const sequence = BigInt(snapshot.sequence);
    if (this.disposed || this.sequence === null || this.checkpointSequence === null ||
      sequence < this.checkpointSequence || sequence > this.sequence ||
      !sameGeometry(snapshot.terminal_size, this.terminalSize) || this.replayAfter(sequence) === null) return false;
    const requested_limit = Number(snapshot.scrollback_limit);
    if (!Number.isSafeInteger(requested_limit) || requested_limit < 0) return false;
    if (this.stage) this.stage.cancelled = true;
    const scrollback_limit = Math.min(MAX_HISTORY_ROWS, requested_limit);
    const options = { background: true, scrollback_limit };
    const stage: HistoryStage = { adapter: this.factory(snapshot.terminal_size, options), sequence, generation: this.generation, cancelled: false };
    this.stage = stage;
    let committed = false;
    try {
      // Inspect the primary grid even if this checkpoint displays an alternate
      // screen. Its first row may complete a wrapped prefix from history.
      await this.writeStage(stage, snapshot.payload);
      const primary = stage.adapter.copyPrimaryRows?.();
      if (!primary || primary.length !== snapshot.terminal_size.rows) return false;
      stage.adapter.dispose();
      stage.adapter = this.factory(snapshot.terminal_size, options);
      const rows = [...(scrollback_limit ? snapshot.rows.slice(-scrollback_limit) : []), ...primary];
      let batch = "";
      for (let index = 0; index < rows.length; index += 1) {
        const row = rows[index];
        batch += literalRow(row.text) + (index + 1 < rows.length && !row.wrapped ? "\r\n" : "");
        if (batch.length >= 4096 || index + 1 === rows.length) {
          await this.writeStage(stage, encoder.encode(batch));
          batch = "";
        }
      }
      await this.writeStage(stage, home);
      await this.writeStage(stage, snapshot.payload);
      await this.writeStage(stage, snapshot.input_prefix);
      while (this.currentStage(stage)) {
        const replay = this.replayAfter(stage.sequence);
        if (replay === null) return false;
        for (const chunk of replay) {
          await this.writeStage(stage, chunk.data);
          stage.sequence = chunk.end;
        }
        // Only the ordered live-write barrier can publish a caught-up model.
        await this.enqueue(() => {
          if (!this.currentStage(stage) || stage.sequence !== this.sequence) return;
          const previous = this.adapter;
          const offset = previous.viewportOffset?.() ?? 0;
          try {
            stage.adapter.activate?.(offset);
          } catch (error) {
            previous.activate?.(offset);
            throw error;
          }
          this.adapter = stage.adapter;
          this.stage = null;
          committed = true;
          previous.dispose();
        });
        if (committed) return true;
      }
      return false;
    } catch {
      // History is optional: a discarded stage must not interrupt the live
      // parser, acknowledgements, or input.
      return false;
    } finally {
      if (!committed) stage.adapter.dispose();
      if (this.stage === stage) this.stage = null;
    }
  }

  private async writeStage(stage: HistoryStage, data: Uint8Array): Promise<void> {
    for (let offset = 0; offset < data.length; offset += HISTORY_BATCH_BYTES) {
      if (!this.currentStage(stage)) throw new Error("History stage cancelled");
      await this.writeBytes(stage.adapter, data.subarray(offset, offset + HISTORY_BATCH_BYTES));
      if (!this.currentStage(stage)) throw new Error("History stage cancelled");
      await new Promise<void>((resolve) => setTimeout(resolve, 0));
    }
    if (!this.currentStage(stage)) throw new Error("History stage cancelled");
  }
}
