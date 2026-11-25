import { execFile } from "node:child_process";
import type { Point, Screenshot } from "./model.js";

export interface OcrOptions {
  /** Local Tesseract executable, optionally followed by fixed launcher arguments. */
  executable?: string | readonly [string, ...string[]];
  /** Installed Tesseract language or combination, e.g. eng+deu. Defaults to eng. */
  language?: string;
}

export interface TextMatch extends Point {
  text: string;
  bounds: { x: number; y: number; width: number; height: number };
}

const tsv_header = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext";
const png_signature = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);

function pngBytes(image: Screenshot): Buffer {
  if (image.data.length > 144 * 1024 * 1024) throw new RangeError("OCR screenshot exceeds 144 MiB of base64");
  const bytes = Buffer.from(image.data, "base64");
  if (image.mime_type !== "image/png" || bytes.length < 33 ||
      !bytes.subarray(0, 8).equals(png_signature) || bytes.toString("ascii", 12, 16) !== "IHDR" ||
      image.width !== bytes.readUInt32BE(16) || image.height !== bytes.readUInt32BE(20) ||
      image.width < 1 || image.height < 1 || image.width > 16384 || image.height > 16384 ||
      image.width * image.height > 32 * 1024 * 1024) {
    throw new Error("OCR requires a ctream PNG screenshot with matching pixel dimensions");
  }
  return bytes;
}

function recognize(png: Buffer, { executable = "tesseract", language = "eng" }: OcrOptions): Promise<string> {
  if (!language.trim() || language.startsWith("-")) throw new Error("expected a Tesseract language");
  const [program, ...prefix] = typeof executable === "string" ? [executable] : executable;
  return new Promise((resolve, reject) => {
    let write_error: Error | undefined;
    const child = execFile(program, [...prefix, "stdin", "stdout", "-l", language, "tsv"], {
      encoding: "utf8", maxBuffer: 8 * 1024 * 1024, windowsHide: true,
    }, (error, stdout, stderr) => {
      if (error) reject(new Error(`Tesseract OCR failed: ${stderr.trim().slice(-4096) || error.message}`, { cause: error }));
      else if (write_error) reject(write_error);
      else resolve(stdout);
    });
    child.stdin!.once("error", (error: Error) => { write_error = error; });
    child.stdin!.end(png);
  });
}

interface Word {
  text: string;
  key: string;
  x: number;
  y: number;
  width: number;
  height: number;
}

function matchingWords(line: Word[], query: string[], matches: TextMatch[]): void {
  for (let start = 0; start + query.length <= line.length; start++) {
    if (!query.every((key, offset) => key === line[start + offset].key)) continue;
    const words = line.slice(start, start + query.length);
    let left = Infinity;
    let top = Infinity;
    let right = 0;
    let bottom = 0;
    for (const word of words) {
      left = Math.min(left, word.x);
      top = Math.min(top, word.y);
      right = Math.max(right, word.x + word.width);
      bottom = Math.max(bottom, word.y + word.height);
    }
    matches.push({
      text: words.map((word) => word.text).join(" "),
      x: Math.floor((left + right) / 2),
      y: Math.floor((top + bottom) / 2),
      bounds: { x: left, y: top, width: right - left, height: bottom - top },
    });
  }
}

/** Locate consecutive OCR words on a single line. Each match includes a clickable center in screenshot pixels. */
export async function findText(screenshot: Screenshot, query: string, options: OcrOptions = {}): Promise<TextMatch[]> {
  if (Buffer.byteLength(query) > 64 * 1024) throw new RangeError("OCR query exceeds 64 KiB");
  const phrase = query.trim().normalize("NFC").toLowerCase();
  if (!phrase) throw new Error("OCR query must contain text");
  const keys = phrase.split(/\s+/u);
  const tsv = await recognize(pngBytes(screenshot), options);
  const [header, ...rows] = tsv.split(/\r?\n/u);
  if (header !== tsv_header) throw new Error("Tesseract returned an invalid TSV header");
  const matches: TextMatch[] = [];
  let line_id: string | undefined;
  let words: Word[] = [];
  for (const row of rows) {
    if (!row.startsWith("5\t")) continue;
    const fields = row.split("\t");
    const id = fields.slice(1, 5).join("\t");
    if (id !== line_id) {
      matchingWords(words, keys, matches);
      words = [];
      line_id = id;
    }
    const [x, y, width, height] = fields.slice(6, 10).map(Number);
    if (fields.length !== 12 || ![x, y, width, height].every(Number.isSafeInteger) ||
        x < 0 || y < 0 || width < 1 || height < 1 ||
        x + width > screenshot.width || y + height > screenshot.height) {
      throw new Error("Tesseract returned a word with invalid pixel bounds");
    }
    const text = fields[11];
    words.push({ text, key: text.normalize("NFC").toLowerCase(), x, y, width, height });
  }
  matchingWords(words, keys, matches);
  return matches;
}
