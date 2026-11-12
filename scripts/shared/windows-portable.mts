import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { constants, createReadStream } from "node:fs";
import { copyFile, lstat, mkdir, mkdtemp, open, readdir, rm, writeFile } from "node:fs/promises";
import { basename, dirname, join, resolve } from "node:path";
import { promisify } from "node:util";

const execute = promisify(execFile);
const max_archive_entries = 20_000;
const max_expanded_bytes = 4 * 1024 * 1024 * 1024;
const max_manifest_bytes = 4 * 1024 * 1024;
export const windowsPortableExecutables = ["ctmux-app", "ctld", "ctmuxd", "ctl-taskd"] as const;
const windowsMachines = new Map([
  ["x86_64-pc-windows-msvc", 0x8664],
  ["aarch64-pc-windows-msvc", 0xaa64],
]);

export interface WindowsPortableManifest {
  schema_version: 1;
  app_version: string;
  target_triple: string;
  files: Record<string, string>;
}

export interface WindowsPortableOptions {
  target: string;
  app_version: string;
  executables: Record<(typeof windowsPortableExecutables)[number], string>;
  resources_directory?: string;
  archive_path: string;
}

export function isWindowsDesktopTarget(target: string): boolean {
  return windowsMachines.has(target);
}

/** Validate a payload path using the installer's full ZIP path limits. */
export function validateWindowsPortablePath(relative: string): void {
  const segments = relative.split("/");
  if (Buffer.byteLength(`ctmux/${relative}`) > 1024 || segments.length + 1 > 32) {
    throw new Error(`Windows portable path exceeds the installer limits: ${relative}`);
  }
  for (const segment of segments) validateWindowsName(segment);
}

/** Validate a standalone executable without launching a foreign target. */
export async function validateWindowsExecutableTarget(path: string, target: string): Promise<void> {
  const machine = windowsMachines.get(target);
  if (machine === undefined) throw new Error(`unsupported Windows desktop target: ${target}`);
  await validateWindowsExecutable(path, machine);
}

/** Assemble a portable app explicitly: Tauri --no-bundle does not copy sidecars or resources. */
export async function createWindowsPortable(options: WindowsPortableOptions): Promise<WindowsPortableManifest> {
  const machine = windowsMachines.get(options.target);
  if (machine === undefined) throw new Error(`unsupported Windows desktop target: ${options.target}`);
  if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/.test(options.app_version)) {
    throw new Error("Windows portable app version must be a semantic version");
  }
  const archive = resolve(options.archive_path);
  if (!basename(archive).endsWith(".zip")) throw new Error("Windows portable archive must end in .zip");
  await mkdir(dirname(archive), { recursive: true });
  const staging = await mkdtemp(join(dirname(archive), ".ctmux-portable-"));
  try {
    const app = join(staging, "ctmux");
    await mkdir(app);
    const files = new Map<string, string>();
    const names = new Map<string, string>();
    let expanded_bytes = 0;
    const reserve = (relative: string): void => {
      validateWindowsPortablePath(relative);
      const segments = relative.split("/");
      for (let count = 1; count <= segments.length; count += 1) {
        const path = segments.slice(0, count).join("/"), key = path.toLowerCase();
        const previous = names.get(key);
        if (previous !== undefined && previous !== path) throw new Error(`duplicate Windows portable path: ${path}`);
        names.set(key, path);
      }
      // Include every directory, plus ctmux/ and portable.json. This remains
      // conservative when a ZIP implementation omits nonempty directory entries.
      if (names.size + 2 > max_archive_entries) throw new Error("Windows portable archive contains too many entries");
    };
    const copy = async (source: string, relative: string): Promise<void> => {
      reserve(relative);
      if (files.has(relative)) throw new Error(`duplicate Windows portable path: ${relative}`);
      const metadata = await lstat(source);
      if (!metadata.isFile() || metadata.isSymbolicLink()) throw new Error(`portable payload must contain regular files: ${relative}`);
      if (expanded_bytes + metadata.size > max_expanded_bytes) throw new Error("Windows portable archive exceeds the expanded size limit");
      const destination = join(app, ...relative.split("/"));
      await mkdir(dirname(destination), { recursive: true });
      await copyFile(source, destination, constants.COPYFILE_EXCL);
      expanded_bytes += (await lstat(destination)).size;
      if (expanded_bytes > max_expanded_bytes) throw new Error("Windows portable archive exceeds the expanded size limit");
      const digest = createHash("sha256");
      for await (const chunk of createReadStream(destination)) digest.update(chunk);
      files.set(relative, digest.digest("hex"));
    };
    for (const name of windowsPortableExecutables) {
      await copy(options.executables[name], `${name}.exe`);
      await validateWindowsExecutable(join(app, `${name}.exe`), machine);
    }
    if (options.resources_directory !== undefined) {
      const walk = async (source: string, relative: string): Promise<void> => {
        const metadata = await lstat(source);
        if (metadata.isSymbolicLink()) throw new Error(`portable payload cannot contain symlinks: ${relative}`);
        if (metadata.isDirectory()) {
          reserve(relative);
          await mkdir(join(app, ...relative.split("/")), { recursive: true });
          for (const name of (await readdir(source)).sort()) {
            validateWindowsName(name);
            await walk(join(source, name), `${relative}/${name}`);
          }
        } else {
          await copy(source, relative);
        }
      };
      const resources = await lstat(options.resources_directory);
      if (!resources.isDirectory() || resources.isSymbolicLink()) throw new Error("portable resources must be a regular directory");
      await walk(options.resources_directory, "resources/agent-bundles");
    }
    const manifest: WindowsPortableManifest = {
      schema_version: 1, app_version: options.app_version, target_triple: options.target,
      files: Object.fromEntries([...files].sort(([left], [right]) => left < right ? -1 : left > right ? 1 : 0)),
    };
    const manifest_bytes = Buffer.from(`${JSON.stringify(manifest, null, 2)}\n`);
    if (manifest_bytes.length > max_manifest_bytes) throw new Error("Windows portable manifest exceeds the size limit");
    if (expanded_bytes + manifest_bytes.length > max_expanded_bytes) throw new Error("Windows portable archive exceeds the expanded size limit");
    await writeFile(join(app, "portable.json"), manifest_bytes, { flag: "wx" });
    const temporary_archive = join(staging, "payload.zip");
    if (process.platform === "win32") {
      // PowerShell's .NET compatibility defaults can make CreateFromDirectory
      // emit backslashes. Explicit entry names keep ZIP paths portable and also
      // include hidden files and empty directories without shell enumeration.
      const zip_plan = join(staging, "zip-entries.txt");
      // Validated ASCII names contain no newlines. A line plan avoids legacy
      // PowerShell JSON size limits when packages contain many directories.
      await writeFile(zip_plan, [
        "ctmux/",
        ...[...names.values()].filter((name) => !files.has(name)).sort().map((name) => `ctmux/${name}/`),
        ...[...files.keys(), "portable.json"].sort().map((name) => `ctmux/${name}`),
      ].join("\n"), { flag: "wx" });
      await execute("powershell.exe", ["-NoLogo", "-NoProfile", "-NonInteractive", "-Command",
        "$ErrorActionPreference = 'Stop'; Add-Type -AssemblyName System.IO.Compression; Add-Type -AssemblyName System.IO.Compression.FileSystem; " +
        "$archive = [System.IO.Compression.ZipFile]::Open($env:CTL_DESKTOP_ZIP_OUTPUT, [System.IO.Compression.ZipArchiveMode]::Create); " +
        "try { foreach ($name in [System.IO.File]::ReadLines($env:CTL_DESKTOP_ZIP_PLAN)) { " +
        "if ($name.EndsWith('/')) { $null = $archive.CreateEntry($name) } else { " +
        "$relative = $name.Substring(6); " +
        "$source = [System.IO.Path]::Combine($env:CTL_DESKTOP_ZIP_ROOT, $relative); " +
        "$null = [System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile($archive, $source, $name, " +
        "[System.IO.Compression.CompressionLevel]::Optimal) " +
        "} } } finally { $archive.Dispose() }"], {
        env: { ...process.env, CTL_DESKTOP_ZIP_ROOT: app, CTL_DESKTOP_ZIP_OUTPUT: temporary_archive, CTL_DESKTOP_ZIP_PLAN: zip_plan },
      });
    } else {
      await execute("zip", ["-q", "-r", temporary_archive, "ctmux"], { cwd: staging });
    }
    await copyFile(temporary_archive, archive, constants.COPYFILE_EXCL);
    return manifest;
  } finally {
    await rm(staging, { recursive: true, force: true });
  }
}

