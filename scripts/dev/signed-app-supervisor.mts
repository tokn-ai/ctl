import { spawn, type ChildProcess } from "node:child_process";
import { randomUUID } from "node:crypto";
import { constants } from "node:fs";
import { chmod, copyFile, lstat, rm } from "node:fs/promises";
import { createServer, type Socket } from "node:net";
import path from "node:path";
import { encodeAppMessage, maxAppMessageBytes, parseAppLaunchRequest, type AppLaunchRequest, type AppSupervisorResponse } from "./signed-app-protocol.mts";
import { ensurePrivateDirectory } from "./signed-runtime.mts";

interface SupervisorOptions {
  prepare: (ctld_executable: string) => Promise<void>;
  on_exit: (status: { code: number | null; signal: NodeJS.Signals | null }) => void;
  on_error?: (error: unknown) => void;
  shutdown_timeout_ms?: number;
}

interface Request {
  socket: Socket;
  generation: number;
  launch: AppLaunchRequest;
}

interface App {
  child: ChildProcess;
  socket: Socket;
  snapshot: string;
  intentional_stop: boolean;
  spawned: boolean;
  finished: boolean;
  completed: Promise<void>;
}

/** Own the app independently of Tauri's disposable Cargo/launch relay process. */
export async function serveAppSupervisor(
  socket_path: string,
  options: SupervisorOptions,
): Promise<{ close: () => Promise<void> }> {
  await ensurePrivateDirectory(path.dirname(socket_path));
  const sockets = new Set<Socket>();
  const apps = new Set<App>();
  let current: App | undefined;
  let generation = 0;
  let closing = false;
  let pending = Promise.resolve();
  let closed: Promise<void> | undefined;

  const viable = (request: Request): boolean => {
    if (!closing && !request.socket.destroyed && !request.socket.readableEnded && request.generation === generation) return true;
    if (!closing && request.generation !== generation) {
      reply(request.socket, { type: "error", message: "This app launch was superseded by a newer build" }, true);
    }
    return false;
  };

  const launch = async (request: Request, snapshot: string): Promise<void> => {
    const child = spawn(snapshot, request.launch.args, {
      cwd: request.launch.cwd,
      env: request.launch.env,
      // Inherit the outer launcher's terminal, never the disposable relay pipe.
      stdio: "inherit",
    });
    const app: App = {
      child, socket: request.socket, snapshot, intentional_stop: false,
      spawned: false, finished: false, completed: Promise.resolve(),
    };
    apps.add(app);
    current = app;
    app.completed = new Promise((resolve) => {
      child.once("close", (code, signal) => {
        app.finished = true;
        void (async () => {
          if (current === app) current = undefined;
          await rm(snapshot, { force: true }).catch(() => {});
          apps.delete(app);
          if (app.spawned && !app.intentional_stop && !closing) {
            reply(app.socket, { type: "exit", code, signal }, true);
            options.on_exit({ code, signal });
          }
          resolve();
        })();
      });
    });
    await new Promise<void>((resolve, reject) => {
      child.once("error", reject);
      child.once("spawn", () => { app.spawned = true; resolve(); });
    });
    reply(request.socket, { type: "started" });
  };

  const replace = async (request: Request): Promise<void> => {
    if (!viable(request)) return;
    let snapshot: string | undefined;
    let phase = "snapshot";
    try {
      snapshot = await snapshotExecutable(request.launch.executable);
      if (!viable(request)) return;
      phase = "prepare";
      await options.prepare(request.launch.ctld_executable);
      if (!viable(request)) return;
      // This is the replacement commit point. Losing the relay after this point
      // must not leave the app stopped halfway through a successful replacement.
      phase = "launch";
      if (current) await stopApp(current, options.shutdown_timeout_ms ?? 2_000);
      if (closing) return;
      await launch(request, snapshot);
      snapshot = undefined; // The launched child owns cleanup after its exit.
    } catch (error) {
      // The preparation callback owns its diagnostics; never pass launch
      // arguments or the request environment to logging callbacks.
      if (phase === "prepare") options.on_error?.(error);
      const message = phase === "launch"
        ? "The successfully built app could not be started"
        : phase === "prepare"
          ? "Signed daemon preparation failed; the current app was preserved"
          : "The built app could not be prepared; the current app was preserved";
      reply(request.socket, { type: "error", message }, true);
    } finally {
      if (snapshot) await rm(snapshot, { force: true });
    }
  };

  const server = createServer((socket) => {
    sockets.add(socket);
    socket.once("close", () => sockets.delete(socket));
    socket.on("error", () => {});
    void readRequest(socket).then((request) => {
      if (closing || socket.destroyed) return;
      const accepted: Request = { socket, launch: request, generation: ++generation };
      pending = pending.then(() => replace(accepted));
      // A client receives only static failure descriptions, never environment
      // contents or an exception that might include command arguments.
      pending = pending.catch(() => reply(socket, { type: "error", message: "Signed app replacement failed" }, true));
    }, () => reply(socket, { type: "error", message: "Invalid signed app launch request" }, true));
  });
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(socket_path, resolve);
  });
  try {
    await chmod(socket_path, 0o600);
  } catch (error) {
    closing = true;
    for (const socket of sockets) socket.destroy();
    await new Promise<void>((resolve) => server.close(() => resolve()));
    throw error;
  }

  return {
    close: () => {
      closed ??= (async () => {
        closing = true;
        generation += 1;
        for (const socket of sockets) socket.destroy();
        const stopped = current ? stopApp(current, options.shutdown_timeout_ms ?? 2_000) : Promise.resolve();
        const serverClosed = new Promise<void>((resolve, reject) => {
          server.close((error) => error ? reject(error) : resolve());
        });
        await Promise.all([pending, stopped, serverClosed]);
        await Promise.all([...apps].map((app) => app.completed));
      })();
      return closed;
    },
  };
}

