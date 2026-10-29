import { describe, expect, it } from "vitest";
import { CONFIGURABLE_COMMAND_IDS } from "./commandIds";
import command_ids from "./commandIds.contract.json";

describe("frontend and native command contract", () => {
  it("keeps the native validation fixture in sync with every configurable command", () => {
    // Rust validates this same fixture: rejecting the hyphen in ctl-taskd.restart
    // previously prevented the entire native shortcut menu from being installed.
    expect(CONFIGURABLE_COMMAND_IDS).toEqual(command_ids);
  });
});