function validateWindowsName(name: string): void {
  if (!name || name.length > 255 || name === "." || name === ".." || /[^\x20-\x7e]|[<>:"/\\|?*]/.test(name) || /[. ]$/.test(name) ||
    /^(?:con|prn|aux|nul|clock\$|conin\$|conout\$|com[1-9]|lpt[1-9])(?:\.|$)/i.test(name)) {
    throw new Error(`unsafe Windows portable filename: ${name}`);
  }
}

/** Inspect headers, never execute a potentially foreign-architecture binary. */
async function validateWindowsExecutable(path: string, machine: number): Promise<void> {
  const metadata = await lstat(path);
  if (!metadata.isFile() || metadata.isSymbolicLink() || metadata.size < 64) throw new Error("portable executable must be a regular PE file");
  const file = await open(path, "r");
  try {
    const dos = Buffer.alloc(64);
    await file.read(dos, 0, dos.length, 0);
    const offset = dos.readUInt32LE(0x3c);
    if (dos.readUInt16LE(0) !== 0x5a4d || offset < 64 || offset > 16 * 1024 * 1024 || offset > metadata.size - 24) throw new Error("portable executable has an invalid DOS/PE header");
    const header = Buffer.alloc(24);
    await file.read(header, 0, header.length, offset);
    const sections = header.readUInt16LE(6), optional_size = header.readUInt16LE(20);
    if (header.readUInt32LE(0) !== 0x4550 || header.readUInt16LE(4) !== machine) throw new Error("portable executable PE machine does not match the Windows target");
    if (sections === 0 || sections > 96 || optional_size < 112 || offset + 24 + optional_size + sections * 40 > metadata.size ||
      (header.readUInt16LE(22) & 0x0002) === 0 || (header.readUInt16LE(22) & 0x2000) !== 0) {
      throw new Error("portable executable has an invalid PE image header");
    }
    const magic = Buffer.alloc(2);
    await file.read(magic, 0, magic.length, offset + 24);
    if (magic.readUInt16LE(0) !== 0x20b) throw new Error("portable executable must be a PE32+ image");
  } finally {
    await file.close();
  }
}
