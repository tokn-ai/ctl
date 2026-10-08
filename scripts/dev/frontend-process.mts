import { spawn } from "node:child_process";

export async function startFrontendProcess(options: {
  entry_path: string;
  app_directory: string;
}) {
  // Vite installs process exit handlers. Keep them in a child so the launcher
  // can finish native-app and signed-supervisor cleanup before it exits.
  const frontend = spawn(process.execPath, [
    "--experimental-strip-types", "--disable-warning=ExperimentalWarning", options.entry_path,
  ], { cwd: options.app_directory, stdio: ["inherit", "inherit", "inherit", "ipc"] });
  const exited = new Promise<{ code: number | null; signal: NodeJS.Signals | null }>((resolve) => {
    frontend.once("exit", (code, signal) => resolve({ code, signal }));
    frontend.once("error", () => resolve({ code: 1, signal: null }));
  });
  const close = async () => {
    if (frontend.exitCode === null && frontend.signalCode === null) frontend.kill("SIGTERM");
    await exited;
  };
  try {
    const url = await new Promise<string>((resolve, reject) => {
      const timeout = setTimeout(() => fail(new Error("Desktop frontend startup timed out")), 30_000);
      const cleanup = () => {
        clearTimeout(timeout);
        frontend.off("message", ready);
        frontend.off("error", fail);
      };
      const fail = (error: Error) => {
        cleanup();
        reject(error);
      };
      const ready = (message: unknown) => {
        if (!message || typeof message !== "object" || !("kind" in message) || message.kind !== "ready") return;
        try {
          if (!("url" in message) || typeof message.url !== "string") throw new Error("Missing development URL");
          const url = new URL(message.url);
          if (!["http:", "https:"].includes(url.protocol) || !Number(url.port)) throw new Error("Invalid development URL");
          cleanup();
          resolve(message.url);
        } catch (error) {
          fail(error instanceof Error ? error : new Error("Invalid frontend response"));
        }
      };
      frontend.on("message", ready);
      frontend.once("error", fail);
      void exited.then(({ code, signal }) => fail(new Error(
        `Desktop frontend exited before reporting a URL (${signal ?? `status ${code}`})`,
      )));
    });
    return { url, close, exited };
  } catch (error) {
    await close();
    throw error;
  }
}
