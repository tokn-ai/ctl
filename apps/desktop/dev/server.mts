import { createServer, type InlineConfig, type ViteDevServer } from "vite";
import { chooseDesktopDevPort } from "./server-options.ts";

export async function startDesktopFrontend(options: {
  app_directory: string;
  host?: string;
  initial_port?: number;
}): Promise<{ url: string; server: ViteDevServer }> {
  const server_options = {
    host: options.host || "127.0.0.1",
    port: options.initial_port ?? chooseDesktopDevPort(),
    strictPort: false,
  };
  const config: InlineConfig = {
    root: options.app_directory,
    server: server_options,
  };
  const server = await createServer(config);
  try {
    await server.listen();
    const url = server.resolvedUrls?.local[0] ?? server.resolvedUrls?.network[0];
    if (!url) throw new Error("Vite did not report a development URL");
    // Tauri keeps this URL through native rebuilds. A Vite config reload must
    // reuse it too, or fail instead of silently moving the frontend elsewhere.
    server_options.port = Number(new URL(url).port);
    server_options.strictPort = true;
    server.printUrls();
    return { url, server };
  } catch (error) {
    await server.close();
    throw error;
  }
}
