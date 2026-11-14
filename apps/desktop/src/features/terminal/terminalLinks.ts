import { translate, type Locale } from "../i18n/messages";
import { WebLinksAddon } from "@xterm/addon-web-links";
import type { IBufferRange, IDisposable, Terminal } from "@xterm/xterm";
import { installTerminalMouse } from "./terminalMouse";
import { detectFileLinks, parseFileLink } from "../files/fileLinks";
import type { FileLink } from "../../lib/types";
import { validWebUrl } from "../../lib/links";

interface HoveredLink {
  url: string;
  range: IBufferRange;
}

function containsPoint(terminal: Terminal, range: IBufferRange, event: MouseEvent): boolean {
  if (!(event.target instanceof Node) || !terminal.element?.contains(event.target)) return false;
  const screen = terminal.element?.querySelector(".xterm-screen")?.getBoundingClientRect();
  if (!screen || screen.width <= 0 || screen.height <= 0 || event.clientX < screen.left ||
    event.clientX >= screen.right || event.clientY < screen.top || event.clientY >= screen.bottom) return false;
  const x = Math.floor((event.clientX - screen.left) * terminal.cols / screen.width) + 1;
  const y = Math.floor((event.clientY - screen.top) * terminal.rows / screen.height) + terminal.buffer.active.viewportY + 1;
  return y >= range.start.y && y <= range.end.y &&
    (y !== range.start.y || x >= range.start.x) && (y !== range.end.y || x <= range.end.x);
}

/** Share activation rules between detected URLs and the terminal's OSC 8 links. */
export function installTerminalLinks(
  terminal: Terminal,
  on_open_link: (url: string) => void,
  on_copy_text: (text: string) => void = () => {},
  on_open_file?: (file: FileLink) => void,
  getLocale: () => Locale = () => "en",
): IDisposable {
  const element = terminal.element;
  if (!element) throw new Error("Terminal must be opened before installing links");
  const owner_document = element.ownerDocument;
  const owner_window = owner_document.defaultView;
  const is_mac = /Mac|iPhone|iPad|iPod/.test(owner_window?.navigator.platform ?? "");
  let hovered: HoveredLink | null = null;
  const previous_handler = terminal.options.linkHandler;

  const leave = () => {
    hovered = null;
    element.removeAttribute("title");
  };
  const hover = (_event: MouseEvent, url: string, range: IBufferRange) => {
    if (!validWebUrl(url) && !(on_open_file && parseFileLink(url))) {
      leave();
      return;
    }
    hovered = { url, range };
    element.title = translate(getLocale(), validWebUrl(url) ? "session.open_link_hint" : "session.preview_link_hint", {
      key: is_mac ? "Cmd" : "Ctrl", url,
    });
  };
  // Activation belongs to the shared click/drag gesture, never xterm's plain click.
  const activate = () => {};
  const handler = { activate, hover, leave, allowNonHttpProtocols: !!on_open_file };
  terminal.options.linkHandler = handler;
  const addon = new WebLinksAddon(activate, { hover, leave });
  terminal.loadAddon(addon);
  const files = on_open_file ? terminal.registerLinkProvider({
    provideLinks: (line, callback) => {
      const buffer = terminal.buffer.active;
      let first = line - 1;
      while (first > 0 && line - first < 32 && buffer.getLine(first)?.isWrapped) first -= 1;
      let text = "";
      const cells: { x: number; y: number; end_x: number }[] = [];
      for (let row = first; row < Math.min(buffer.length, first + 32); row += 1) {
        const current = buffer.getLine(row);
        if (!current || row !== first && !current.isWrapped) break;
        const wrapped = buffer.getLine(row + 1)?.isWrapped;
        let row_text = "";
        const row_cells: typeof cells = [];
        for (let x = 0; x < terminal.cols && text.length + row_text.length < 16_384; x += 1) {
          const cell = current.getCell(x);
          if (!cell || cell.getWidth() === 0) continue;
          const chars = cell.getChars() || " ";
          row_text += chars;
          for (let index = 0; index < chars.length; index += 1) row_cells.push({ x: x + 1, y: row + 1, end_x: x + cell.getWidth() });
        }
        if (!wrapped) row_text = row_text.trimEnd();
        text += row_text;
        cells.push(...row_cells.slice(0, row_text.length));
        if (!wrapped || text.length >= 16_384) break;
      }
      callback(detectFileLinks(text).flatMap((link) => {
        const start = cells[link.start];
        const end = cells[link.end - 1];
        if (!start || !end || line < start.y || line > end.y) return [];
        const range: IBufferRange = { start: { x: start.x, y: start.y }, end: { x: end.end_x, y: end.y } };
        return [{ text: link.text, range, activate, hover: (event: MouseEvent) => hover(event, link.text, range), leave }];
      }));
    },
  }) : null;

  const mouse = installTerminalMouse(terminal, is_mac, {
    link_at: (event) => hovered && containsPoint(terminal, hovered.range, event) ? hovered.url : null,
    on_open_link: (url) => {
      if (validWebUrl(url)) on_open_link(url);
      else { const file = parseFileLink(url); if (file) on_open_file?.(file); }
    }, on_copy_text,
  });

  return {
    dispose: () => {
      mouse.dispose();
      leave();
      addon.dispose();
      files?.dispose();
      if (terminal.options.linkHandler === handler) terminal.options.linkHandler = previous_handler;
    },
  };
}
