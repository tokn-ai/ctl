import { SerializeAddon } from "@xterm/addon-serialize";
import { loadTerminalSnapshot, saveTerminalSnapshot } from "./offlineCache";
import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { encodeTerminalBinary, encodeTerminalText } from "../../lib/bytes";
import type { SessionSummary, TerminalSize } from "../../lib/types";
import { sessionKey } from "../targets/targets";
import {
  TerminalPresenter,
  type ProposedDimensions,
  type TerminalAdapter,
} from "./TerminalPresenter";

const DEFAULT_SCROLLBACK_LINES = 10_000;
const MAX_TERMINAL_DIMENSION = 65_535;

interface CachedTerminal {
  container: HTMLElement;
  presenter: TerminalPresenter;
  resume_from: string | null;
  presentation_version: number;
  is_local: boolean;
  terminal_id?: string;
  session?: SessionSummary;
  snapshot_timer?: ReturnType<typeof setTimeout>;
  offline_snapshot?: boolean;
  has_content?: boolean;
}

function validDimensions(
  dimensions: ProposedDimensions | null,
): dimensions is ProposedDimensions {
  return (
    dimensions !== null &&
    Number.isInteger(dimensions.columns) &&
    Number.isInteger(dimensions.rows) &&
    dimensions.columns >= 2 &&
    dimensions.rows >= 1 &&
    dimensions.columns <= MAX_TERMINAL_DIMENSION &&
    dimensions.rows <= MAX_TERMINAL_DIMENSION
  );
}

export class XtermRenderer {
  private static readonly renderers = new Set<XtermRenderer>();

  async archivePanes(session: SessionSummary, reason: string) {
    const panes = new Map<string, { terminal_id: string; reason: string; lines: string[] }>();
    for (const renderer of XtermRenderer.renderers) {
      const terminal = renderer.sessions.get(sessionKey(session));
      if (!terminal) continue;
      const terminal_id = terminal.terminal_id ?? session.session_id;
      panes.set(terminal_id, { terminal_id, reason, lines: await terminal.presenter.copyLines() });
    }
    return [...panes.values()];
  }

  private cellObserver: ResizeObserver | null = null;
  private cellFrame: number | null = null;
  private readonly cellListeners = new Set<(cell: { width: number; height: number }) => void>();

  observeCellDimensions(listener: (cell: { width: number; height: number }) => void): () => void {
    this.cellListeners.add(listener);
    if (!this.cellObserver) {
      this.cellObserver = new ResizeObserver(() => this.scheduleCellMeasurement());
      this.container.querySelectorAll(".xterm-screen").forEach((screen) => this.cellObserver!.observe(screen));
    }
    this.scheduleCellMeasurement();
    return () => {
      this.cellListeners.delete(listener);
      if (!this.cellListeners.size) {
        this.cellObserver?.disconnect();
        this.cellObserver = null;
        if (this.cellFrame !== null) cancelAnimationFrame(this.cellFrame);
        this.cellFrame = null;
      }
    };
  }

  private scheduleCellMeasurement(): void {
    if (this.cellFrame !== null) return;
    this.cellFrame = requestAnimationFrame(() => {
      this.cellFrame = null;
      const cell = this.cellDimensions();
      if (cell) this.cellListeners.forEach((listener) => listener(cell));
    });
  }
  private viewport: HTMLElement | null = null;
  setViewport(viewport: HTMLElement | null): void { this.viewport = viewport; }
  cellDimensions() { return this.active.presenter.cellDimensions(); }
  private readonly sessions = new Map<string, CachedTerminal>();
  private active: CachedTerminal;

  constructor(
    private readonly container: HTMLElement,
    private readonly onInput: (data: Uint8Array) => void,
    initialSize: TerminalSize,
  ) {
    this.active = this.createTerminal(initialSize);
    XtermRenderer.renderers.add(this);
  }

  activateSession(session: SessionSummary, use_cached_terminal = false): void {
    const key = sessionKey(session);
    let terminal = this.sessions.get(key);
    if (terminal && !use_cached_terminal && terminal.terminal_id !== session.terminal_id) {
      this.sessions.delete(key);
      if (terminal !== this.active) this.disposeTerminal(terminal);
      terminal = undefined;
    }
    if (terminal === this.active) return;

    void this.saveSnapshot(this.active);
    this.active.container.hidden = true;
    if (![...this.sessions.values()].includes(this.active)) {
      this.disposeTerminal(this.active);
    }
    if (!terminal) {
      terminal = this.createTerminal(session.terminal_size);
      terminal.is_local = session.target.kind === "local";
      terminal.terminal_id = session.terminal_id;
      this.sessions.set(key, terminal);
    }
    this.active = terminal;
    terminal.container.hidden = false;
    this.scheduleCellMeasurement();
  }

  rememberSession(session: SessionSummary): void {
    this.active.session = session;
    this.active.terminal_id = session.terminal_id;
  }

