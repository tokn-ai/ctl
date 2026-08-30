import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { encodeTerminalBinary, encodeTerminalText } from "../../lib/bytes";
import type { TerminalSize } from "../../lib/types";
import {
  TerminalPresenter,
  type ProposedDimensions,
  type TerminalAdapter,
} from "./TerminalPresenter";

const DEFAULT_SCROLLBACK_LINES = 10_000;

export class XtermRenderer {
  private readonly presenter: TerminalPresenter;

  constructor(
    private readonly container: HTMLElement,
    private readonly onInput: (data: Uint8Array) => void,
    initialSize: TerminalSize,
  ) {
    this.presenter = new TerminalPresenter(
      (terminalSize) => this.createAdapter(terminalSize),
      initialSize,
    );
  }

  write(data: Uint8Array): Promise<void> {
    return this.presenter.write(data);
  }

  restoreCheckpoint(
    terminalSize: TerminalSize,
    payload: Uint8Array,
    inputPrefix: Uint8Array,
  ): Promise<void> {
    return this.presenter.restoreCheckpoint(terminalSize, payload, inputPrefix);
  }

  recreate(terminalSize: TerminalSize): Promise<void> {
    return this.presenter.recreate(terminalSize);
  }

  resize(terminalSize: TerminalSize): Promise<void> {
    return this.presenter.resize(terminalSize);
  }

  proposeDimensions(): ProposedDimensions | null {
    return this.presenter.proposeDimensions();
  }

  focus(): void {
    this.presenter.focus();
  }

  dispose(): void {
    this.presenter.dispose();
  }

  private createAdapter(terminalSize: TerminalSize): TerminalAdapter {
    this.container.replaceChildren();
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
    terminal.open(this.container);
    terminal.onData((data) => this.onInput(encodeTerminalText(data)));
    terminal.onBinary((data) => this.onInput(encodeTerminalBinary(data)));

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
