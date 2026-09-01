import { describe, expect, it, vi } from "vitest";
import type { AppCommand } from "./types";
import { findEnabledNativeCommand } from "./useNativeCommandEvents";

function command(id: string, enabled: boolean): AppCommand {
  return {
    id,
    category: "Test",
    title: id,
    enabled,
    run: vi.fn(),
  };
}

describe("native command events", () => {
  it("dispatches only a known enabled command", () => {
    const commands = [command("enabled", true), command("disabled", false)];

    expect(findEnabledNativeCommand(commands, "enabled")).toBe(commands[0]);
    expect(findEnabledNativeCommand(commands, "disabled")).toBeNull();
    expect(findEnabledNativeCommand(commands, "unknown")).toBeNull();
  });
});
