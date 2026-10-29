import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { ShellType, ViewLayout } from "../../lib/types";
import { SessionIcon } from "./SessionIcon";

const terminal = (terminal_id: string): ViewLayout => ({ kind: "terminal", terminal_id });

describe("SessionIcon", () => {
  it("uses distinct shell glyphs and accessible names for single terminals", () => {
    const shells: [ShellType, string][] = [
      ["bash", "Bash"], ["zsh", "Zsh"], ["fish", "Fish"], ["pwsh", "PowerShell"],
      ["cmd", "Command Prompt"], ["sh", "POSIX shell"], ["unknown", "Terminal"],
    ];
    const glyphs = shells.map(([shell_type, name]) => {
      const markup = renderToStaticMarkup(<SessionIcon shell_type={shell_type} layout={terminal("one")} />);
      expect(markup).toContain(`aria-label="${name} session"`);
      expect(markup).toContain(`<title>${name} session</title>`);
      expect(markup).toContain('stroke="currentColor"');
      return markup.slice(markup.indexOf("</title>") + "</title>".length);
    });
    expect(new Set(glyphs).size).toBe(shells.length);
  });

  it("uses the shell for a single terminal inside redundant split wrappers", () => {
    const layout: ViewLayout = {
      kind: "split", axis: "horizontal", children: [
        { kind: "split", axis: "vertical", children: [terminal("one")] },
      ],
    };
    expect(renderToStaticMarkup(<SessionIcon shell_type="fish" layout={layout} />))
      .toBe(renderToStaticMarkup(<SessionIcon shell_type="fish" />));
  });

  it("draws weighted nested splits with horizontal children side by side", () => {
    const layout: ViewLayout = {
      kind: "split", axis: "horizontal", weights: [1, 2], children: [
        terminal("left"),
        { kind: "split", axis: "vertical", weights: [3, 1], children: [terminal("top"), terminal("bottom")] },
      ],
    };
    const before = JSON.stringify(layout);
    const markup = renderToStaticMarkup(<SessionIcon shell_type="bash" layout={layout} />);
    expect(markup).toContain('aria-label="3-pane session layout"');
    expect(markup).toContain('d="M9 4v16 M9 16h12"');
    expect(JSON.stringify(layout)).toBe(before);
  });

  it.each([
    undefined, [1], [0, 2], [-1, 2], [Number.NaN, 2], [Number.POSITIVE_INFINITY, 2],
  ])("uses equal subdivisions for invalid or missing weights %j", (weights) => {
    const layout: ViewLayout = {
      kind: "split", axis: "vertical", weights, children: [terminal("top"), terminal("bottom")],
    };
    const markup = renderToStaticMarkup(<SessionIcon layout={layout} />);
    expect(markup).toContain('d="M3 12h18"');
  });

  it("normalizes large finite weights without overflowing", () => {
    const layout: ViewLayout = {
      kind: "split", axis: "horizontal", weights: [Number.MAX_VALUE, Number.MAX_VALUE],
      children: [terminal("left"), terminal("right")],
    };
    expect(renderToStaticMarkup(<SessionIcon layout={layout} />)).toContain('d="M12 4v16"');
  });

  it("keeps a very narrow pane visible at icon size", () => {
    const layout: ViewLayout = {
      kind: "split", axis: "horizontal", weights: [Number.MIN_VALUE, Number.MAX_VALUE],
      children: [terminal("narrow"), terminal("wide")],
    };
    expect(renderToStaticMarkup(<SessionIcon layout={layout} />)).toContain('d="M5.5 4v16"');
  });

  it("handles absent and empty layouts with the generic terminal", () => {
    const markup = renderToStaticMarkup(<SessionIcon size={20} class_name="session-icon" />);
    expect(markup).toContain('aria-label="Terminal session"');
    expect(markup).toContain('class="session-icon" width="20" height="20"');
    const layout: ViewLayout = { kind: "split", axis: "horizontal", children: [] };
    expect(renderToStaticMarkup(<SessionIcon layout={layout} />))
      .toBe(renderToStaticMarkup(<SessionIcon />));
  });
});
