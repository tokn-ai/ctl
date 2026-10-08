type TauriDevConfig = {
  build: { devUrl: string };
  app: { security: { devCsp: string } };
};

export function desktopDevArguments(
  args: string[],
  options: { url: string; config: TauriDevConfig; platform: NodeJS.Platform },
): string[] {
  const previous = new URL(options.config.build.devUrl);
  const next = new URL(options.url);
  const websocketOrigin = (url: URL) => `${url.protocol === "https:" ? "wss:" : "ws:"}//${url.host}`;
  const origins = new Map([
    [previous.origin, next.origin],
    [websocketOrigin(previous), websocketOrigin(next)],
  ]);
  const dev_csp = options.config.app.security.devCsp.replace(
    /[^\s;]+/g, (source) => origins.get(source) ?? source,
  );
  const override = JSON.stringify({
    build: {
      devUrl: options.url,
      // Vite is already listening. Keep the native preflight, without starting
      // another frontend. macOS/Linux build daemons through the Cargo runner.
      beforeDevCommand: {
        script: options.platform === "win32"
          ? "pnpm --workspace-root daemons:build && pnpm --workspace-root bundles:check"
          : "pnpm --workspace-root bundles:check",
        // The frontend is already ready, so waiting for the hook explicitly
        // prevents native startup from racing the Windows daemon build.
        wait: true,
      },
    },
    app: { security: { devCsp: dev_csp } },
  });
  const separator = args.indexOf("--");
  const split = separator < 0 ? args.length : separator;
  return [...args.slice(0, split), "--config", override, ...args.slice(split)];
}
