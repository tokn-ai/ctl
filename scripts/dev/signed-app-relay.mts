import { createConnection } from "node:net";
import { writeFile } from "node:fs/promises";
import { constants } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { encodeAppMessage, maxAppMessageBytes, parseAppSupervisorResponse, type AppLaunchRequest } from "./signed-app-protocol.mts";

export async function relayApp(socket_path: string, request: AppLaunchRequest, on_started: () => Promise<void>): Promise<number> {
  const socket = createConnection(socket_path);
  let contents = Buffer.alloc(0);
  let started = false;
  const timeout = setTimeout(() => socket.destroy(new Error("Signed app preparation timed out")), 120_000);
  try {
    socket.write(encodeAppMessage(request));
    for await (const chunk of socket) {
      contents = Buffer.concat([contents, chunk as Buffer]);
      if (contents.length > maxAppMessageBytes) throw new Error("Signed app supervisor response exceeds its size limit");
      let newline: number;
      while ((newline = contents.indexOf(10)) !== -1) {
        const response = parseAppSupervisorResponse(JSON.parse(contents.subarray(0, newline).toString("utf8")));
        contents = contents.subarray(newline + 1);
        if (response.type === "error") throw new Error(response.message);
        if (response.type === "started" && !started) {
          started = true;
          clearTimeout(timeout);
          await on_started();
        } else if (response.type === "exit" && started) {
          return response.code ?? (response.signal ? 128 + constants.signals[response.signal] : 1);
        } else {
          throw new Error("Unexpected signed app supervisor response");
        }
      }
    }
    throw new Error("Signed app supervisor closed before reporting app exit");
  } finally {
    clearTimeout(timeout);
    socket.destroy();
  }
}

async function main(): Promise<void> {
  const supervisor = process.env.CTMUX_DEV_APP_SUPERVISOR;
  const ctld_executable = process.env.CTMUX_DEV_CTLD_EXECUTABLE;
  const marker = process.env.CTMUX_DEV_APP_FAILURE_MARKER;
  const [executable, ...args] = process.argv.slice(2);
  if (!supervisor || !ctld_executable || !marker || !executable) throw new Error("Signed app relay is missing its launch context");
  process.exitCode = await relayApp(supervisor, {
    executable: path.resolve(executable), args, cwd: process.cwd(), env: { ...process.env }, ctld_executable,
  }, () => writeFile(marker, "started", { mode: 0o600 }));
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch(async (error: unknown) => {
    // Cargo appends its own exit diagnostic. Let the wrapper restore Tauri's
    // recoverable build-failure classification after Cargo finishes writing.
    const marker = process.env.CTMUX_DEV_APP_FAILURE_MARKER;
    if (marker) await writeFile(marker, "failed", { mode: 0o600 }).catch(() => {});
    console.error(error instanceof Error ? error.message : "Signed app relay failed");
    process.exitCode = 1;
  });
}
