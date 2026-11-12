import { execFile } from "node:child_process";
import { mkdir } from "node:fs/promises";
import { promisify } from "node:util";

const execute = promisify(execFile);

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
