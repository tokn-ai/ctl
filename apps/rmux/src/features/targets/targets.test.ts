import { describe, expect, it } from "vitest";
import type { ConnectionTarget, SessionSummary } from "../../lib/types";
import {
  LOCAL_TARGET,
  appLocalSshTarget,
  inactiveSshConfigDestinations,
  loadRemoteTargets,
  normalizeSshDestination,
  sameSession,
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

  it("reads unique normalized SSH destinations from legacy storage", () => {
    const storage = memoryStorage();
    storage.setItem(
      "rmux.remote_hosts",
      JSON.stringify({
        schema_version: 2,
        ssh_hosts: [
          { destination: "rmux-docker" },
          { destination: " rmux-docker " },
          { destination: "lab" },
        ],
      }),
    );

    expect(loadRemoteTargets(storage)).toEqual([
      { kind: "ssh", destination: "rmux-docker" },
      { kind: "ssh", destination: "lab" },
    ]);
  });

  it("reads app-local SSH settings from legacy schema two", () => {
    const storage = memoryStorage();
    const target = appLocalSshTarget({
      alias: "rmux-remote-test",
      hostname: "127.0.0.1",
      user: "rmux",
      port: 2222,
      identity_file: "~/.ssh/local.id_rsa",
    });
    expect(target).not.toBeNull();

    storage.setItem(
      "rmux.remote_hosts",
      JSON.stringify({
        schema_version: 2,
        ssh_hosts: [
          {
            destination: "rmux-remote-test",
            hostname: "127.0.0.1",
            user: "rmux",
            port: 2222,
            identity_file: "~/.ssh/local.id_rsa",
          },
        ],
      }),
    );
    expect(loadRemoteTargets(storage)).toEqual([target]);
  });

  it("rejects invalid app-local ports instead of falling back to SSH defaults", () => {
    expect(
      appLocalSshTarget({
        alias: "invalid",
        hostname: "127.0.0.1",
        user: null,
        port: 0,
        identity_file: null,
      }),
    ).toBeNull();
  });

  it("migrates schema-one destination lists on read", () => {
    const storage = memoryStorage();
    storage.setItem(
      "rmux.remote_hosts",
      JSON.stringify({ schema_version: 1, ssh_destinations: ["legacy"] }),
    );

    expect(loadRemoteTargets(storage)).toEqual([
      { kind: "ssh", destination: "legacy" },
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

  it("offers normalized SSH config hosts that are not already active", () => {
    expect(
      inactiveSshConfigDestinations(
        [
          { destination: " workstation " },
          { destination: "rmux-docker" },
          { destination: "workstation" },
          { destination: "host\ncommand" },
        ],
        [LOCAL_TARGET, { kind: "ssh", destination: "workstation" }],
      ),
    ).toEqual(["rmux-docker"]);
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
