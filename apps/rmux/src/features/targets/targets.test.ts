import { describe, expect, it } from "vitest";
import type { ConnectionTarget, SessionSummary } from "../../lib/types";
import {
  LOCAL_TARGET,
  loadRemoteTargets,
  normalizeSshDestination,
  sameSession,
  saveRemoteTargets,
  sessionKey,
  targetKey,
} from "./targets";

function session(target: ConnectionTarget, sessionId: string): SessionSummary {
  return {
    target,
    session_id: sessionId,
    name: sessionId,
    status: "running",
    terminal_size: {
      columns: 80,
      rows: 24,
      pixel_width: null,
      pixel_height: null,
    },
    next_sequence: "0",
  };
}

describe("connection targets", () => {
  it("gives equal daemon session ids distinct composite identities", () => {
    const local = session(LOCAL_TARGET, "same-id");
    const remote = session(
      { kind: "ssh", destination: "rmux-docker" },
      "same-id",
    );

    expect(sessionKey(local)).not.toBe(sessionKey(remote));
    expect(sameSession(local, remote)).toBe(false);
    expect(targetKey(local.target)).toBe("local");
  });

  it("normalizes aliases and rejects empty or control-bearing destinations", () => {
    expect(normalizeSshDestination("  rmux-docker ")).toBe("rmux-docker");
    expect(normalizeSshDestination("  ")).toBeNull();
    expect(normalizeSshDestination("host\ncommand")).toBeNull();
  });

  it("persists only unique normalized SSH destinations", () => {
    const storage = memoryStorage();
    saveRemoteTargets(storage, [
      LOCAL_TARGET,
      { kind: "ssh", destination: "rmux-docker" },
      { kind: "ssh", destination: " rmux-docker " },
      { kind: "ssh", destination: "lab" },
    ]);

    expect(loadRemoteTargets(storage)).toEqual([
      { kind: "ssh", destination: "rmux-docker" },
      { kind: "ssh", destination: "lab" },
    ]);
  });

  it("ignores malformed or unknown persisted schemas", () => {
    const storage = memoryStorage();
    storage.setItem("rmux.remote_hosts", "not-json");
    expect(loadRemoteTargets(storage)).toEqual([]);

    storage.setItem(
      "rmux.remote_hosts",
      JSON.stringify({ schema_version: 2, ssh_destinations: ["host"] }),
    );
    expect(loadRemoteTargets(storage)).toEqual([]);
  });
});

function memoryStorage(): Storage {
  const values = new Map<string, string>();
  return {
    get length() {
      return values.size;
    },
    clear() {
      values.clear();
    },
    getItem(key) {
      return values.get(key) ?? null;
    },
    key(index) {
      return [...values.keys()][index] ?? null;
    },
    removeItem(key) {
      values.delete(key);
    },
    setItem(key, value) {
      values.set(key, value);
    },
  };
}
