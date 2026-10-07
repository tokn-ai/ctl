import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { chooseDesktopDevPort } from "./dev/server-options";

const host = process.env.TAURI_DEV_HOST;
const dynamicPort = process.env.CTL_DESKTOP_DYNAMIC_PORT === "1";

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [react()],

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // Raw `tauri dev` uses its template URL. The root launcher overrides this
  // with an already-bound port; browser previews can also select a free port.
  server: {
    port: dynamicPort ? chooseDesktopDevPort() : 1430,
    strictPort: !dynamicPort,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
}));
