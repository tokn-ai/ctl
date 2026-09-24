// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { KeybindingsFlow } from "./KeybindingsFlow";
import type { KeybindingsDocument } from "../../lib/types";
afterEach(cleanup);
const document: KeybindingsDocument = { schema_version: 1, overrides: [{ command_id: "session.close", keybinding: null }] };
function setup(settings = document) {
  const save = vi.fn().mockResolvedValue(undefined);
  render(<KeybindingsFlow commands={[]} document={settings} path="/config/keybindings.json" error={null} platform="other" onSave={save} onClose={vi.fn()} />);
  return save;
}
it("saves a literal configurable prefix without changing app shortcuts", async () => {
  const save = setup();
  fireEvent.click(screen.getByRole("option", { name: /Terminal prefix/ }));
  fireEvent.change(screen.getByLabelText("Shortcut"), { target: { value: "Ctrl+A" } });
  fireEvent.click(screen.getByRole("button", { name: "Save shortcut" }));
  await waitFor(() => expect(save).toHaveBeenCalledExactlyOnceWith({ ...document, prefix: { key: "Ctrl+A", bindings: [] } }));
});
it("reports action conflicts without writing settings", async () => {
  const save = setup();
  fireEvent.click(screen.getByRole("option", { name: /Prefix · Zoom pane/ }));
  fireEvent.change(screen.getByLabelText("Shortcut"), { target: { value: "V" } });
  fireEvent.click(screen.getByRole("button", { name: "Save shortcut" }));
  expect((await screen.findByRole("alert")).textContent).toContain("already assigned");
  expect(save).not.toHaveBeenCalled();
});
it("applies the tmux-style preset without removing existing app shortcuts", async () => {
  const save = setup();
  fireEvent.click(screen.getByRole("option", { name: /Use tmux-style prefix preset/ }));
  await waitFor(() => expect(save).toHaveBeenCalledExactlyOnceWith({ ...document, prefix: { key: "Ctrl+B", bindings: [{ command_id: "pane.split_right", key: "%" }, { command_id: "pane.split_below", key: '"' }] } }));
});
