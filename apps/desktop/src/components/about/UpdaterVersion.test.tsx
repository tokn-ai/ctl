// @vitest-environment jsdom
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { getUpdaterVersion } from "../../lib/tauri";
import { UpdaterVersion } from "./UpdaterVersion";

vi.mock("../../lib/tauri", () => ({ getUpdaterVersion: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

it("reads the selected ctlup on opening and discards a stale result after a local update", async () => {
  let complete!: (version: string) => void;
  vi.mocked(getUpdaterVersion).mockImplementationOnce(() => new Promise((resolve) => { complete = resolve; }))
    .mockResolvedValueOnce("0.1.5-rc.1+test");
  const view = render(<UpdaterVersion visible={false} refresh_revision={0} />);
  expect(getUpdaterVersion).not.toHaveBeenCalled();
  view.rerender(<UpdaterVersion visible refresh_revision={0} />);
  expect(getUpdaterVersion).toHaveBeenCalledOnce();
  view.rerender(<UpdaterVersion visible refresh_revision={1} />);
  expect(await screen.findByText("ctlup 0.1.5-rc.1+test")).toBeTruthy();
  await act(async () => complete("0.1.4"));
  expect(screen.queryByText("ctlup 0.1.4")).toBeNull();
});

it("distinguishes an absent updater from an executable that fails its version probe", async () => {
  vi.mocked(getUpdaterVersion).mockRejectedValueOnce({ code: "ctlup_unavailable", message: "Install ctlup." })
    .mockRejectedValueOnce(new Error("Unexpected version output."));
  const view = render(<UpdaterVersion visible refresh_revision={0} />);
  expect(await screen.findByText("Not installed")).toBeTruthy();
  view.rerender(<UpdaterVersion visible refresh_revision={1} />);
  expect((await screen.findByText("Version unavailable")).title).toBe("Unexpected version output.");
  expect(screen.queryByText("Not installed")).toBeNull();
});
