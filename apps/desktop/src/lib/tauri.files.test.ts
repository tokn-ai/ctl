// @vitest-environment jsdom
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { FileRequest } from "./types";

const api = vi.hoisted(() => ({ invoke: vi.fn(), destroy: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: api.invoke, Channel: class {} }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => ({ destroy: api.destroy }) }));

const request: FileRequest = { request_id: "request-1", target: { kind: "local" }, operation: { kind: "metadata", path: "/work/file", cwd: null } };

beforeEach(() => { vi.resetModules(); vi.clearAllMocks(); });

describe("file request bridge", () => {
  it("shares the current document token while each request keeps an independent identity", async () => {
    api.invoke.mockImplementation(async (command: string) => command === "file_document" ? "document-token" : { kind: "metadata", path: "/work/file", entry: null });
    const { fileRequest } = await import("./tauri");
    await Promise.all([fileRequest(request), fileRequest({ ...request, request_id: "request-2" })]);
    expect(api.invoke.mock.calls.filter(([command]) => command === "file_document")).toHaveLength(1);
    expect(api.invoke).toHaveBeenCalledWith("file_request", { request: { ...request, document_id: "document-token" } });
    expect(api.invoke).toHaveBeenCalledWith("file_request", { request: { ...request, request_id: "request-2", document_id: "document-token" } });
  });

  it("cancels only its own request and ignores a response arriving after cancellation", async () => {
    let finish: (value: unknown) => void = () => {};
    api.invoke.mockImplementation((command: string) => command === "file_request" ? new Promise((resolve) => { finish = resolve; }) : Promise.resolve(command === "file_document" ? "document-token" : undefined));
    const { fileRequest } = await import("./tauri");
    const controller = new AbortController();
    const pending = fileRequest(request, controller.signal);
    await vi.waitFor(() => expect(api.invoke).toHaveBeenCalledWith("file_request", expect.anything()));
    controller.abort();
    expect(api.invoke).toHaveBeenCalledWith("cancel_file_request", { request: { request_id: "request-1", document_id: "document-token" } });
    finish({ kind: "metadata", path: "/work/file", entry: null });
    await expect(pending).rejects.toMatchObject({ name: "AbortError" });
  });

  it("does not start a file operation cancelled while its document token loads", async () => {
    let finish: (value: string) => void = () => {};
    api.invoke.mockReturnValue(new Promise<string>((resolve) => { finish = resolve; }));
    const { fileRequest } = await import("./tauri");
    const controller = new AbortController();
    const pending = fileRequest(request, controller.signal);
    controller.abort();
    finish("document-token");
    await expect(pending).rejects.toMatchObject({ name: "AbortError" });
    expect(api.invoke.mock.calls.map(([command]) => command)).toEqual(["file_document"]);
  });
});
