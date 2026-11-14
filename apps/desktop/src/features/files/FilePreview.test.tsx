// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { FilePreviewRequest, PreviewSnapshot } from "../../lib/types";
import { LocaleProvider } from "../i18n/LocaleContext";
import { FilePreviewProvider, PreviewPanel, PreviewWindowPage, useFilePreview } from "./FilePreview";

const api = vi.hoisted(() => ({ openPreviewWindow: vi.fn(), closePreviewWindow: vi.fn(), openExternalUrl: vi.fn(), loadPreviewWindow: vi.fn(), loadFilePreview: vi.fn() }));
vi.mock("../../lib/tauri", () => ({ ...api, setNativeWindowTitle: vi.fn() }));
vi.mock("./previewModel", () => ({ loadFilePreview: api.loadFilePreview }));

const snapshot: PreviewSnapshot = { target: { kind: "local" }, path: "/work/readme.md", title: "readme.md", line: null,
  column: null, content: "# Read me", kind: "markdown", scroll_top: 0, truncated: false };
const request: FilePreviewRequest = { target: { kind: "local" }, path: "readme.md", cwd: "/work", line: null, column: null };

beforeEach(() => { vi.clearAllMocks(); api.openPreviewWindow.mockResolvedValue("preview-id"); api.openExternalUrl.mockResolvedValue(undefined); api.closePreviewWindow.mockResolvedValue(undefined); });
afterEach(() => { cleanup(); localStorage.removeItem("ctmux.desktop.language"); vi.restoreAllMocks(); });

function Actions() {
  const previews = useFilePreview()!;
  return <><button onClick={() => previews.open(request)}>First</button><button onClick={() => previews.open({ ...request, path: "other.md" })}>Second</button></>;
}

