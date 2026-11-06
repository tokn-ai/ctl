import { execFileSync } from "node:child_process";

// Verify Cargo's resolved production dependency graph without compiling a second binary.
for (const target of ["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl", "x86_64-unknown-linux-gnu", "aarch64-apple-darwin"]) {
  const graph = execFileSync("cargo", ["tree", "--locked", "--color", "never", "-p", "ctl-cli", "--target", target,
    "-e", "normal,build,features", "-i", "reqwest"], { encoding: "utf8" });
  const has = (feature: string) => graph.includes(`reqwest feature "${feature}"`);
  if (!has("native-tls") || !has("system-proxy") || has("native-tls-vendored") !== target.endsWith("-musl")) {
    throw new Error(`Unexpected TLS features for ${target}:\n${graph}`);
  }
  console.log(`${target}: ${has("native-tls-vendored") ? "vendored" : "system"} native TLS`);
}
