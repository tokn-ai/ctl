import { execFile } from "node:child_process";

export interface CtreamOptions {
  /** Path to ctream, or a program and fixed arguments. Defaults to ctream on PATH. */
  executable?: string | readonly [string, ...string[]];
  host?: string;
  agent?: string;
  ssh_program?: string;
  ssh_args?: readonly string[];
  /** Maximum duration of a CLI invocation in milliseconds. Zero disables the deadline. */
  timeout_ms?: number;
}

const JSON_LIMIT = 2 * 1024 * 1024;
const REQUEST_LIMIT = 64 * 1024;
// ctream bounds frames at 33,554,432 RGB pixels. Allow PNG overhead and base64.
const SCREENSHOT_LIMIT = 144 * 1024 * 1024;

export function cli(options: CtreamOptions) {
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

  return (args: string[], body?: {operation: string} & Record<string, unknown>): Promise<unknown> =>
    new Promise((resolve, reject) => {
      const payload = body === undefined ? undefined : JSON.stringify(body);
      if (payload !== undefined && Buffer.byteLength(payload) > REQUEST_LIMIT) {
        reject(new RangeError("ctream request exceeds 64 KiB"));
        return;
      }
      let write_error: Error | undefined;
      const child = execFile(program, [...prefix, ...args], {
        encoding: "utf8",
        windowsHide: true,
        timeout,
        maxBuffer: body?.operation === "screenshot" ? SCREENSHOT_LIMIT : JSON_LIMIT,
      }, (error, stdout, stderr) => {
        if (error) {
          reject(new Error(stderr.trim() || error.message, { cause: error }));
        } else if (write_error) {
          reject(write_error);
        } else {
          try {
            resolve(JSON.parse(stdout));
          } catch (cause) {
            reject(new Error("ctream returned invalid JSON", { cause }));
          }
        }
      });
      child.stdin!.on("error", (error: Error) => { write_error = error; });
      child.stdin!.end(payload);
    });
}