describe("file preview", () => {
  it("finishes the originating gesture's focus handoff while a remote file is loading", () => {
    let next_frame: FrameRequestCallback = () => {};
    vi.spyOn(window, "requestAnimationFrame").mockImplementation((callback) => { next_frame = callback; return 1; });
    vi.spyOn(window, "cancelAnimationFrame").mockImplementation(() => {});
    api.loadFilePreview.mockReturnValue(new Promise(() => {}));
    const terminal_key = vi.fn();
    const view = render(<FilePreviewProvider><textarea aria-label="Terminal" onKeyDown={terminal_key} /><Actions /></FilePreviewProvider>);
    const terminal = screen.getByRole("textbox", { name: "Terminal" });
    terminal.focus();
    fireEvent.click(screen.getByText("First"));
    const content = view.container.querySelector<HTMLDivElement>(".file-preview-content")!;
    expect(document.activeElement).toBe(content);
    terminal.focus(); // A trailing native mouse/key gesture can restore xterm.
    act(() => next_frame(0));
    expect(document.activeElement).toBe(content);
    expect(screen.getByRole("status").textContent).toContain("Loading");
    fireEvent.keyDown(content, { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(api.loadFilePreview.mock.calls[0][1].aborted).toBe(true);
    expect(terminal_key).not.toHaveBeenCalled();
  });

  it("keeps deliberate terminal focus during the initial nonmodal handoff", () => {
    let next_frame: FrameRequestCallback = () => {};
    vi.spyOn(window, "requestAnimationFrame").mockImplementation((callback) => { next_frame = callback; return 1; });
    vi.spyOn(window, "cancelAnimationFrame").mockImplementation(() => {});
    api.loadFilePreview.mockReturnValue(new Promise(() => {}));
    render(<FilePreviewProvider><textarea aria-label="Terminal" /><Actions /></FilePreviewProvider>);
    const terminal = screen.getByRole("textbox", { name: "Terminal" });
    terminal.focus();
    fireEvent.click(screen.getByText("First"));
    fireEvent.pointerDown(terminal);
    terminal.focus();
    act(() => next_frame(0));
    expect(document.activeElement).toBe(terminal);
    expect(screen.getByRole("dialog")).toBeTruthy();
  });

  it("focuses the nonmodal preview and restores its source focus when Escape closes it", async () => {
    api.loadFilePreview.mockResolvedValue({ snapshot, truncated: false });
    const view = render(<FilePreviewProvider><Actions /></FilePreviewProvider>);
    const source = screen.getByText("First");
    source.focus();
    fireEvent.click(source);
    await screen.findByRole("heading", { name: "Read me" });
    const content = view.container.querySelector<HTMLDivElement>(".file-preview-content")!;
    expect(document.activeElement).toBe(content);
    expect(screen.getByRole("dialog").getAttribute("aria-modal")).toBe("false");
    fireEvent.keyDown(content, { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(document.activeElement).toBe(source);
  });

  it("opens a dedicated window from its snapshot without fetching the file again", async () => {
    api.loadPreviewWindow.mockResolvedValue({ ...snapshot, scroll_top: 240, truncated: true });
    const view = render(<PreviewWindowPage />);
    await screen.findByRole("heading", { name: "Read me" });
    expect(api.loadPreviewWindow).toHaveBeenCalledOnce();
    expect(api.loadFilePreview).not.toHaveBeenCalled();
    expect(view.container.querySelector<HTMLDivElement>(".file-preview-content")!.scrollTop).toBe(240);
    expect(screen.getByRole("status").textContent).toContain("first 1 MiB");
    fireEvent.keyDown(screen.getByRole("region"), { key: "w", code: "KeyW", ctrlKey: true });
    expect(api.closePreviewWindow).toHaveBeenCalledOnce();
  });

  it("restores dedicated image scrolling after decoding without overriding later user scrolling", () => {
    let decoded = false;
    let scroll_top = 0;
    vi.spyOn(Element.prototype, "scrollTop", "set").mockImplementation(function (this: Element, value: number) {
      if (this.classList.contains("file-preview-content")) scroll_top = decoded ? value : 0;
    });
    const view = render(<PreviewPanel dedicated snapshot={{ ...snapshot, kind: "image", content: "data:image/png;base64,aW1hZ2U=", scroll_top: 240 }} error={null} on_retry={() => {}} />);
    const content = view.container.querySelector<HTMLDivElement>(".file-preview-content")!;
    expect(scroll_top).toBe(0);
    decoded = true;
    fireEvent.load(screen.getByRole("img"));
    expect(scroll_top).toBe(240);
    content.scrollTop = 120;
    fireEvent.load(screen.getByRole("img"));
    expect(scroll_top).toBe(120);
  });

  it("cancels replaced requests and ignores a late response from the previous file", async () => {
    let first: (value: unknown) => void = () => {};
    api.loadFilePreview.mockImplementationOnce(() => new Promise((resolve) => { first = resolve; }))
      .mockResolvedValueOnce({ snapshot: { ...snapshot, title: "other.md", content: "# Current" }, truncated: false });
    render(<FilePreviewProvider><Actions /></FilePreviewProvider>);
    fireEvent.click(screen.getByText("First"));
    expect(screen.getByRole("status").textContent).toContain("Loading");
    fireEvent.click(screen.getByText("Second"));
    await screen.findByRole("heading", { name: "Current" });
    expect(api.loadFilePreview.mock.calls[0][1].aborted).toBe(true);
    first({ snapshot, truncated: false });
    await Promise.resolve();
    expect(screen.queryByRole("heading", { name: "Read me" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Close file preview" }));
    expect(api.loadFilePreview.mock.calls[1][1].aborted).toBe(true);
  });

  it("moves the content and scroll position into a dedicated window", async () => {
    api.loadFilePreview.mockResolvedValue({ snapshot, truncated: false });
    const view = render(<FilePreviewProvider><Actions /></FilePreviewProvider>);
    fireEvent.click(screen.getByText("First"));
    await screen.findByRole("heading", { name: "Read me" });
    view.container.querySelector<HTMLDivElement>(".file-preview-content")!.scrollTop = 240;
    fireEvent.click(screen.getByRole("button", { name: "Open window" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(api.openPreviewWindow).toHaveBeenCalledExactlyOnceWith({ ...snapshot, scroll_top: 240 }, "readme.md — Preview");
  });

  it("passes the chosen language to the native title and preserves the file name and contents", async () => {
    localStorage.setItem("ctmux.desktop.language", "zh-Hans");
    api.loadFilePreview.mockResolvedValue({ snapshot, truncated: false });
    render(<LocaleProvider><FilePreviewProvider><Actions /></FilePreviewProvider></LocaleProvider>);
    fireEvent.click(screen.getByText("First"));
    await screen.findByRole("heading", { name: "Read me" });
    fireEvent.click(screen.getByRole("button", { name: "打开窗口" }));
    await waitFor(() => expect(api.openPreviewWindow).toHaveBeenCalledExactlyOnceWith(snapshot, "readme.md — 预览"));
  });

  it("renders Markdown without running HTML or loading referenced images", async () => {
    render(<PreviewPanel snapshot={{ ...snapshot, content: '# Hello\n<script>alert(1)</script>\n\n![tracking](https://example.com/pixel.png)\n\n[Docs](https://example.com/docs)' }} error={null} on_retry={() => {}} />);
    expect(screen.getByRole("heading", { name: "Hello" })).toBeTruthy();
    expect(document.querySelector("script")).toBeNull();
    expect(document.querySelector("img")).toBeNull();
    fireEvent.click(screen.getByRole("link", { name: "Docs" }));
    expect(api.openExternalUrl).toHaveBeenCalledExactlyOnceWith("https://example.com/docs");
  });

  it("keeps local Markdown destinations out of DOM navigation and rejects unsafe schemes", () => {
    const open_file = vi.fn();
    render(<PreviewPanel snapshot={{ ...snapshot, content: '[Source](file:///work/main.rs:12)\n\n[Script](javascript:alert%281%29)\n\n[Whitespace](java&#x0a;script:alert%281%29)\n\n[Data](data:text/html,hello)\n\n[Other](blob:https://example.com/id)' }} error={null} on_retry={() => {}} on_open_file={open_file} />);
    const source = screen.getByRole("link", { name: "Source" });
    expect(source.getAttribute("href")).toBe("#");
    fireEvent.click(source);
    expect(open_file).toHaveBeenCalledExactlyOnceWith({ target: snapshot.target, cwd: "/work", path: "/work/main.rs", line: 12, column: null });
    for (const name of ["Script", "Whitespace", "Data", "Other"]) expect(screen.getByText(name).getAttribute("href"), name).toBeNull();
    expect(api.openExternalUrl).not.toHaveBeenCalled();
  });

  it("preserves failed promotion content and shows its error", async () => {
    const promote = vi.fn().mockRejectedValue(new Error("Window limit reached"));
    render(<PreviewPanel snapshot={snapshot} error={null} on_retry={() => {}} on_promote={promote} />);
    fireEvent.click(screen.getByRole("button", { name: "Open window" }));
    expect(await screen.findByRole("alert")).toHaveProperty("textContent", "Window limit reached");
    expect(screen.getByRole("heading", { name: "Read me" })).toBeTruthy();
  });
});
