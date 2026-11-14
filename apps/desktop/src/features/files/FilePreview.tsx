import { createContext, useCallback, useContext, useEffect, useLayoutEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import ReactMarkdown from "react-markdown";
import { Icon } from "../../components/ui/Icon";
import { localizedErrorText } from "../../lib/errors";
import { useLocale } from "../i18n/LocaleContext";
import type { DisplayText } from "../i18n/messages";
import { validWebUrl } from "../../lib/links";
import { closePreviewWindow, loadPreviewWindow, openExternalUrl, openPreviewWindow } from "../../lib/tauri";
import type { FilePreviewRequest, PreviewSnapshot } from "../../lib/types";
import { targetLabel } from "../targets/targets";
import { useWindowTitle } from "../window/useWindowTitle";
import { parseFileLink } from "./fileLinks";
import { loadFilePreview } from "./previewModel";
import "./filePreview.css";

interface PreviewState {
  id: number;
  request: FilePreviewRequest;
  snapshot: PreviewSnapshot | null;
  error: DisplayText | null;
  truncated: boolean;
}

interface PreviewActions {
  open(request: FilePreviewRequest): void;
  focused: boolean;
}

const PreviewContext = createContext<PreviewActions | null>(null);
export function useFilePreview() { return useContext(PreviewContext); }

export function FilePreviewProvider({ children }: { children: ReactNode }) {
  const { t } = useLocale();
  const [state, setState] = useState<PreviewState | null>(null);
  const [focused, setFocused] = useState(false);
  const pending = useRef<AbortController | null>(null);
  const generation = useRef(0);
  const previous_focus = useRef<HTMLElement | null>(null);
  const open = useCallback((request: FilePreviewRequest) => {
    pending.current?.abort();
    if (document.activeElement instanceof HTMLElement && !document.activeElement.closest(".file-preview")) previous_focus.current = document.activeElement;
    const controller = new AbortController();
    pending.current = controller;
    const id = ++generation.current;
    setState({ id, request, snapshot: null, error: null, truncated: false });
    void loadFilePreview(request, controller.signal).then((loaded) => {
      if (!controller.signal.aborted) setState((current) => current?.id === id ? { ...current, ...loaded } : current);
    }, (failure) => {
      if (!controller.signal.aborted) setState((current) => current?.id === id ? { ...current, error: localizedErrorText(failure) } : current);
    });
  }, []);
  const close = useCallback(() => {
    pending.current?.abort();
    pending.current = null;
    generation.current += 1;
    setState(null);
    setFocused(false);
    if (previous_focus.current?.isConnected) previous_focus.current.focus();
  }, []);
  useEffect(() => () => pending.current?.abort(), []);
  return <PreviewContext.Provider value={{ open, focused }}>{children}{state && <PreviewPanel key={state.id}
    snapshot={state.snapshot} request={state.request} error={state.error} truncated={state.truncated}
    on_close={close} on_retry={() => open(state.request)} on_open_file={open}
    on_focus={setFocused}
    on_promote={async (snapshot) => {
      await openPreviewWindow(snapshot, t("files.native_title", { filename: snapshot.title }));
      if (generation.current === state.id) {
        pending.current?.abort();
        pending.current = null;
        setState(null);
        setFocused(false);
        if (previous_focus.current?.isConnected) previous_focus.current.focus();
      }
    }} />}</PreviewContext.Provider>;
}

export function PreviewWindowPage() {
  const { t } = useLocale();
  const [snapshot, setSnapshot] = useState<PreviewSnapshot | null>(null);
  const [error, setError] = useState<DisplayText | null>(null);
  const [attempt, setAttempt] = useState(0);
  useWindowTitle(snapshot ? t("files.host_title", { filename: snapshot.title, host: snapshot.target.kind === "local" ? t("files.local_host") : targetLabel(snapshot.target) }) : t("files.preview_title"));
  useEffect(() => {
    const closeShortcut = (event: KeyboardEvent) => {
      const primary = /Mac|iPhone|iPad|iPod/.test(navigator.platform) ? event.metaKey : event.ctrlKey;
      if (!primary || event.shiftKey || event.altKey || event.code !== "KeyW" && event.key.toLowerCase() !== "w") return;
      event.preventDefault();
      event.stopPropagation();
      void closePreviewWindow().catch((failure) => setError(localizedErrorText(failure)));
    };
    window.addEventListener("keydown", closeShortcut, true);
    return () => window.removeEventListener("keydown", closeShortcut, true);
  }, []);
  useEffect(() => {
    let disposed = false;
    setError(null);
    void loadPreviewWindow().then((value) => { if (!disposed) setSnapshot(value); },
      (failure) => { if (!disposed) setError(localizedErrorText(failure)); });
    return () => { disposed = true; };
  }, [attempt]);
  return <FilePreviewProvider><PreviewPanel dedicated snapshot={snapshot} error={error}
    on_retry={() => setAttempt((value) => value + 1)} /></FilePreviewProvider>;
}

interface PanelProps {
  snapshot: PreviewSnapshot | null;
  request?: FilePreviewRequest;
  error: DisplayText | null;
  truncated?: boolean;
  dedicated?: boolean;
  on_close?(): void;
  on_retry(): void;
  on_promote?(snapshot: PreviewSnapshot): Promise<void>;
  on_open_file?(request: FilePreviewRequest): void;
  on_focus?(focused: boolean): void;
}

export function PreviewPanel({ snapshot, request, error, truncated, dedicated = false, on_close, on_retry, on_promote, on_open_file, on_focus }: PanelProps) {
  const { t, formatText } = useLocale();
  const previews = useFilePreview();
  const panel = useRef<HTMLElement>(null);
  const scroll = useRef<HTMLDivElement>(null);
  const selected_line = useRef<HTMLElement>(null);
  const restored_image = useRef<PreviewSnapshot | null>(null);
  const [source, setSource] = useState(false);
  const [promoting, setPromoting] = useState(false);
  const [action_error, setActionError] = useState<DisplayText | null>(null);
  const [image_failed, setImageFailed] = useState(false);
  const [position, setPosition] = useState<{ left: number; top: number } | null>(null);
  const drag = useRef<{ x: number; y: number; left: number; top: number } | null>(null);
  const target = snapshot?.target ?? request?.target;
  const path = snapshot?.path ?? request?.path ?? "";
  const title = snapshot?.title ?? (path.split(/[/\\]/).pop() || t("files.preview_title"));
  const location = snapshot?.line ? `:${snapshot.line}${snapshot.column ? `:${snapshot.column}` : ""}` : "";
  useLayoutEffect(() => {
    const content = scroll.current;
    if (dedicated || !content) return;
    const source = document.activeElement;
    content.focus({ preventScroll: true });
    let interacted = false;
    const interrupt = () => { interacted = true; };
    document.addEventListener("pointerdown", interrupt, true);
    document.addEventListener("keydown", interrupt, true);
    // File links open during mouseup. Finish the focus handoff after the
    // originating native gesture, which can otherwise refocus xterm after the
    // preview mounts. A later intentional interaction keeps its own focus.
    const frame = requestAnimationFrame(() => {
      document.removeEventListener("pointerdown", interrupt, true);
      document.removeEventListener("keydown", interrupt, true);
      if (!interacted && (document.activeElement === source || document.activeElement === document.body)) {
        content.focus({ preventScroll: true });
      }
    });
    return () => {
      cancelAnimationFrame(frame);
      document.removeEventListener("pointerdown", interrupt, true);
      document.removeEventListener("keydown", interrupt, true);
    };
  }, [dedicated]);
  useEffect(() => {
    setSource(false);
    setActionError(null);
    setImageFailed(false);
    const container = scroll.current;
    if (container && snapshot) {
      container.scrollTop = dedicated ? snapshot.scroll_top
        : snapshot.scroll_top || Math.max(0, (selected_line.current?.offsetTop ?? 0) - container.offsetTop - 48);
    }
  }, [snapshot, dedicated]);
  const openLink = (href: string) => {
    if (href.startsWith("#")) {
      try { scroll.current?.querySelector(`[id="${CSS.escape(decodeURIComponent(href.slice(1)))}"]`)?.scrollIntoView(); } catch { /* Invalid fragment. */ }
      return;
    }
    if (validWebUrl(href)) {
      void openExternalUrl(href).catch((failure) => setActionError(localizedErrorText(failure)));
      return;
    }
    const file = parseFileLink(href);
    if (!file || !snapshot) return;
    const separator = snapshot.path.includes("\\") ? "\\" : "/";
    const cwd = snapshot.path.slice(0, snapshot.path.lastIndexOf(separator)) || separator;
    (on_open_file ?? previews?.open)?.({ ...file, cwd, target: snapshot.target });
  };
  const promote = async () => {
    if (!snapshot || !on_promote || promoting) return;
    setPromoting(true);
    setActionError(null);
    try {
      await on_promote({ ...snapshot, kind: source ? "text" : snapshot.kind, scroll_top: scroll.current?.scrollTop ?? 0 });
    } catch (failure) { setActionError(localizedErrorText(failure)); }
    finally { setPromoting(false); }
  };
  const text = snapshot?.content ?? "";
  const line_start = snapshot?.line ? nthLineOffset(text, snapshot.line) : -1;
  const line_end = line_start < 0 ? -1 : text.indexOf("\n", line_start);
  return <section ref={panel} role={dedicated ? "region" : "dialog"} aria-modal={dedicated ? undefined : false} data-local-shortcuts
    aria-label={t("files.preview_title")} className={`file-preview ${dedicated ? "file-preview-dedicated" : "file-preview-floating"}`}
    style={position && !dedicated ? { left: position.left, top: position.top, right: "auto" } : undefined}
    onFocusCapture={() => on_focus?.(true)} onBlurCapture={(event) => {
      if (!(event.relatedTarget instanceof Node) || !event.currentTarget.contains(event.relatedTarget)) on_focus?.(false);
    }}
    onKeyDown={(event) => { event.stopPropagation(); if (event.key === "Escape" && on_close && !promoting) { event.preventDefault(); on_close(); } }}>
    <header className="file-preview-header" onPointerDown={(event) => {
      if (dedicated || event.button !== 0 || (event.target as Element).closest("button")) return;
      const rect = panel.current!.getBoundingClientRect();
      drag.current = { x: event.clientX, y: event.clientY, left: rect.left, top: rect.top };
      event.currentTarget.setPointerCapture(event.pointerId);
    }} onPointerMove={(event) => {
      if (!drag.current) return;
      const rect = panel.current!.getBoundingClientRect();
      setPosition({ left: Math.max(0, Math.min(window.innerWidth - Math.min(rect.width, 160), drag.current.left + event.clientX - drag.current.x)),
        top: Math.max(0, Math.min(window.innerHeight - 48, drag.current.top + event.clientY - drag.current.y)) });
    }} onPointerUp={() => { drag.current = null; }} onLostPointerCapture={() => { drag.current = null; }}>
      <div className="file-preview-heading"><strong>{title}{location}</strong><span title={path}>{target && (target.kind === "local" ? t("files.local_host") : targetLabel(target))}{path && ` · ${path}`}</span></div>
      {snapshot?.kind === "markdown" && <button type="button" aria-pressed={source} onClick={() => setSource((value) => !value)}>{source ? t("files.rendered") : t("files.source")}</button>}
      {on_promote && <button type="button" disabled={!snapshot || promoting} onClick={() => void promote()} title={t("files.open_window_tooltip")}><Icon name="monitor" />{promoting ? t("files.opening") : t("files.open_window")}</button>}
      {on_close && <button type="button" disabled={promoting} onClick={on_close} aria-label={t("files.close")}><Icon name="close" /></button>}
    </header>
    {(action_error || truncated || snapshot?.truncated) && <div className="file-preview-notice" role={action_error ? "alert" : "status"}>{action_error ? formatText(action_error) : t("files.truncated")}</div>}
    <div className="file-preview-content" ref={scroll} tabIndex={0}>
      {error ? <div className="file-preview-message" role="alert"><strong>{t("files.failed")}</strong><p>{formatText(error)}</p><button type="button" onClick={on_retry}>{t("files.retry")}</button></div>
        : !snapshot ? <div className="file-preview-message" role="status"><span className="spinner" />{t("files.loading")}</div>
        : snapshot.kind === "image" ? image_failed ? <div className="file-preview-message" role="alert">{t("files.image_failed")}</div> : <div className="file-preview-image"><img src={snapshot.content} alt={snapshot.title} onError={() => setImageFailed(true)} onLoad={() => {
          // Image dimensions become scrollable only after decoding. Restore
          // once, so later load events cannot undo the reader's own scrolling.
          if (dedicated && restored_image.current !== snapshot) {
            restored_image.current = snapshot;
            if (scroll.current) scroll.current.scrollTop = snapshot.scroll_top;
          }
        }} /></div>
        : snapshot.kind === "markdown" && !source ? <article className="file-preview-markdown"><ReactMarkdown skipHtml
          urlTransform={(url) => url.startsWith("#") || validWebUrl(url) || parseFileLink(url) ? url : ""}
          components={{ a: ({ href, children }) => {
            const local_file = !!href && !href.startsWith("#") && !validWebUrl(href);
            return <a href={local_file ? "#" : href || undefined}
              onClick={(event) => { event.preventDefault(); if (href) openLink(href); }}
              onAuxClick={(event) => { if (local_file) event.preventDefault(); }}>{children}</a>;
          },
            img: ({ alt }) => <span className="file-preview-image-label">{t("files.image_label", { alt: alt ?? "" })}</span> }}>{snapshot.content}</ReactMarkdown></article>
        : <pre className="file-preview-code"><code>{line_start < 0 ? text : <>{text.slice(0, line_start)}<mark ref={selected_line}>{text.slice(line_start, line_end < 0 ? text.length : line_end) || " "}</mark>{line_end < 0 ? "" : text.slice(line_end)}</>}</code></pre>}
      {snapshot && snapshot.kind !== "image" && !snapshot.content && <p className="file-preview-empty">{t("files.empty")}</p>}
    </div>
  </section>;
}

function nthLineOffset(text: string, line: number): number {
  let offset = 0;
  for (let current = 1; current < line; current += 1) {
    const next = text.indexOf("\n", offset);
    if (next < 0) return -1;
    offset = next + 1;
  }
  return offset;
}
