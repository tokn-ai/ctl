import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";
import { parseOptions, rectangle, takeDirectory } from "./demo.mts";

test("recording destinations cannot escape the ignored demo directory", () => {
  for (const name of ["../private", "/tmp/output", "take/name", "..", "", "-take"]) {
    assert.throws(() => takeDirectory("vpn", name));
  }
  assert.throws(() => takeDirectory("../vpn", "take-01"));
  assert.ok(takeDirectory("vpn", "take-01").endsWith(path.join(".demo-recordings", "vpn", "take-01")));
});

test("capture rectangle permits secondary displays but rejects invalid dimensions", () => {
  assert.equal(rectangle("-1280,0,1280,800"), "-1280,0,1280,800");
  for (const value of ["0,0,0,800", "0,0,-1,800", "0,0,1280", "0,0,1.5,800", "0,0,99999999999999999999,800"]) {
    assert.throws(() => rectangle(value));
  }
});

test("capture options reject ambiguity before starting a recorder", () => {
  assert.deepEqual(parseOptions(["--take", "take-01", "--rect", "-1280,0,1280,800"]), {
    "--take": "take-01", "--rect": "-1280,0,1280,800",
  });
  for (const args of [["--take"], ["take", "one"], ["--take", "one", "--take", "two"], ["--take", "--rect"]]) {
    assert.throws(() => parseOptions(args));
  }
});
