import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { chmod, lstat, mkdir, mkdtemp, readFile, readlink, readdir, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test, { type TestContext } from "node:test";
import { promisify } from "node:util";
import { extractWindowsPortableFixture, windowsExecutableFixture } from "../shared/windows-portable.fixtures.mts";
import { windowsPortableExecutables } from "../shared/windows-portable.mts";
import { packageDesktopBundle, type DesktopBundleOptions } from "./package-desktop-bundle.mts";

const execute = promisify(execFile);

async function fixture(context: TestContext, target = "x86_64-unknown-linux-gnu"): Promise<DesktopBundleOptions> {
  const root = await mkdtemp(join(tmpdir(), "ctmux-desktop-packaging-"));
  context.after(() => rm(root, { recursive: true, force: true }));
  return {
    target,
    app_version: "0.1.0",
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
  await file(join(options.input_directory, "deb", "ctmux_0.1.0_amd64.deb"), "Debian installer");
  await file(join(options.input_directory, "rpm", "ctmux-0.1.0-1.x86_64.rpm"), "RPM installer");
  await file(join(options.input_directory, "appimage", "ctmux_0.1.0_amd64.AppImage"), "AppImage installer", 0o755);
}

async function macBundle(options: DesktopBundleOptions): Promise<string> {
  const app = join(options.input_directory, "macos", "ctmux.app");
  await file(join(app, "Contents", "Info.plist"), "<plist>fixture app</plist>");
  await file(join(app, "Contents", "MacOS", "ctmux-app"), "#!/bin/sh\nexit 0\n", 0o755);
  await file(join(app, "Contents", "Helpers", "ctld.app", "Contents", "MacOS", "ctld"), "helper", 0o751);
  await symlink("ctmux-app", join(app, "Contents", "MacOS", "current"));
  await file(join(options.input_directory, "dmg", "ctmux_0.1.0_aarch64.dmg"), "Disk image");
  return app;
}

async function windowsBundle(options: DesktopBundleOptions): Promise<void> {
  options.app_version = "0.1.0";
  options.resources_directory = join(dirname(options.input_directory), "resources");
  await mkdir(options.input_directory, { recursive: true });
  for (const executable of windowsPortableExecutables) {
    await writeFile(join(options.input_directory, `${executable}.exe`), windowsExecutableFixture(options.target));
  }
  await file(join(options.resources_directory, "bundle/.hidden"), "hidden bundled resource");
}

test("packages x64 and ARM64 Windows executables and resources with a complete portable inventory", async (context) => {
  for (const target of ["x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"]) {
    const options = await fixture(context, target);
    await windowsBundle(options);
    const manifest = await packageDesktopBundle(options);
    assert.equal(manifest.signing_mode, "not_applicable");
    assert.equal(manifest.schema_version, 1);
    assert.deepEqual(manifest.assets.map((asset) => asset.name), [`ctmux-${options.bundle_id}-${target}.zip`]);
    const archive = join(options.output_directory, manifest.assets[0].name);
    assert.equal(manifest.assets[0].sha256, createHash("sha256").update(await readFile(archive)).digest("hex"));
    const extracted = join(dirname(options.output_directory), "extracted");
    await extractWindowsPortableFixture(archive, extracted);
    assert.deepEqual(await readdir(extracted), ["ctmux"]);
    const app = join(extracted, "ctmux");
    const portable = JSON.parse(await readFile(join(app, "portable.json"), "utf8"));
    assert.deepEqual(Object.keys(portable).sort(), ["app_version", "files", "schema_version", "target_triple"]);
    assert.equal(portable.app_version, options.app_version);
    assert.equal(portable.target_triple, target);
    assert.equal(portable.schema_version, 1);
    assert.deepEqual(Object.keys(portable.files), [
      "ctl-taskd.exe", "ctld.exe", "ctmux-app.exe", "ctmuxd.exe", "resources/agent-bundles/bundle/.hidden",
    ]);
    for (const [path, digest] of Object.entries(portable.files)) {
      assert.equal(digest, createHash("sha256").update(await readFile(join(app, ...path.split("/")))).digest("hex"));
    }
    assert.equal((await readdir(options.output_directory)).length, 3);
  }
});

test("Windows packaging rejects missing, malformed and foreign-machine executables without publishing a manifest", async (context) => {
  for (const failure of ["missing", "malformed", "foreign"] as const) {
    for (const executable of windowsPortableExecutables) {
      const options = await fixture(context, "aarch64-pc-windows-msvc");
      await windowsBundle(options);
      const path = join(options.input_directory, `${executable}.exe`);
      if (failure === "missing") await rm(path);
      else await writeFile(path, failure === "malformed" ? Buffer.alloc(1024) : windowsExecutableFixture("x86_64-pc-windows-msvc"));
      await assert.rejects(packageDesktopBundle(options), failure === "missing" ? /ENOENT/ : /PE/);
      assert.deepEqual(await readdir(options.output_directory), []);
    }
  }
});

test("Windows packaging requires an app version and refuses unsafe or ambiguous resource paths", async (context) => {
  const missing_version = await fixture(context, "x86_64-pc-windows-msvc");
  missing_version.app_version = "";
  await assert.rejects(packageDesktopBundle(missing_version), /invalid app version/);
  for (const name of ["CON.txt", "trailing.", "bad:name"]) {
    // Windows cannot create these names; rejection on Unix also prevents unsafe cross-packaging.
    if (process.platform === "win32") continue;
    const options = await fixture(context, "x86_64-pc-windows-msvc");
    await windowsBundle(options);
    await file(join(options.resources_directory!, name), "unsafe resource");
    await assert.rejects(packageDesktopBundle(options), /unsafe Windows portable filename/);
    assert.deepEqual(await readdir(options.output_directory), []);
  }
  if (process.platform !== "win32" && process.platform !== "darwin") {
    const options = await fixture(context, "x86_64-pc-windows-msvc");
    await windowsBundle(options);
    await file(join(options.resources_directory!, "CASE"), "one");
    await file(join(options.resources_directory!, "case"), "two");
    await assert.rejects(packageDesktopBundle(options), /duplicate Windows portable path/);
  }
});

test("Windows packaging rejects resource symlinks", { skip: process.platform === "win32" }, async (context) => {
  const options = await fixture(context, "x86_64-pc-windows-msvc");
  await windowsBundle(options);
  await symlink("bundle/.hidden", join(options.resources_directory!, "linked"));
  await assert.rejects(packageDesktopBundle(options), /cannot contain symlinks/);
  assert.deepEqual(await readdir(options.output_directory), []);
});

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
    (extension) => `ctmux-${options.bundle_id}-${options.target}${extension}`,
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
    (extension) => `ctmux-${options.bundle_id}-${options.target}${extension}`,
  ));
  assert.deepEqual(JSON.parse(await readFile(join(options.output_directory, `desktop-${options.target}.json`), "utf8")), manifest);
  const archive = join(options.output_directory, manifest.assets[1].name);
  const extracted = join(dirname(options.output_directory), "extracted");
  await mkdir(extracted);
  await execute("tar", ["-xzf", archive, "-C", extracted]);
  const app = join(extracted, "ctmux.app");
  assert.equal((await lstat(join(app, "Contents", "MacOS", "ctmux-app"))).mode & 0o777, 0o755);
  assert.equal((await lstat(join(app, "Contents", "Helpers", "ctld.app", "Contents", "MacOS", "ctld"))).mode & 0o777, 0o751);
  assert.equal(await readlink(join(app, "Contents", "MacOS", "current")), "ctmux-app");
  assert.equal(await readFile(join(app, "Contents", "MacOS", "current"), "utf8"), "#!/bin/sh\nexit 0\n");
  for (const asset of manifest.assets) {
    assert.equal(asset.sha256, createHash("sha256").update(await readFile(join(options.output_directory, asset.name))).digest("hex"));
  }
});

