import { describe, expect, it } from "vitest";
import {
  firstEnabledIndex,
  nextEnabledIndex,
  searchCommands,
} from "./commandSearch";
import type { AppCommand } from "./types";

function command(
  id: string,
  title: string,
  overrides: Partial<AppCommand> = {},
): AppCommand {
  return {
    id,
    category: "Session",
    title,
    enabled: true,
    run() {},
    ...overrides,
  };
}

describe("command search", () => {
  it("filters hidden commands and preserves registry order for an empty query", () => {
    const visible = command("visible", "Visible");
    const hidden = command("hidden", "Hidden", { visibleInPalette: false });

    expect(searchCommands([visible, hidden], "")).toEqual([visible]);
  });

  it("matches titles, categories, keywords, and subsequences", () => {
    const commands = [
      command("new", "New Shell", { keywords: ["create"] }),
      command("refresh", "Refresh Sessions"),
      command("input", "Request Input", { category: "Terminal" }),
    ];

    expect(searchCommands(commands, "new").map(({ id }) => id)).toEqual(["new"]);
    expect(searchCommands(commands, "terminal input").map(({ id }) => id)).toEqual([
      "input",
    ]);
    expect(searchCommands(commands, "create").map(({ id }) => id)).toEqual(["new"]);
    expect(searchCommands(commands, "rfss").map(({ id }) => id)).toEqual([
      "refresh",
    ]);
  });

  it("ranks an exact title before broader matches", () => {
    const exact = command("exact", "New Shell");
    const broader = command("broader", "Open New Shell Form");

    expect(searchCommands([broader, exact], "new shell")[0]).toBe(exact);
  });

  it("navigates enabled commands circularly while skipping disabled entries", () => {
    const commands = [
      command("first", "First", { enabled: false }),
      command("second", "Second"),
      command("third", "Third", { enabled: false }),
      command("fourth", "Fourth"),
    ];

    expect(firstEnabledIndex(commands)).toBe(1);
    expect(nextEnabledIndex(commands, 1, 1)).toBe(3);
    expect(nextEnabledIndex(commands, 3, 1)).toBe(1);
    expect(nextEnabledIndex(commands, 1, -1)).toBe(3);
    expect(nextEnabledIndex(commands, -1, -1)).toBe(3);
  });
});
