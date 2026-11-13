import type { FileLink } from "../../lib/types";

function positive(value: string | undefined): number | null {
  if (!value) return null;
  const parsed = Number(value);
  return Number.isSafeInteger(parsed) && parsed > 0 && parsed <= 0xffff_ffff ? parsed : null;
}

/** File URLs always refer to the terminal's host; they never select another host. */
export function parseFileLink(value: string): FileLink | null {
  if (!value || value.trim() !== value || /[\u0000-\u001f\u007f-\u009f]/.test(value)) return null;
  if (/^(?:https?|ftp|ssh|mailto|data|javascript|vbscript):/i.test(value)) return null;
  let path = value;
  const file_url = /^file:\/\//i.test(value);
  let line: number | null = null;
  let column: number | null = null;
  if (file_url) {
    try {
      const url = new URL(value);
      if (url.protocol !== "file:" || url.username || url.password || url.port || url.search) return null;
      path = decodeURIComponent(url.pathname);
      const fragment = /^#L?(\d+)(?::(\d+))?$/.exec(url.hash);
      if (url.hash && !fragment) return null;
      line = positive(fragment?.[1]);
      column = positive(fragment?.[2]);
      if (fragment && (!line || fragment[2] && !column)) return null;
      if (/^\/[A-Za-z]:\//.test(path)) path = path.slice(1);
    } catch { return null; }
  }
  const location = /:(\d+)(?::(\d+))?$/.exec(path);
  if (location) {
    line = positive(location[1]);
    column = positive(location[2]);
    if (!line || location[2] && !column) return null;
    path = path.slice(0, location.index);
  }
  if (/^[A-Za-z][A-Za-z\d+.-]*:/.test(path) && !/^[A-Za-z]:[\\/]/.test(path)) return null;
  if (!file_url && path.includes(":") && !/^[A-Za-z]:[\\/]/.test(path)) return null;
  if (!path || /[\u0000-\u001f\u007f-\u009f]/.test(path)) return null;
  return { path, line, column };
}

export interface DetectedFileLink {
  text: string;
  start: number;
  end: number;
  file: FileLink;
}

/** Conservative candidates; the file service confirms existence before preview. */
export function detectFileLinks(text: string): DetectedFileLink[] {
  const found: DetectedFileLink[] = [];
  const tokens = /(?:"([^"\r\n]+)"|'([^'\r\n]+)'|[^\s<>"'`|()[\]{}]+)/g;
  for (const match of text.matchAll(tokens)) {
    const quoted = match[1] ?? match[2];
    const token = (quoted ?? match[0]).replace(/[.,;!]+$/, "");
    if (!token || /^(?:https?:\/\/|[\w.+-]+@)/i.test(token)) continue;
    const file = parseFileLink(token);
    if (!file || !(/[/\\]/.test(file.path) || /(?:^|\.)[\w-]+\.[\w-]+$/.test(file.path) || /^\.[\w-]+$/.test(file.path) || /^(?:README|LICENSE|COPYING|NOTICE|Makefile|Dockerfile|Containerfile|Gemfile|Justfile)$/i.test(file.path))) continue;
    const start = match.index! + (quoted === undefined ? 0 : 1);
    found.push({ text: token, start, end: start + token.length, file });
    if (found.length === 32) break;
  }
  return found;
}
