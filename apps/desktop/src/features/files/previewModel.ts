import { decodeBase64 } from "../../lib/bytes";
import { fileRequest } from "../../lib/tauri";
import type { FilePreviewRequest, FileResponse, PreviewSnapshot } from "../../lib/types";
import { LocalizedError } from "../i18n/messages";

export const TEXT_PREVIEW_LIMIT = 1024 * 1024;
export const IMAGE_PREVIEW_LIMIT = 4 * 1024 * 1024;

const image_types: Record<string, string> = {
  png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg", gif: "image/gif",
  webp: "image/webp", avif: "image/avif", bmp: "image/bmp", ico: "image/x-icon",
};

function check(response: FileResponse): FileResponse {
  if (response.kind === "error") throw new Error(response.message);
  return response;
}

export async function loadFilePreview(request: FilePreviewRequest, signal: AbortSignal): Promise<{ snapshot: PreviewSnapshot; truncated: boolean }> {
  const metadata = check(await fileRequest({ request_id: crypto.randomUUID(), target: request.target,
    operation: { kind: "metadata", path: request.path, cwd: request.cwd } }, signal));
  if (metadata.kind !== "metadata") throw new LocalizedError("files.unexpected_response");
  if (!metadata.entry) throw new LocalizedError("files.missing");
  if (metadata.entry.entry_type !== "file") throw new LocalizedError(metadata.entry.entry_type === "directory" ? "files.directory" : "files.not_regular");
  // Resolve I/O through the canonical path, but preserve the link's filename
  // when choosing a renderer (a symlink target can be extensionless).
  const extension = /\.([^.]+)$/.exec(metadata.entry.name)?.[1].toLowerCase() ?? "";
  const image_type = image_types[extension];
  if (image_type && metadata.entry.size > IMAGE_PREVIEW_LIMIT) throw new LocalizedError("files.image_too_large");
  const response = check(await fileRequest({ request_id: crypto.randomUUID(), target: request.target,
    operation: { kind: "read_file", path: metadata.path, cwd: null, offset: 0,
      length: image_type ? IMAGE_PREVIEW_LIMIT : TEXT_PREVIEW_LIMIT } }, signal));
  if (response.kind !== "file") throw new LocalizedError("files.unexpected_response");
  const bytes = decodeBase64(response.data_base64);
  let content: string;
  if (image_type) {
    if (!response.eof) throw new LocalizedError("files.image_too_large");
    content = `data:${image_type};base64,${response.data_base64}`;
  } else {
    if (bytes.includes(0)) throw new LocalizedError("files.binary");
    try {
      // A bounded tail may stop within a UTF-8 character; streaming omits that
      // incomplete final character without disguising invalid bytes elsewhere.
      content = new TextDecoder("utf-8", { fatal: true }).decode(bytes, { stream: !response.eof });
    } catch { throw new LocalizedError("files.unsupported_encoding"); }
  }
  return { snapshot: { target: request.target, path: response.path, line: request.line, column: request.column,
    kind: image_type ? "image" : /^(md|markdown|mdown)$/.test(extension) && !request.line ? "markdown" : "text",
    content, scroll_top: 0, title: metadata.entry.name, truncated: !response.eof }, truncated: !response.eof };
}
