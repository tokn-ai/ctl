import { constants } from "node:os";
import path from "node:path";

export const maxAppMessageBytes = 1024 * 1024;

export interface AppLaunchRequest {
  executable: string;
  args: string[];
  cwd: string;
  env: Record<string, string | undefined>;
  ctld_executable: string;
}

export type AppSupervisorResponse =
  | { type: "started" }
  | { type: "exit"; code: number | null; signal: NodeJS.Signals | null }
  | { type: "error"; message: string };

/** Only local launch metadata crosses this socket; never log or persist it. */
export function parseAppLaunchRequest(value: unknown): AppLaunchRequest {
  if (!record(value) || !absolutePath(value.executable) || !absolutePath(value.cwd) ||
    !absolutePath(value.ctld_executable) || !Array.isArray(value.args) ||
    value.args.length > 8192 || !value.args.every(text) || !record(value.env) ||
    Object.keys(value.env).length > 4096 || !Object.entries(value.env).every(([name, contents]) =>
      name.length > 0 && name.length <= 1024 && !name.includes("=") && text(name) &&
      (contents === undefined || text(contents)))) {
    throw new Error("Invalid signed app launch request");
  }
  return value as unknown as AppLaunchRequest;
}

export function parseAppSupervisorResponse(value: unknown): AppSupervisorResponse {
  if (record(value)) {
    if (value.type === "started") return { type: "started" };
    if (value.type === "error" && text(value.message)) return { type: "error", message: value.message };
    if (value.type === "exit" && (value.code === null ||
      (typeof value.code === "number" && Number.isInteger(value.code) && value.code >= 0 && value.code <= 0xffff_ffff)) &&
      (value.signal === null || (typeof value.signal === "string" && Object.hasOwn(constants.signals, value.signal)))) {
      return { type: "exit", code: value.code, signal: value.signal as NodeJS.Signals | null };
    }
  }
  throw new Error("Invalid signed app supervisor response");
}

export function encodeAppMessage(message: AppLaunchRequest | AppSupervisorResponse): string {
  const encoded = `${JSON.stringify(message)}\n`;
  if (Buffer.byteLength(encoded) > maxAppMessageBytes) throw new Error("Signed app message exceeds its size limit");
  return encoded;
}

function text(value: unknown): value is string {
  return typeof value === "string" && !value.includes("\0");
}

function absolutePath(value: unknown): value is string {
  return text(value) && path.isAbsolute(value);
}

function record(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
