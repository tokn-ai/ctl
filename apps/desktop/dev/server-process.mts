import { fileURLToPath } from "node:url";
import { startDesktopFrontend } from "./server.mts";

async function main(): Promise<void> {
  if (!process.send) throw new Error("The desktop frontend requires its launcher IPC channel");
  process.on("SIGINT", () => {
    // The foreground launcher owns Ctrl+C and closes this child after Tauri.
  });
  const frontend = await startDesktopFrontend({
    app_directory: fileURLToPath(new URL("../", import.meta.url)),
    host: process.env.TAURI_DEV_HOST,
  });
  const close = async () => {
    await frontend.server.close();
    process.exit();
  };
  // Losing the launcher must not leave its frontend listener behind.
  process.once("disconnect", () => void close());
  if (!process.connected) {
    await close();
    return;
  }
  process.send({ kind: "ready", url: frontend.url }, (error) => {
    if (error) {
      console.error(error.message);
      process.exitCode = 1;
      void close();
    }
  });
}

main().catch((error: unknown) => {
  console.error(error instanceof Error ? error.message : error);
  process.exitCode = 1;
});
