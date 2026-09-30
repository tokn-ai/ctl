import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { chmod, lstat, mkdir, mkdtemp, readFile, readlink, readdir, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test, { type TestContext } from "node:test";
import { promisify } from "node:util";
import { packageDesktopBundle, type DesktopBundleOptions } from "./package-desktop-bundle.mts";

const execute = promisify(execFile);

async function fixture(context: TestContext, target = "x86_64-unknown-linux-gnu"): Promise<DesktopBundleOptions> {
  const root = await mkdtemp(join(tmpdir(), "rmux-desktop-packaging-"));
  context.after(() => rm(root, { recursive: true, force: true }));
  return {
    target,
    bundle_id: "0.1.0-dev.0123456789ab",
    git_revision: "0123456789abcdef0123456789abcdef01234567",
    signing_mode: target.endsWith("apple-darwin") ? "unsigned" : "not_applicable",
    input_directory: join(root, "bundle"),
    output_directory: join(root, "assets"),
  };
}

async function file(path: string, content: string, mode = 0o644): Promise<void> {
  await mkdir(dirname(path), { recursive: true });
  await writeFile(path, content);
  await chmod(path, mode);
}

async function linuxInstallers(options: DesktopBundleOptions): Promise<void> {
  await file(join(options.input_directory, "deb", "rmux_0.1.0_amd64.deb"), "Debian installer");
  await file(join(options.input_directory, "rpm", "rmux-0.1.0-1.x86_64.rpm"), "RPM installer");
  await file(join(options.input_directory, "appimage", "rmux_0.1.0_amd64.AppImage"), "AppImage installer", 0o755);
}

async function macBundle(options: DesktopBundleOptions): Promise<string> {
  const app = join(options.input_directory, "macos", "rmux.app");
  await file(join(app, "Contents", "Info.plist"), "<plist>fixture app</plist>");
  await file(join(app, "Contents", "MacOS", "rmux-app"), "#!/bin/sh\nexit 0\n", 0o755);
  await file(join(app, "Contents", "Helpers", "ctld.app", "Contents", "MacOS", "ctld"), "helper", 0o751);
  await symlink("rmux-app", join(app, "Contents", "MacOS", "current"));
  await file(join(options.input_directory, "dmg", "rmux_0.1.0_aarch64.dmg"), "Disk image");
  return app;
}

test("stages every Linux installer with target-specific names and matching checksums", async (context) => {
  const options = await fixture(context);
  await linuxInstallers(options);
  // Tauri leaves build intermediates under these directories; only final files
  // in the format's immediate output directory are release installers.
  await file(join(options.input_directory, "deb", "intermediate", "ignored.deb"), "not an installer");
  const manifest = await packageDesktopBundle(options);
  assert.deepEqual(manifest, {
    schema_version: 1,
    target: options.target,
    bundle_id: options.bundle_id,
    git_revision: options.git_revision,
    signing_mode: "not_applicable",
    assets: manifest.assets,
  });
  assert.deepEqual(manifest.assets.map((asset) => asset.name), [".deb", ".rpm", ".AppImage"].map(
    (extension) => `rmux-${options.bundle_id}-${options.target}${extension}`,
  ));
  for (const asset of manifest.assets) {
    const contents = await readFile(join(options.output_directory, asset.name));
    assert.equal(asset.sha256, createHash("sha256").update(contents).digest("hex"));
    assert.equal(await readFile(join(options.output_directory, `${asset.name}.sha256`), "utf8"), `${asset.sha256}  ${asset.name}\n`);
  }
  assert.equal((await lstat(join(options.output_directory, manifest.assets[2].name))).mode & 0o777, 0o755);
  assert.deepEqual(JSON.parse(await readFile(join(options.output_directory, `desktop-${options.target}.json`), "utf8")), manifest);
  assert.equal((await readdir(options.output_directory)).length, 7);
});

test("archives the macOS app with executable modes and relative symlinks intact", async (context) => {
  const options = await fixture(context, "aarch64-apple-darwin");
  await macBundle(options);
  const manifest = await packageDesktopBundle(options);
  assert.equal(manifest.signing_mode, "unsigned");
  assert.deepEqual(manifest.assets.map((asset) => asset.name), [".dmg", ".app.tar.gz"].map(
    (extension) => `rmux-${options.bundle_id}-${options.target}${extension}`,
  ));
  const archive = join(options.output_directory, manifest.assets[1].name);
  const extracted = join(dirname(options.output_directory), "extracted");
  await mkdir(extracted);
  await execute("tar", ["-xzf", archive, "-C", extracted]);
  const app = join(extracted, "rmux.app");
  assert.equal((await lstat(join(app, "Contents", "MacOS", "rmux-app"))).mode & 0o777, 0o755);
  assert.equal((await lstat(join(app, "Contents", "Helpers", "ctld.app", "Contents", "MacOS", "ctld"))).mode & 0o777, 0o751);
  assert.equal(await readlink(join(app, "Contents", "MacOS", "current")), "rmux-app");
  assert.equal(await readFile(join(app, "Contents", "MacOS", "current"), "utf8"), "#!/bin/sh\nexit 0\n");
  for (const asset of manifest.assets) {
    assert.equal(asset.sha256, createHash("sha256").update(await readFile(join(options.output_directory, asset.name))).digest("hex"));
  }
});

for (const directory of ["deb", "rpm", "appimage"]) {
  test(`rejects incomplete Linux output without ${directory}`, async (context) => {
    const options = await fixture(context, "aarch64-unknown-linux-gnu");
    await linuxInstallers(options);
    await rm(join(options.input_directory, directory), { recursive: true });
    await assert.rejects(packageDesktopBundle(options), /missing .* installer/);
    await assert.rejects(lstat(options.output_directory), { code: "ENOENT" });
  });
}

test("rejects duplicate installer formats instead of choosing an arbitrary build", async (context) => {
  const options = await fixture(context);
  await linuxInstallers(options);
  await file(join(options.input_directory, "rpm", "old.rpm"), "old release");
  await assert.rejects(packageDesktopBundle(options), /exactly one \.rpm installer.*found 2/);
  await assert.rejects(lstat(options.output_directory), { code: "ENOENT" });
});

test("rejects missing macOS app and disk image", async (context) => {
  const options = await fixture(context, "x86_64-apple-darwin");
  const app = await macBundle(options);
  await rm(app, { recursive: true });
  await assert.rejects(packageDesktopBundle(options), /missing macOS application bundle/);
  await macBundle(options);
  await rm(join(options.input_directory, "dmg"), { recursive: true });
  await assert.rejects(packageDesktopBundle(options), /missing \.dmg installer/);
});

test("rejects symlink and empty installers", async (context) => {
  const options = await fixture(context);
  await linuxInstallers(options);
  const installer = join(options.input_directory, "deb", "rmux_0.1.0_amd64.deb");
  await rm(installer);
  await symlink("../rpm/rmux-0.1.0-1.x86_64.rpm", installer);
  await assert.rejects(packageDesktopBundle(options), /nonempty regular file/);
  await rm(installer);
  await file(installer, "");
  await assert.rejects(packageDesktopBundle(options), /nonempty regular file/);
});

test("rejects invalid identity and signing metadata before reading build output", async (context) => {
  const options = await fixture(context);
  await assert.rejects(packageDesktopBundle({ ...options, target: "../unsupported" }), /unsupported desktop target/);
  await assert.rejects(packageDesktopBundle({ ...options, bundle_id: "../../escape" }), /invalid bundle ID/);
  await assert.rejects(packageDesktopBundle({ ...options, git_revision: "short" }), /40 hexadecimal/);
  await assert.rejects(packageDesktopBundle({ ...options, signing_mode: "signed" }), /invalid signing mode/);
  await assert.rejects(packageDesktopBundle({ ...options, target: "aarch64-apple-darwin" }), /invalid signing mode/);
});

test("refuses to mix new assets with stale output", async (context) => {
  const options = await fixture(context);
  await linuxInstallers(options);
  await file(join(options.output_directory, "stale.deb"), "previous build");
  await assert.rejects(packageDesktopBundle(options), /output directory must be empty/);
  assert.deepEqual(await readdir(options.output_directory), ["stale.deb"]);
});
