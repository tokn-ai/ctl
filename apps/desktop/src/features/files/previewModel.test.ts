import { beforeEach, describe, expect, it, vi } from "vitest";
import type { FilePreviewRequest, FileResponse } from "../../lib/types";
import { IMAGE_PREVIEW_LIMIT, loadFilePreview, TEXT_PREVIEW_LIMIT } from "./previewModel";

const api = vi.hoisted(() => ({ fileRequest: vi.fn() }));
vi.mock("../../lib/tauri", () => api);

const request: FilePreviewRequest = { target: { kind: "ssh", destination: "test-host" }, cwd: "/work/project", path: "readme.md", line: null, column: null };
const metadata = (name = "readme.md", size = 8): FileResponse => ({ kind: "metadata", path: `/work/project/${name}`,
  entry: { name, path: `/work/project/${name}`, entry_type: "file", size, is_symlink: false } });
const file = (content: string, eof = true): FileResponse => ({ kind: "file", path: "/work/project/readme.md",
  data_base64: btoa(content), offset: 0, total_size: content.length, eof });

beforeEach(() => vi.clearAllMocks());

describe("file preview loading", () => {
  it("resolves paths on their source host before reading bounded content", async () => {
    api.fileRequest.mockResolvedValueOnce(metadata()).mockResolvedValueOnce(file("# Hello"));
    const signal = new AbortController().signal;
    const result = await loadFilePreview(request, signal);
    expect(api.fileRequest.mock.calls.map(([value]) => value)).toEqual([
      { request_id: expect.any(String), target: request.target, operation: { kind: "metadata", path: "readme.md", cwd: "/work/project" } },
      { request_id: expect.any(String), target: request.target, operation: { kind: "read_file", path: "/work/project/readme.md", cwd: null, offset: 0, length: TEXT_PREVIEW_LIMIT } },
    ]);
    expect(api.fileRequest.mock.calls.every((call) => call[1] === signal)).toBe(true);
    expect(result.snapshot).toMatchObject({ target: request.target, kind: "markdown", content: "# Hello", truncated: false });
  });

  it("uses source view when a diagnostic names a line", async () => {
    api.fileRequest.mockResolvedValueOnce(metadata()).mockResolvedValueOnce(file("# Hello"));
    const { snapshot } = await loadFilePreview({ ...request, line: 4, column: 2 }, new AbortController().signal);
    expect(snapshot).toMatchObject({ kind: "text", line: 4, column: 2 });
  });

  it.each([
    { name: "photo.PNG", kind: "image", data_base64: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aSFEAAAAASUVORK5CYII=", length: IMAGE_PREVIEW_LIMIT },
    { name: "readme.md", kind: "markdown", data_base64: btoa("# Linked document"), length: TEXT_PREVIEW_LIMIT },
  ])("previews a $name symlink by its filename while reading the canonical target", async ({ name, kind, data_base64, length }) => {
    const canonical_path = "/store/content";
    api.fileRequest.mockResolvedValueOnce({ kind: "metadata", path: canonical_path,
      entry: { name, path: canonical_path, entry_type: "file", size: atob(data_base64).length, is_symlink: true } })
      .mockResolvedValueOnce({ kind: "file", path: canonical_path, data_base64, offset: 0, total_size: atob(data_base64).length, eof: true });
    const result = await loadFilePreview({ ...request, path: name }, new AbortController().signal);
    expect(result.snapshot).toMatchObject({ kind, path: canonical_path, title: name,
      content: kind === "image" ? `data:image/png;base64,${data_base64}` : "# Linked document" });
    expect(api.fileRequest.mock.calls[1][0].operation).toEqual({ kind: "read_file", path: canonical_path, cwd: null, offset: 0, length });
  });

  it("reports missing files without attempting a read", async () => {
    api.fileRequest.mockResolvedValueOnce({ kind: "metadata", path: "/missing", entry: null });
    await expect(loadFilePreview(request, new AbortController().signal)).rejects.toThrow("no longer exists");
    expect(api.fileRequest).toHaveBeenCalledOnce();
  });

  it("rejects binary content and keeps partial text explicitly truncated", async () => {
    api.fileRequest.mockResolvedValueOnce(metadata()).mockResolvedValueOnce(file("binary\0file"));
    await expect(loadFilePreview(request, new AbortController().signal)).rejects.toThrow("Binary files");
    api.fileRequest.mockResolvedValueOnce(metadata()).mockResolvedValueOnce(file("first part", false));
    const result = await loadFilePreview(request, new AbortController().signal);
    expect(result.snapshot.truncated).toBe(true);
  });

  it("refuses oversized images before transfer and turns bounded images into data URLs", async () => {
    api.fileRequest.mockResolvedValueOnce(metadata("image.png", IMAGE_PREVIEW_LIMIT + 1));
    await expect(loadFilePreview(request, new AbortController().signal)).rejects.toThrow("4 MiB");
    api.fileRequest.mockResolvedValueOnce(metadata("image.png")).mockResolvedValueOnce(file("image"));
    const result = await loadFilePreview(request, new AbortController().signal);
    expect(result.snapshot).toMatchObject({ kind: "image", content: `data:image/png;base64,${btoa("image")}` });
  });
});
