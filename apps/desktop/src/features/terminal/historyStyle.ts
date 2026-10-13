import type { IBufferCell, IBufferLine } from "@xterm/xterm";
import type { TerminalColor, TerminalHistoryRow, TerminalStyle } from "../../lib/types";

const attributes = [
  ["bold", 1], ["faint", 2], ["italic", 3], ["underline", 4],
  ["blink", 5], ["inverse", 7], ["strikethrough", 9],
] as const;

function byte(value: number): number {
  if (!Number.isInteger(value) || value < 0 || value > 255) throw new Error("Invalid history color");
  return value;
}

function colorSgr(color: TerminalColor | undefined, foreground: boolean): number[] {
  if (color === undefined) return [];
  const target = foreground ? 38 : 48;
  switch (color.kind) {
    case "indexed": return [target, 5, byte(color.index)];
    case "rgb": return [target, 2, byte(color.red), byte(color.green), byte(color.blue)];
    default: throw new Error("Invalid history color kind");
  }
}

function styleSgr(style: TerminalStyle): string {
  const values = [0];
  for (const [name, code] of attributes) {
    if (style[name] !== undefined && typeof style[name] !== "boolean") throw new Error("Invalid history attribute");
    if (style[name]) values.push(code);
  }
  values.push(...colorSgr(style.foreground, true), ...colorSgr(style.background, false));
  return `\u001b[${values.join(";")}m`;
}

/** Generate SGR from typed attributes; literal rows cannot inject terminal controls. */
export function historyRowAnsi(row: TerminalHistoryRow): string {
  if (/[\u0000-\u001f\u007f-\u009f]/.test(row.text)) throw new Error("Invalid control character in terminal history");
  if (!row.styles?.length) return row.text;
  // Protocol offsets count Unicode scalar values, not UTF-16 code units or cells.
  const characters = [...row.text];
  let position = 0;
  let ansi = "\u001b[0m";
  for (const span of row.styles) {
    if (!Number.isInteger(span.start) || !Number.isInteger(span.length) || span.start < position ||
      span.start < 0 || span.length <= 0 || span.start > 0xffffffff || span.length > 0xffffffff ||
      span.start + span.length > characters.length) throw new Error("Invalid history style range");
    ansi += characters.slice(position, span.start).join("") + styleSgr(span.style) +
      characters.slice(span.start, span.start + span.length).join("") + "\u001b[0m";
    position = span.start + span.length;
  }
  return ansi + characters.slice(position).join("");
}

function captureColor(value: number, rgb: boolean): TerminalColor {
  return rgb ? { kind: "rgb", red: (value >> 16) & 255, green: (value >> 8) & 255, blue: value & 255 } :
    { kind: "indexed", index: value };
}

function captureStyle(cell: IBufferCell): TerminalStyle {
  const style: TerminalStyle = {};
  if (!cell.isFgDefault()) style.foreground = captureColor(cell.getFgColor(), !!cell.isFgRGB());
  if (!cell.isBgDefault()) style.background = captureColor(cell.getBgColor(), !!cell.isBgRGB());
  if (cell.isBold()) style.bold = true;
  if (cell.isDim()) style.faint = true;
  if (cell.isItalic()) style.italic = true;
  if (cell.isUnderline()) style.underline = true;
  if (cell.isBlink()) style.blink = true;
  if (cell.isInverse()) style.inverse = true;
  if (cell.isStrikethrough()) style.strikethrough = true;
  return style;
}

/** Preserve all physical cells, including blank backgrounds and wide glyphs. */
export function captureHistoryRow(line: IBufferLine | undefined, wrapped: boolean): TerminalHistoryRow {
  const row: TerminalHistoryRow = { text: "", wrapped, styles: [] };
  let offset = 0;
  let empty_tail = 0;
  for (let column = 0; line && column < line.length; column += 1) {
    const cell = line.getCell(column);
    if (!cell || cell.getWidth() === 0) continue;
    const text = cell.getChars() || " ";
    const length = [...text].length;
    row.text += text;
    const style = captureStyle(cell);
    empty_tail = !cell.getChars() && !Object.keys(style).length ? empty_tail + 1 : 0;
    if (Object.keys(style).length) {
      const previous = row.styles![row.styles!.length - 1];
      if (previous && previous.start + previous.length === offset && JSON.stringify(previous.style) === JSON.stringify(style)) {
        previous.length += length;
      } else {
        row.styles!.push({ start: offset, length, style });
      }
    }
    offset += length;
  }
  // Empty default cells must not become printed spaces on the primary grid:
  // xterm distinguishes them when trimming text and reflowing logical lines.
  if (!wrapped && empty_tail) row.text = row.text.slice(0, -empty_tail);
  return row;
}
