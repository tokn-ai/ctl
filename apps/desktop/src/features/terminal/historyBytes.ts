/** Only complete logical lines above the primary live grid are history. */
export interface HistoryBuffer {
  baseY: number;
  getLine(index: number): { isWrapped: boolean; translateToString(trim_right?: boolean): string } | undefined;
}

const encoder = new TextEncoder();

export function terminalHistoryBytes(buffer: HistoryBuffer): number {
  let bytes = 0;
  let logical_line = "";
  for (let row = 0; row < buffer.baseY; row += 1) {
    logical_line += buffer.getLine(row)?.translateToString(false) ?? "";
    if (!buffer.getLine(row + 1)?.isWrapped) {
      // Match Rust str::trim_end (Unicode White_Space), including NEL but not BOM.
      bytes += encoder.encode(logical_line.replace(/\p{White_Space}+$/u, "")).length + 1;
      logical_line = "";
    }
  }
  // A wrapped prefix continuing into the live grid is not a complete line.
  return bytes;
}
