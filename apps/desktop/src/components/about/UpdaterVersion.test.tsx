// @vitest-environment jsdom
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { getUpdaterBuild } from "../../lib/tauri";
import type { UpdaterBuildInfo } from "../../lib/types";
import { UpdaterVersion } from "./UpdaterVersion";

vi.mock("../../lib/tauri", () => ({ getUpdaterBuild: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

it("reads the selected ctlup on opening and discards a stale result after a local update", async () => {
  let complete!: (build: UpdaterBuildInfo) => void;
  const revision = "abcdef12".repeat(5);
  vi.mocked(getUpdaterBuild).mockImplementationOnce(() => new Promise((resolve) => { complete = resolve; }))
    .mockResolvedValueOnce({ version: "0.1.5-rc.1+test", channel: "nightly", source_revision: revision });
  const view = render(<UpdaterVersion visible={false} refresh_revision={0} />);
  expect(getUpdaterBuild).not.toHaveBeenCalled();
  view.rerender(<UpdaterVersion visible refresh_revision={0} />);
  expect(getUpdaterBuild).toHaveBeenCalledOnce();
  view.rerender(<UpdaterVersion visible refresh_revision={1} />);
  expect(await screen.findByText("ctlup 0.1.5-rc.1+test")).toBeTruthy();
  expect(screen.getByText("nightly")).toBeTruthy();
  expect(screen.getByText("SHA abcdef12").title).toBe(revision);
  await act(async () => complete({ version: "0.1.4", channel: "stable", source_revision: null }));
  expect(screen.queryByText("ctlup 0.1.4")).toBeNull();
});

it("still shows the version of an updater without source metadata", async () => {
  vi.mocked(getUpdaterBuild).mockResolvedValueOnce({ version: "0.1.4", channel: null, source_revision: null });
  render(<UpdaterVersion visible refresh_revision={0} />);
  expect(await screen.findByText("ctlup 0.1.4")).toBeTruthy();
  expect(screen.getByText("Channel unknown")).toBeTruthy();
});

it("distinguishes an absent updater from an executable that fails its version probe", async () => {
  vi.mocked(getUpdaterBuild).mockRejectedValueOnce({ code: "ctlup_unavailable", message: "Install ctlup." })
    .mockRejectedValueOnce(new Error("Unexpected version output."));
  const view = render(<UpdaterVersion visible refresh_revision={0} />);
  expect(await screen.findByText("Not installed")).toBeTruthy();
  view.rerender(<UpdaterVersion visible refresh_revision={1} />);
  expect((await screen.findByText("Version unavailable")).title).toBe("Unexpected version output.");
  expect(screen.queryByText("Not installed")).toBeNull();
});