test("keeps unsigned and signed macOS release packages at distinct names", async (context) => {
  const options = await fixture(context, "aarch64-apple-darwin");
  options.bundle_id = options.app_version;
  await macBundle(options);

  const unsigned = await packageDesktopBundle(options);
  assert.equal(unsigned.signing_mode, "unsigned");
  assert.deepEqual(unsigned.assets.map((asset) => asset.name), [
    "ctmux-0.1.0-aarch64-apple-darwin-unsigned.dmg",
    "ctmux-0.1.0-aarch64-apple-darwin-unsigned.app.tar.gz",
  ]);
  assert.deepEqual(JSON.parse(await readFile(join(options.output_directory, "desktop-aarch64-apple-darwin-unsigned.json"), "utf8")), unsigned);
  for (const asset of unsigned.assets) {
    assert.equal(await readFile(join(options.output_directory, `${asset.name}.sha256`), "utf8"), `${asset.sha256}  ${asset.name}\n`);
  }

  const signedDirectory = join(dirname(options.output_directory), "signed");
  const signed = await packageDesktopBundle({ ...options, signing_mode: "signed", output_directory: signedDirectory });
  assert.deepEqual(signed.assets.map((asset) => asset.name), [
    "ctmux-0.1.0-aarch64-apple-darwin.dmg",
    "ctmux-0.1.0-aarch64-apple-darwin.app.tar.gz",
  ]);
  assert.deepEqual(JSON.parse(await readFile(join(signedDirectory, "desktop-aarch64-apple-darwin.json"), "utf8")), signed);
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
  const installer = join(options.input_directory, "deb", "ctmux_0.1.0_amd64.deb");
  await rm(installer);
  await symlink("../rpm/ctmux-0.1.0-1.x86_64.rpm", installer);
  await assert.rejects(packageDesktopBundle(options), /nonempty regular file/);
  await rm(installer);
  await file(installer, "");
  await assert.rejects(packageDesktopBundle(options), /nonempty regular file/);
});

test("rejects invalid identity and signing metadata before reading build output", async (context) => {
  const options = await fixture(context);
  await assert.rejects(packageDesktopBundle({ ...options, target: "../unsupported" }), /unsupported desktop target/);
  await assert.rejects(packageDesktopBundle({ ...options, app_version: "../../escape" }), /invalid app version/);
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
