import { copyFile, mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const root = import.meta.dir;
const repository = fileURLToPath(new URL("../../", import.meta.url));
const output = join(root, "dist");
const media = process.env.CTL_WEBSITE_MEDIA_DIR ??
  join(repository, ".demo-recordings/persistence/fitted");
const gallery_root = process.env.CTL_WEBSITE_GALLERY_DIR ??
  join(repository, ".demo-recordings/website");
const media_files = {
  "demo.mp4": media,
  "poster.png": media,
  "remote.png": gallery_root,
  "vpn.png": gallery_root,
  "vpn-preview.mp4": gallery_root,
};
type MediaName = keyof typeof media_files;
const media_names = Object.keys(media_files) as MediaName[];

const mode = Bun.argv[2] ?? "serve";
if (mode !== "build" && mode !== "serve") throw new Error("Usage: bun apps/website/site.ts [serve|build]");
const media_host = process.env.CTL_WEBSITE_MEDIA_HOST?.trim() || undefined;
if (media_host && !/^(?:[a-z0-9](?:[a-z0-9-]*[a-z0-9])?\.)+[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$/i.test(media_host)) {
  throw new Error("CTL_WEBSITE_MEDIA_HOST must be a hostname without a scheme or path.");
}

const files = new Map([
  ["/site.css", Bun.file(join(root, "site.css"))],
  ["/app-icon.svg", Bun.file(join(repository, "apps/desktop/app-icon.svg"))],
]);
if (!media_host) {
  for (const name of media_names) {
    const file = Bun.file(join(media_files[name], name));
    if (await file.exists()) files.set("/" + name, file);
  }
}
const hasMedia = (name: MediaName) => Boolean(media_host) || files.has("/" + name);
const mediaUrl = (name: MediaName) => media_host ? `https://${media_host}/${name}` : `./${name}`;
const posterAttribute = (name: MediaName) => hasMedia(name) ? ` poster="${mediaUrl(name)}"` : "";

if (Bun.argv.includes("--verify-media")) {
  if (!media_host) throw new Error("Set CTL_WEBSITE_MEDIA_HOST to verify published media.");
  await Promise.all(media_names.map(async (name) => {
    const is_video = name.endsWith(".mp4");
    const url = mediaUrl(name);
    const response = await fetch(url, {
      method: is_video ? "GET" : "HEAD",
      headers: is_video ? { Range: "bytes=0-31" } : undefined,
      signal: AbortSignal.timeout(15_000),
    });
    await response.body?.cancel();
    const content_type = response.headers.get("content-type")?.split(";")[0];
    const valid_status = response.status === 200 || (is_video && response.status === 206 &&
      response.headers.get("content-range")?.startsWith("bytes 0-31/"));
    if (!valid_status || content_type !== (is_video ? "video/mp4" : "image/png")) {
      throw new Error(`Unexpected media response from ${url} (HTTP ${response.status}, ${content_type ?? "missing Content-Type"}).`);
    }
  }));
}

const media_markup = hasMedia("demo.mp4")
  ? `<video controls playsinline preload="metadata" aria-labelledby="demo-title" aria-describedby="demo-description"${posterAttribute("poster.png")}>
            <source src="${mediaUrl("demo.mp4")}" type="video/mp4">
            <a href="${mediaUrl("demo.mp4")}">Download the persistence demo</a>
          </video>`
  : '<div class="demo-placeholder"><span class="mono">ctmux</span><p>The persistence demo video is not included in this build.</p></div>';
const gallery = [
  hasMedia("remote.png") && `<figure>
    <a href="${mediaUrl("remote.png")}"><img width="1280" height="800" loading="lazy" src="${mediaUrl("remote.png")}" alt="Sample ctmux workspace with local and development server sessions"></a>
    <figcaption><strong>Local and remote sessions</strong><span>Sample workspace</span></figcaption>
  </figure>`,
  hasMedia("vpn.png") && `<figure>
    <a href="${mediaUrl("vpn.png")}"><img width="1280" height="800" loading="lazy" src="${mediaUrl("vpn.png")}" alt="VPN panel showing a connected sample OpenConnect profile"></a>
    <figcaption><strong>Saved VPN profiles</strong><span>Sample workspace</span></figcaption>
  </figure>`,
  hasMedia("vpn-preview.mp4") && `<figure class="gallery-video">
    <video controls playsinline preload="none" aria-label="Simulated VPN connection walkthrough"${posterAttribute("vpn.png")}>
      <source src="${mediaUrl("vpn-preview.mp4")}" type="video/mp4">
      <a href="${mediaUrl("vpn-preview.mp4")}">Download the VPN walkthrough</a>
    </video>
    <figcaption><strong>Connecting a saved VPN</strong><span>Illustrated with simulated states</span></figcaption>
  </figure>`,
].filter(Boolean);
const template = await readFile(join(root, "index.html"), "utf8");
const page = template.replace("<!-- demo:media -->", media_markup).replace("<!-- demo:gallery -->",
  gallery.length ? `<section class="gallery" aria-labelledby="gallery-title">
    <h2 id="gallery-title">Inside the workspace</h2>
    <div class="gallery-grid">${gallery.join("\n")}</div>
  </section>` : "");

if (mode === "build") {
  await mkdir(output, { recursive: true });
  for (const name of media_names) {
    if (!files.has("/" + name)) await Bun.file(join(output, name)).delete().catch((error) => {
      if (error.code !== "ENOENT") throw error;
    });
  }
  await Promise.all([
    writeFile(join(output, "index.html"), page),
    ...Array.from(files, ([name, file]) => copyFile(file.name!, join(output, name.slice(1)))),
  ]);
  console.log(`Built local site at ${output}`);
} else {
  const server = Bun.serve({
    hostname: "127.0.0.1",
    port: process.env.PORT === undefined ? 4175 : Number(process.env.PORT),
    routes: {
      "/": () => new Response(page, { headers: { "content-type": "text/html; charset=utf-8" } }),
      ...Object.fromEntries(Array.from(files, ([path, file]) => [path, () => new Response(file)])),
    },
    fetch: () => new Response("Not found", { status: 404 }),
  });
  console.log(`ctl landing page at ${server.url}`);
}
