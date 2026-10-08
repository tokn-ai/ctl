import { spawn } from "node:child_process";
import { constants } from "node:os";
import { fileURLToPath } from "node:url";

// Keep Vite's CLI and argument handling; only its initial port policy differs.
const app_directory = fileURLToPath(new URL("../../apps/desktop/", import.meta.url));
const vite = spawn(process.execPath, [
  fileURLToPath(new URL("../../apps/desktop/node_modules/vite/bin/vite.js", import.meta.url)),
  "--host", "127.0.0.1", ...process.argv.slice(2),
], {
  cwd: app_directory,
  env: { ...process.env, CTL_DESKTOP_DYNAMIC_PORT: "1" },
  stdio: "inherit",
});
const interrupt = () => vite.kill("SIGINT");
const terminate = () => vite.kill("SIGTERM");
process.on("SIGINT", interrupt);
process.on("SIGTERM", terminate);
vite.once("error", (error) => {
  console.error(error.message);
  process.exitCode = 1;
});
vite.once("exit", (code, signal) => {
  process.off("SIGINT", interrupt);
  process.off("SIGTERM", terminate);
  process.exitCode = code ?? (signal ? 128 + constants.signals[signal] : 1);
});
