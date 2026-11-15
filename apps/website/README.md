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

The gallery loads `remote.png`, `vpn.png`, and `vpn-preview.mp4` from
`.demo-recordings/website/`. Use `CTL_WEBSITE_GALLERY_DIR` to select another
directory with those assets. The gallery omits any missing media. The included
VPN walkthrough uses simulated preview states, and the screenshots use the
desktop preview's fictional workspace.

For GitHub Pages, set `CTL_WEBSITE_MEDIA_HOST` under the repository's
`github-pages` environment variables. Use the public Cloudflare hostname
without a scheme or path. Publish `demo.mp4`, `poster.png`, `remote.png`,
`vpn.png`, and `vpn-preview.mp4` at that host's root with their respective
`video/mp4` and `image/png` content types.

The bundle workflow builds the landing page using
`bun apps/website/site.ts build --verify-media`. The hosted build links to
these five files over HTTPS and checks their responses before publishing.
It publishes the HTML, stylesheet, and icon together with the runtime bundle
registry in one GitHub Pages artifact. Select **GitHub Actions** as the Pages
publishing source. To republish the landing page with the last successful
bundle snapshot after a configuration change, run the bundle workflow from
`main` with `recover_pages` enabled.
