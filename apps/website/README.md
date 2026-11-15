# ctl landing page

From the repository root, run `bun apps/website/site.ts serve` and open
`http://127.0.0.1:4175/`. Set `PORT` to use a different loopback port.

Run `bun apps/website/site.ts build` to produce a static site in
`apps/website/dist/`. The preview server and static build use the same markup
and assets.

Both commands use the fitted persistence take from
`.demo-recordings/persistence/fitted/` when present. Set
`CTL_WEBSITE_MEDIA_DIR` to use another directory containing `demo.mp4` and an
optional `poster.png`. A checkout without a recording builds with a placeholder.
The video uses native controls and starts only on request. Both recordings
and generated output are ignored by Git.

The sample workspace screenshot is `remote.png` in
`.demo-recordings/website/`. Set `CTL_WEBSITE_GALLERY_DIR` to change its
location. The released recordings and their posters are in
`.demo-recordings/website-release-v1/`. Set `CTL_WEBSITE_RELEASE_DIR` to
point at another local copy. The preview omits missing recordings.

For GitHub Pages, set `CTL_WEBSITE_MEDIA_HOST` under the repository's
`github-pages` environment variables. Use the public Cloudflare hostname
without a scheme or path. Keep `demo.mp4`, `poster.png`, and `remote.png`
at the host's root. Add the released `vpn-v1`, `reconnect-v1`, and
`cli-v1` files there before merging the page update. Each release includes
an MP4 (`video/mp4`) and a PNG poster (`image/png`). Leave previous media
available while published pages can still link to them.

The bundle workflow builds the landing page using
`bun apps/website/site.ts build --verify-media`. The hosted build links to
the released media over HTTPS and checks their responses before publishing.
It publishes the HTML, stylesheet, and icon together with the runtime bundle
registry in one GitHub Pages artifact. Select **GitHub Actions** as the Pages
publishing source. To republish the landing page with the last successful
bundle snapshot after a configuration change, run the bundle workflow from
`main` with `recover_pages` enabled.
