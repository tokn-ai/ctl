import { WebLinksAddon } from "@xterm/addon-web-links";
import type { IBufferRange, IDisposable, Terminal } from "@xterm/xterm";
import { installTerminalMouse } from "./terminalMouse";

interface HoveredLink {
  url: string;
  range: IBufferRange;
}

function validWebUrl(value: string): boolean {
  if (value.trim() !== value || /[\u0000-\u001f\u007f-\u009f]/.test(value)) return false;
  const authority = /^https?:\/\/([^/?#]*)/i.exec(value)?.[1];
  if (!authority || authority.includes("\\")) return false;
  try {
    const url = new URL(value);
    return (url.protocol === "http:" || url.protocol === "https:") && !!url.hostname;
  } catch {
    return false;
  }
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
    if (!validWebUrl(url)) {
      leave();
      return;
    }
    hovered = { url, range };
    element.title = `${is_mac ? "Cmd" : "Ctrl"}-click to open\n${url}`;
  };
  // Activation belongs to the shared click/drag gesture, never xterm's plain click.
  const activate = () => {};
  const handler = { activate, hover, leave, allowNonHttpProtocols: false };
  terminal.options.linkHandler = handler;
  const addon = new WebLinksAddon(activate, { hover, leave });
  terminal.loadAddon(addon);

  const mouse = installTerminalMouse(terminal, is_mac, {
    link_at: (event) => hovered && containsPoint(terminal, hovered.range, event) ? hovered.url : null,
    on_open_link, on_copy_text,
  });

  return {
    dispose: () => {
      mouse.dispose();
      leave();
      addon.dispose();
      if (terminal.options.linkHandler === handler) terminal.options.linkHandler = previous_handler;
    },
  };
}
