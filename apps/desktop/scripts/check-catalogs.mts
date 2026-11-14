import { execFileSync } from "node:child_process";
import { mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../src/features/i18n/catalogs/", import.meta.url));
const source_locale = "en";
const locales = [source_locale, "zh-Hans"];
const scratch = mkdtempSync(join(tmpdir(), "ctmux-i18n-"));
const require = createRequire(import.meta.url);
const formatjs = join(dirname(require.resolve("@formatjs/cli/package.json")), "bin", "formatjs");

try {
  const files = locales.map((locale) => {
    const messages = new Map<string, string>();
    for (const name of readdirSync(join(root, locale)).filter((name) => name.endsWith(".json"))) {
      const entries = JSON.parse(readFileSync(join(root, locale, name), "utf8")) as Record<string, unknown>;
      for (const [id, value] of Object.entries(entries)) {
        if (messages.has(id)) throw new Error(`Duplicate message ${id} in ${locale}/${name}`);
        if (typeof value !== "string" || !value.trim()) throw new Error(`Empty or invalid message ${id} in ${locale}/${name}`);
        messages.set(id, value);
      }
    }
    const file = join(scratch, `${locale}.json`);
    writeFileSync(file, JSON.stringify(Object.fromEntries(messages)));
    return file;
  });
  execFileSync(process.execPath, [
    formatjs, "verify", "--source-locale", source_locale,
    "--missing-keys", "--extra-keys", "--structural-equality", ...files,
  ], { stdio: "inherit" });
} finally {
  rmSync(scratch, { recursive: true, force: true });
}