async function snapshotExecutable(executable: string): Promise<string> {
  const metadata = await lstat(executable);
  if (!metadata.isFile() || (process.getuid && metadata.uid !== process.getuid()) || (metadata.mode & 0o100) === 0) {
    throw new Error("Built app is not an owned executable file");
  }
  const snapshot = path.join(path.dirname(executable), `.ctmux-app-dev-${randomUUID()}${path.extname(executable)}`);
  try {
    await copyFile(executable, snapshot, constants.COPYFILE_EXCL | constants.COPYFILE_FICLONE);
    await chmod(snapshot, 0o700);
    return snapshot;
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "EEXIST") await rm(snapshot, { force: true });
    throw error;
  }
}

function reply(socket: Socket, message: AppSupervisorResponse, end = false): void {
  if (socket.destroyed || socket.writableEnded) return;
  const contents = encodeAppMessage(message);
  if (end) socket.end(contents); else socket.write(contents);
}

async function stopApp(app: App, timeout_ms: number): Promise<void> {
  app.intentional_stop = true;
  for (const signal of ["SIGTERM", "SIGKILL"] as const) {
    if (app.finished || app.child.exitCode !== null || app.child.signalCode !== null) break;
    app.child.kill(signal);
    let timer: ReturnType<typeof setTimeout> | undefined;
    await Promise.race([app.completed, new Promise<void>((resolve) => { timer = setTimeout(resolve, timeout_ms); })]);
    clearTimeout(timer);
  }
  if (!app.finished && app.child.exitCode === null && app.child.signalCode === null) throw new Error("Owned app did not stop");
  await app.completed;
}

function readRequest(socket: Socket): Promise<AppLaunchRequest> {
  return new Promise((resolve, reject) => {
    let received = Buffer.alloc(0);
    let finished = false;
    const timer = setTimeout(() => finish(new Error("App request timed out")), 10_000);
    const finish = (error?: Error, value?: AppLaunchRequest): void => {
      if (finished) return;
      finished = true;
      clearTimeout(timer);
      socket.off("data", data);
      socket.off("error", failed);
      socket.off("end", ended);
      socket.off("close", ended);
      if (error) reject(error); else resolve(value!);
    };
    const failed = () => finish(new Error("App request failed"));
    const ended = () => finish(new Error("App request ended"));
    const data = (chunk: Buffer): void => {
      if (received.length + chunk.length > maxAppMessageBytes) {
        finish(new Error("App request too large"));
        return;
      }
      received = Buffer.concat([received, chunk]);
      const newline = received.indexOf(10);
      if (newline < 0) return;
      try {
        if (newline !== received.length - 1) throw new Error("Trailing request data");
        finish(undefined, parseAppLaunchRequest(JSON.parse(received.subarray(0, newline).toString("utf8"))));
      } catch {
        finish(new Error("Invalid request"));
      }
    };
    socket.on("data", data);
    socket.once("error", failed);
    socket.once("end", ended);
    socket.once("close", ended);
  });
}
