// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { getDesktopBuild } from "../../lib/tauri";
import { RunningDesktopBuild } from "./RunningDesktopBuild";

vi.mock("../../lib/tauri", () => ({ getDesktopBuild: vi.fn() }));
afterEach(() => { cleanup(); vi.resetAllMocks(); });

const revision = "1234567890abcdef1234567890abcdef12345678";
const fallback = { version: "0.1.0", source_revision: revision, source_fingerprint: null, dirty: null, protocols: [] };

it.each(["nightly", "local", "dev"] as const)("identifies a running %s executable before host checks finish", async (channel) => {
  vi.mocked(getDesktopBuild).mockResolvedValue({ version: "0.1.5", channel, source_revision: revision });
  render(<RunningDesktopBuild visible />);
  expect(await screen.findByText("ctmux 0.1.5")).toBeTruthy();
  expect(screen.getByText(channel)).toBeTruthy();
  expect(screen.getByText("SHA 12345678").title).toBe(revision);
});

it("shows a stable app even when the available diagnostics describe a different version", async () => {
  vi.mocked(getDesktopBuild).mockResolvedValue({ version: "0.1.5", channel: "stable", source_revision: revision });
  render(<RunningDesktopBuild visible fallback={fallback} />);
  expect(await screen.findByText("ctmux 0.1.5")).toBeTruthy();
  expect(screen.getByText("stable")).toBeTruthy();
  expect(screen.queryByText(/SHA/)).toBeNull();
});

it("reports a missing checkout revision without substituting a fingerprint or an unrelated SHA", async () => {
  vi.mocked(getDesktopBuild).mockResolvedValue({ version: "0.1.5", channel: "local", source_revision: null });
  render(<RunningDesktopBuild visible fallback={fallback} />);
  expect(await screen.findByText("SHA unavailable")).toBeTruthy();
  expect(screen.queryByText("SHA 12345678")).toBeNull();
});

it("preserves version and SHA from older native diagnostics with an unknown channel", async () => {
  vi.mocked(getDesktopBuild).mockRejectedValue(new Error("command unavailable"));
  render(<RunningDesktopBuild visible fallback={fallback} />);
  expect(await screen.findByText("Channel unknown")).toBeTruthy();
  expect(screen.getByText("ctmux 0.1.0")).toBeTruthy();
  expect(screen.getByText("SHA 12345678")).toBeTruthy();
});
