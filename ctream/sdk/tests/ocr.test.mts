import assert from "node:assert/strict";
import test from "node:test";
import { findText, type OcrOptions, type Screenshot } from "@tokn-ai/ctream-sdk";

const image: Screenshot = {
  width: 100, height: 48, mime_type: "image/png",
  data: "iVBORw0KGgoAAAANSUhEUgAAAGQAAAAwCAIAAABon0jiAAAAJElEQVR4nO3BAQEAAACCIP+vbkhAAQAAAAAAAAAAAAAAAADvBjhwAAFP/XfkAAAAAElFTkSuQmCC",
};
const header = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext";

function fixture(rows: string[], language = "eng"): OcrOptions {
  const tsv = [header, ...rows].join("\r\n") + "\r\n";
  const script = `
    const assert = require("node:assert/strict");
    assert.deepEqual(process.argv.slice(1), ["stdin", "stdout", "-l", ${JSON.stringify(language)}, "tsv"]);
    const bytes = require("node:fs").readFileSync(0);
    assert.equal(bytes.toString("base64"), ${JSON.stringify(image.data)});
    process.stdout.write(${JSON.stringify(tsv)});
  `;
  return { executable: [process.execPath, "-e", script, "--"] };
}

test("OCR finds repeated consecutive words and returns their centers in screenshot pixels", async () => {
  const rows = [
    "5\t1\t1\t1\t1\t1\t2\t4\t10\t8\t92\tSave",
    "5\t1\t1\t1\t1\t2\t14\t3\t8\t8\t97\tAs",
    "5\t1\t1\t1\t1\t3\t25\t4\t10\t8\t92\tSAVE",
    "5\t1\t1\t1\t1\t4\t37\t3\t8\t8\t97\tAS",
    "5\t1\t1\t1\t1\t5\t50\t4\t10\t8\t92\tSave",
    "5\t1\t2\t1\t1\t1\t2\t20\t8\t8\t97\tAs",
    "5\t1\t2\t1\t1\t2\t20\t20\t12\t8\t97\tSaved",
    "5\t1\t2\t1\t1\t3\t36\t20\t8\t8\t97\tAS",
  ];
  const opts = { ...fixture(rows, "deu+eng"), language: "deu+eng" };
  assert.deepEqual(await findText(image, "  sAve   aS ", opts), [
    { text: "Save As", x: 12, y: 7, bounds: { x: 2, y: 3, width: 20, height: 9 } },
    { text: "SAVE AS", x: 35, y: 7, bounds: { x: 25, y: 3, width: 20, height: 9 } },
  ]);
  assert.deepEqual(await findText(image, "absent", fixture([])), []);
});

test("OCR rejects inconsistent geometry and propagates local process errors", async () => {
  await assert.rejects(findText({ ...image, width: 1 }, "menu"), /matching pixel dimensions/);
  await assert.rejects(findText(image, " "), /query must contain text/);
  await assert.rejects(findText(image, "menu", fixture([
    "5\t1\t1\t1\t1\t1\t99\t4\t10\t8\t92\tMenu",
  ])), /invalid pixel bounds/);
  await assert.rejects(findText(image, "menu", {
    executable: [process.execPath, "-e", 'console.error("missing language model");process.exit(1)', "--"],
  }), /Tesseract OCR failed: missing language model/);
});
