import { execFile, spawn } from "node:child_process";
import { randomBytes, randomInt } from "node:crypto";
import agentWords from "./agent-words.json" with { type: "json" };
import { SessionTransport } from "./session-transport.js";

export interface CtreamOptions {
  /** Path to ctream, or a program and fixed arguments. Defaults to ctream on PATH. */
  executable?: string | readonly [string, ...string[]];
  host?: string;
  /** Path to the display host's ctream-agent executable. */
  agent?: string;
  /** Actor identifier recorded by the display host. Defaults to a generated word#hhhh ID. */
  agent_id?: string;
  ssh_program?: string;
  ssh_args?: readonly string[];
  /** Deadline in milliseconds for a discovery invocation or a dispatched session request. */
  timeout_ms?: number;
}

function selectAgentId(id: string | undefined): string {
  if (id === undefined) {
    return `${agentWords[randomInt(agentWords.length)]}#${randomBytes(2).toString("hex")}`;
  }
  if (typeof id !== "string" || !/^(?:[A-Za-z0-9][A-Za-z0-9._-]{0,63}|[a-z]{1,59}#[0-9a-f]{4})$/.test(id)) {
    throw new TypeError("agent_id must be a 1–64 character label or a lowercase word followed by # and four lowercase hex digits");
  }
  return id;
}

export function cli(options: CtreamOptions) {
  const agentId = selectAgentId(options.agent_id);
  const protocol = agentId.includes("#") ? "1.0.4" : "1.0.3";
  const executable = options.executable ?? "ctream";
  const [program, ...prefix] = typeof executable === "string" ? [executable] : executable;
  if (options.host !== undefined) prefix.push("--host", options.host);
  if (options.agent !== undefined) prefix.push("--agent", options.agent);
  if (options.ssh_program !== undefined) prefix.push("--ssh-program", options.ssh_program);
  for (const arg of options.ssh_args ?? []) prefix.push("--ssh-arg", arg);
  const timeout = options.timeout_ms ?? 0;
  if (!Number.isSafeInteger(timeout) || timeout < 0) {
    throw new RangeError("timeout_ms must be a nonnegative integer");
  }

  return {
    agent_id: agentId,
    query(args: string[]): Promise<unknown> {
      return new Promise((resolve, reject) => {
        const child = execFile(program, [...prefix, ...args], {
          encoding: "utf8", windowsHide: true, timeout, maxBuffer: 2 * 1024 * 1024,
        }, (error, stdout, stderr) => {
          if (error) {
            reject(new Error(stderr.trim() || error.message, { cause: error }));
            return;
          }
          try {
            resolve(JSON.parse(stdout));
          } catch (cause) {
            reject(new Error("ctream returned invalid JSON", { cause }));
          }
        });
        child.stdin!.on("error", () => {}).end();
      });
    },

    session(target: string, window_id?: string): SessionTransport {
      const args = ["session", target, "--protocol", protocol, "--agent-id", agentId];
      if (window_id !== undefined) args.push("--window", window_id);
      return new SessionTransport(
        () => spawn(program, [...prefix, ...args], {stdio: "pipe", windowsHide: true}),
        timeout,
      );
    },
  };
}
