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
  private readonly sessions = new Map<string, CachedTerminal>();
  private active: CachedTerminal;

  constructor(
    private readonly container: HTMLElement,
    private readonly onInput: (data: Uint8Array) => void,
    initialSize: TerminalSize,
  ) {
    this.active = this.createTerminal(initialSize);
  }

  activateSession(session: SessionSummary): void {
    const key = sessionKey(session);
    let terminal = this.sessions.get(key);
    if (terminal === this.active) return;

    this.active.container.hidden = true;
    if (![...this.sessions.values()].includes(this.active)) {
      this.disposeTerminal(this.active);
    }
    if (!terminal) {
      terminal = this.createTerminal(session.terminal_size);
      terminal.is_local = session.target.kind === "local";
      this.sessions.set(key, terminal);
    }
    this.active = terminal;
    terminal.container.hidden = false;
  }

  resumeSequence(): string | null {
    return this.active.resume_from;
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
    return this.applyPresentation(this.active.resume_from, (presenter) =>
      presenter.resize(terminalSize),
    );
  }

  proposeDimensions(): ProposedDimensions | null {
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
    observer.observe(this.container);
    schedule();

    return () => {
      observer.disconnect();
      if (animationFrame !== null) {
        cancelAnimationFrame(animationFrame);
      }
    };
  }

  focus(): void {
    this.active.presenter.focus();
  }

  dispose(): void {
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
    if (terminal.presentation_version === version) terminal.resume_from = sequence;
  }

  private createAdapter(
    container: HTMLElement,
    terminalSize: TerminalSize,
  ): TerminalAdapter {
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
        background: "#111318",
        foreground: "#e4e7ec",
        cursor: "#8ee39d",
        cursorAccent: "#111318",
        selectionBackground: "#395f4d99",
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
    terminal.open(container);
    terminal.onData((data) => {
      if (this.active.container === container) this.onInput(encodeTerminalText(data));
    });
    terminal.onBinary((data) => {
      if (this.active.container === container) this.onInput(encodeTerminalBinary(data));
    });

    return {
      write: (data, callback) => terminal.write(data, callback),
      resize: (columns, rows) => terminal.resize(columns, rows),
      dispose: () => terminal.dispose(),
      focus: () => terminal.focus(),
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