  async viewOffline(session: SessionSummary): Promise<boolean> {
    this.activateSession(session, true);
    if (this.active.has_content || this.active.offline_snapshot) return true;
    const snapshot = loadTerminalSnapshot(session);
    if (!snapshot) return false;
    const terminal = this.active;
    terminal.session = { ...snapshot.session, target: session.target };
    terminal.terminal_id = snapshot.session.terminal_id;
    await terminal.presenter.recreate(snapshot.session.terminal_size);
    await terminal.presenter.write(new TextEncoder().encode(snapshot.payload));
    terminal.offline_snapshot = true;
    return true;
  }

  async saveSnapshot(terminal = this.active): Promise<void> {
    if (terminal.snapshot_timer) clearTimeout(terminal.snapshot_timer);
    terminal.snapshot_timer = undefined;
    if (!terminal.session || terminal.offline_snapshot || !terminal.has_content) return;
    const session = terminal.session;
    let payload: string | null;
    try { payload = await terminal.presenter.snapshot(); }
    catch { return; } // A cache failure must not interrupt the live terminal.
    if (payload !== null) saveTerminalSnapshot({ session, payload, saved_at: Date.now() });
  }

  resumeSequence(): string | null {
    return this.active.offline_snapshot ? null : this.active.resume_from;
  }

  invalidateResumeSequence(): void {
    this.active.resume_from = null;
    this.active.presentation_version += 1;
  }

  retainSessions(session_keys: ReadonlySet<string>): void {
    for (const [key, terminal] of this.sessions) {
      if (!session_keys.has(key)) {
        this.sessions.delete(key);
        // The current view remains visible until the next activation. Removing
        // it from the cache prevents a closed tab from reusing its cursor.
        if (terminal !== this.active) this.disposeTerminal(terminal);
      }
    }
  }

  remapSessions(key_changes: ReadonlyMap<string, string>): void {
    for (const [old_key, new_key] of key_changes) {
      if (old_key === new_key) continue;
      const terminal = this.sessions.get(old_key);
      if (!terminal) continue;
      this.sessions.delete(old_key);
      if (!this.sessions.has(new_key)) this.sessions.set(new_key, terminal);
      else if (terminal !== this.active) this.disposeTerminal(terminal);
    }
  }

  forgetLocalSessions(): void {
    this.retainSessions(
      new Set(
        [...this.sessions]
          .filter(([, terminal]) => !terminal.is_local)
          .map(([key]) => key),
      ),
    );
    if (this.active.is_local) this.invalidateResumeSequence();
  }

  write(data: Uint8Array, sequence_end: string): Promise<void> {
    return this.applyPresentation(sequence_end, (presenter) =>
      presenter.write(data),
    );
  }

  restoreCheckpoint(
    terminalSize: TerminalSize,
    historyLines: string[],
    payload: Uint8Array,
    inputPrefix: Uint8Array,
    sequence: string,
  ): Promise<void> {
    return this.applyPresentation(sequence, (presenter) =>
      presenter.restoreCheckpoint(terminalSize, historyLines, payload, inputPrefix),
    );
  }

  recreate(terminalSize: TerminalSize): Promise<void> {
    return this.applyPresentation(null, (presenter) =>
      presenter.recreate(terminalSize),
    );
  }

  resize(terminalSize: TerminalSize): Promise<void> {
    if (this.active.session) this.active.session = { ...this.active.session, terminal_size: terminalSize };
    return this.applyPresentation(this.active.resume_from, (presenter) =>
      presenter.resize(terminalSize),
    );
  }

  proposeDimensions(): ProposedDimensions | null {
    if (this.viewport) {
      const cell = this.cellDimensions();
      if (!cell) return null;
      const dimensions = { columns: Math.floor(this.viewport.clientWidth / cell.width), rows: Math.floor(this.viewport.clientHeight / cell.height) };
      return validDimensions(dimensions) ? dimensions : null;
    }
    const dimensions = this.active.presenter.proposeDimensions();
    return validDimensions(dimensions) ? dimensions : null;
  }

  observeDimensions(
    onDimensions: (dimensions: ProposedDimensions) => void,
  ): () => void {
    let animationFrame: number | null = null;
    const publish = () => {
      animationFrame = null;
      const dimensions = this.proposeDimensions();
      if (dimensions) {
        onDimensions(dimensions);
      }
    };
    const schedule = () => {
      if (animationFrame !== null) {
        cancelAnimationFrame(animationFrame);
      }
      animationFrame = requestAnimationFrame(publish);
    };
    const observer = new ResizeObserver(schedule);
    observer.observe(this.viewport ?? this.container);
    const stopCellObservation = this.observeCellDimensions(schedule);
    schedule();

    return () => {
      observer.disconnect();
      stopCellObservation();
      if (animationFrame !== null) {
        cancelAnimationFrame(animationFrame);
      }
    };
  }

  focus(): void {
    this.active.presenter.focus();
  }

