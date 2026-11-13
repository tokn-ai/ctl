import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { lstat, mkdir, mkdtemp, open, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test, { type TestContext } from "node:test";
import { promisify } from "node:util";
import { extractWindowsPortableFixture, windowsExecutableFixture, windowsPortableFixtureEntries } from "../shared/windows-portable.fixtures.mts";
import { createWindowsPortable, validateWindowsPortablePath, windowsPortableExecutables, type WindowsPortableOptions } from "../shared/windows-portable.mts";

const execute = promisify(execFile);

async function fixture(context: TestContext): Promise<WindowsPortableOptions> {
  const root = await mkdtemp(join(tmpdir(), "ctmux-portable-limits-"));
  context.after(() => rm(root, { recursive: true, force: true }));
  const target = "x86_64-pc-windows-msvc";
  const executables = {} as WindowsPortableOptions["executables"];
  for (const name of windowsPortableExecutables) {
    executables[name] = join(root, `${name}.exe`);
    await writeFile(executables[name], windowsExecutableFixture(target));
  }
  const resources_directory = join(root, "resources");
  await mkdir(resources_directory);
  return { target, app_version: "0.1.0", executables, resources_directory, archive_path: join(root, "output", "ctmux.zip") };
}

async function unpublished(options: WindowsPortableOptions): Promise<void> {
  await assert.rejects(lstat(options.archive_path), { code: "ENOENT" });
  assert.deepEqual(await readdir(join(options.archive_path, "..")), []);
}

test("Windows portable archives use forward slash paths and preserve hidden files and empty directories", async (context) => {
  const options = await fixture(context);
  const hidden = join(options.resources_directory!, ".hidden");
  await writeFile(hidden, "hidden fixture");
  if (process.platform === "win32") await execute("attrib.exe", ["+H", hidden]);
  await mkdir(join(options.resources_directory!, "empty"));
  const manifest = await createWindowsPortable(options);
  const entries = await windowsPortableFixtureEntries(options.archive_path);
  assert.deepEqual(entries.sort(), [
    "ctmux/", "ctmux/ctl-taskd.exe", "ctmux/ctld.exe", "ctmux/ctmux-app.exe", "ctmux/ctmuxd.exe",
    "ctmux/portable.json", "ctmux/resources/", "ctmux/resources/agent-bundles/",
    "ctmux/resources/agent-bundles/.hidden", "ctmux/resources/agent-bundles/empty/",
  ].sort());
  const extracted = join(options.archive_path, "..", "extracted");
  await extractWindowsPortableFixture(options.archive_path, extracted);
  assert.equal((await lstat(join(extracted, "ctmux/resources/agent-bundles/empty"))).isDirectory(), true);
  for (const [relative, digest] of Object.entries(manifest.files)) {
    assert.equal(createHash("sha256").update(await readFile(join(extracted, "ctmux", ...relative.split("/")))).digest("hex"), digest);
  }
});

test("Windows portable rejects all installer-reserved names and non-ASCII resource names", async (context) => {
  for (const name of ["CLOCK$.txt", "CONIN$.txt", "CONOUT$.txt", "caf\u00e9.txt"]) {
    // Device names cannot be created on Windows, but cross-packaging must also reject them.
    if (process.platform === "win32" && name !== "caf\u00e9.txt") continue;
    const options = await fixture(context);
    await writeFile(join(options.resources_directory!, name), "unsafe resource");
    await assert.rejects(createWindowsPortable(options), /unsafe Windows portable filename/);
    await unpublished(options);
  }
});

test("Windows portable enforces the installer full ZIP path and directory depth limits", async (context) => {
  // Test the byte boundary directly: macOS cannot create a source path this
  // long under its own 1,024-byte absolute path limit.
  const boundary = [...Array.from({ length: 3 }, () => "a".repeat(255)), "a".repeat(250)].join("/");
  assert.equal(Buffer.byteLength(`ctmux/${boundary}`), 1024);
  assert.doesNotThrow(() => validateWindowsPortablePath(boundary));
  assert.throws(() => validateWindowsPortablePath(`${boundary}a`), /path exceeds the installer limits/);
  assert.throws(() => validateWindowsPortablePath("a".repeat(256)), /unsafe Windows portable filename/);
  const options = await fixture(context);
  await mkdir(join(options.resources_directory!, ...Array.from({ length: 30 }, () => "a")), { recursive: true });
  await assert.rejects(createWindowsPortable(options), /path exceeds the installer limits/);
  await unpublished(options);
});

test("Windows portable counts empty directories toward the installer ZIP entry limit", async (context) => {
  const options = await fixture(context);
  // Four executables, portable.json, ctmux/, resources/ and agent-bundles/
  // leave room for 19,992 resource entries at the installer's 20,000 limit.
  for (let offset = 0; offset < 19_993; offset += 256) {
    await Promise.all(Array.from({ length: Math.min(256, 19_993 - offset) }, (_, index) =>
      mkdir(join(options.resources_directory!, `empty-${offset + index}`))));
  }
  await assert.rejects(createWindowsPortable(options), /too many entries/);
  await unpublished(options);
});

test("Windows portable rejects expanded payloads over 4 GiB before copying sparse files", { skip: process.platform === "win32" }, async (context) => {
  const options = await fixture(context);
  const resource = await open(join(options.resources_directory!, "oversized"), "wx");
  try {
    await resource.truncate(4 * 1024 * 1024 * 1024 + 1);
  } finally {
    await resource.close();
  }
  await assert.rejects(createWindowsPortable(options), /expanded size limit/);
  await unpublished(options);
});

test("Windows portable rejects manifests larger than the installer accepts", async (context) => {
  const options = await fixture(context);
  options.app_version = `0.1.0-${"a".repeat(4 * 1024 * 1024)}`;
  await assert.rejects(createWindowsPortable(options), /manifest exceeds the size limit/);
  await unpublished(options);
});

test("Windows portable rejects PE headers beyond the installer DOS offset limit", async (context) => {
  const options = await fixture(context);
  const offset = 16 * 1024 * 1024 + 1;
  const image = Buffer.alloc(offset + 1024);
  const valid = windowsExecutableFixture(options.target);
  valid.copy(image, 0, 0, 64);
  image.writeUInt32LE(offset, 0x3c);
  valid.copy(image, offset, 0x80);
  await writeFile(options.executables["ctmux-app"], image);
  await assert.rejects(createWindowsPortable(options), /invalid DOS\/PE header/);
  await unpublished(options);
});
