import { execFile } from "node:child_process";
import { mkdir, readFile } from "node:fs/promises";
import { promisify } from "node:util";

const execute = promisify(execFile);

/** Read actual central-directory names; native extraction accepts unsafe separators. */
export async function windowsPortableFixtureEntries(archive: string): Promise<string[]> {
  const bytes = await readFile(archive);
  const end = bytes.lastIndexOf(Buffer.from([0x50, 0x4b, 0x05, 0x06]));
  if (end < 0 || end + 22 > bytes.length) throw new Error("fixture ZIP end record is missing");
  const count = bytes.readUInt16LE(end + 10);
  let offset = bytes.readUInt32LE(end + 16);
  const entries = [];
  for (let index = 0; index < count; index += 1) {
    if (offset + 46 > end || bytes.readUInt32LE(offset) !== 0x02014b50) throw new Error("fixture ZIP directory is invalid");
    const name_length = bytes.readUInt16LE(offset + 28);
    const extra_length = bytes.readUInt16LE(offset + 30);
    const comment_length = bytes.readUInt16LE(offset + 32);
    const next = offset + 46 + name_length + extra_length + comment_length;
    if (next > end) throw new Error("fixture ZIP entry exceeds the directory");
    entries.push(bytes.subarray(offset + 46, offset + 46 + name_length).toString("utf8"));
    offset = next;
  }
  return entries;
}

/** A PE32+ header fixture, deliberately never launched by packaging tests. */
export function windowsExecutableFixture(target: string): Buffer {
  const image = Buffer.alloc(1024);
  image.writeUInt16LE(0x5a4d, 0);
  image.writeUInt32LE(0x80, 0x3c);
  image.writeUInt32LE(0x4550, 0x80);
  image.writeUInt16LE(target.startsWith("aarch64-") ? 0xaa64 : 0x8664, 0x84);
  image.writeUInt16LE(1, 0x86);
  image.writeUInt16LE(240, 0x94);
  image.writeUInt16LE(0x22, 0x96);
  image.writeUInt16LE(0x20b, 0x98);
  return image;
}

export async function extractWindowsPortableFixture(archive: string, destination: string): Promise<void> {
  await mkdir(destination, { recursive: true });
  if (process.platform === "win32") {
    await execute("powershell.exe", ["-NoLogo", "-NoProfile", "-NonInteractive", "-Command",
      "$ErrorActionPreference = 'Stop'; Add-Type -AssemblyName System.IO.Compression.FileSystem; " +
      "[System.IO.Compression.ZipFile]::ExtractToDirectory($env:CTL_TEST_ZIP_SOURCE, $env:CTL_TEST_ZIP_DESTINATION)"], {
      env: { ...process.env, CTL_TEST_ZIP_SOURCE: archive, CTL_TEST_ZIP_DESTINATION: destination },
    });
  } else {
    await execute("unzip", ["-q", archive, "-d", destination]);
  }
}