  dispose(): void {
    XtermRenderer.renderers.delete(this);
    this.cellObserver?.disconnect();
    this.cellObserver = null;
    this.cellListeners.clear();
    if (this.cellFrame !== null) cancelAnimationFrame(this.cellFrame);
    this.cellFrame = null;
    for (const terminal of new Set([...this.sessions.values(), this.active])) {
      this.disposeTerminal(terminal);
    }
    this.sessions.clear();
  }

  private createTerminal(terminalSize: TerminalSize): CachedTerminal {
    const container = document.createElement("div");
    container.className = "terminal-session";
    this.container.append(container);
    return {
      container,
      presenter: new TerminalPresenter(
        (size) => this.createAdapter(container, size),
        terminalSize,
      ),
      resume_from: null,
      presentation_version: 0,
      is_local: false,
    };
  }

  private disposeTerminal(terminal: CachedTerminal): void {
    void this.saveSnapshot(terminal);
    terminal.container.querySelectorAll(".xterm-screen").forEach((screen) => this.cellObserver?.unobserve(screen));
    terminal.presenter.dispose();
    terminal.container.remove();
  }

  private async applyPresentation(
    sequence: string | null,
    operation: (presenter: TerminalPresenter) => Promise<void>,
  ): Promise<void> {
    // Capture the view before awaiting: activation may change while xterm is
    // parsing bytes. Only a fully applied presentation is safe to resume.
    const terminal = this.active;
    const version = ++terminal.presentation_version;
    terminal.resume_from = null;
    await operation(terminal.presenter);
    if (terminal.presentation_version === version) {
      terminal.resume_from = sequence;
      terminal.has_content = sequence !== null;
      terminal.offline_snapshot = false;
      if (!terminal.snapshot_timer) {
        terminal.snapshot_timer = setTimeout(() => { void this.saveSnapshot(terminal); }, 2000);
      }
    }
  }

  private createAdapter(
    container: HTMLElement,
    terminalSize: TerminalSize,
  ): TerminalAdapter {
    container.querySelectorAll(".xterm-screen").forEach((screen) => this.cellObserver?.unobserve(screen));
    container.replaceChildren();
    const terminal = new Terminal({
      cols: terminalSize.columns,
      rows: terminalSize.rows,
      allowTransparency: false,
      convertEol: false,
      cursorBlink: true,
      cursorStyle: "bar",
      fontFamily: '"Berkeley Mono", "SFMono-Regular", Consolas, monospace',
      fontSize: 13,
      lineHeight: 1.18,
      scrollback: DEFAULT_SCROLLBACK_LINES,
      theme: {
        background: "#1f1f1f",
        foreground: "#cccccc",
        cursor: "#aeafad",
        cursorAccent: "#1f1f1f",
        selectionBackground: "#264f78",
        black: "#20242b",
        red: "#ff6b6b",
        green: "#8ee39d",
        yellow: "#f3c969",
        blue: "#7aa2f7",
        magenta: "#c099ff",
        cyan: "#72d6d0",
        white: "#e4e7ec",
        brightBlack: "#6c7380",
        brightRed: "#ff8787",
        brightGreen: "#a5efb2",
        brightYellow: "#ffe08a",
        brightBlue: "#9ab7ff",
        brightMagenta: "#d1b2ff",
        brightCyan: "#92e8e3",
        brightWhite: "#ffffff",
      },
    });
    const fitAddon = new FitAddon();
    terminal.loadAddon(fitAddon);
    const serializer = new SerializeAddon();
    terminal.loadAddon(serializer);
    terminal.open(container);
    const screen = container.querySelector(".xterm-screen");
    if (screen) this.cellObserver?.observe(screen);
    this.scheduleCellMeasurement();
    terminal.onData((data) => {
      if (this.active.container === container) this.onInput(encodeTerminalText(data));
    });
    terminal.onBinary((data) => {
      if (this.active.container === container) this.onInput(encodeTerminalBinary(data));
    });

    return {
      snapshot: () => serializer.serialize({ scrollback: 2000 }),
      copyLines: () => {
        const buffer = terminal.buffer.active;
        return Array.from({ length: buffer.length }, (_, index) => buffer.getLine(index)?.translateToString(true) ?? "");
      },
      write: (data, callback) => terminal.write(data, callback),
      resize: (columns, rows) => terminal.resize(columns, rows),
      dispose: () => terminal.dispose(),
      focus: () => terminal.focus(),
      cellDimensions: () => {
        const screen = container.querySelector(".xterm-screen")?.getBoundingClientRect();
        return screen && screen.width > 0 && screen.height > 0 ? { width: screen.width / terminal.cols, height: screen.height / terminal.rows } : null;
      },
      proposeDimensions: () => {
        const proposed = fitAddon.proposeDimensions();
        if (!proposed) {
          return null;
        }
        return {
          columns: proposed.cols,
          rows: proposed.rows,
        };
      },
    };
  }
}
