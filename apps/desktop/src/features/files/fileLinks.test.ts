import { describe, expect, it } from "vitest";
import { detectFileLinks, parseFileLink } from "./fileLinks";

describe("terminal file links", () => {
  it.each([
    ["src/main.rs:12:3", { path: "src/main.rs", line: 12, column: 3 }],
    ["main.rs:12:3", { path: "main.rs", line: 12, column: 3 }],
    ["C:\\work\\main.ts:9", { path: "C:\\work\\main.ts", line: 9, column: null }],
    ["file://terminal-host/work/a%20b.md#L42", { path: "/work/a b.md", line: 42, column: null }],
    ["file:///C:/work/main.ts:2:4", { path: "C:/work/main.ts", line: 2, column: 4 }],
    ["../README.md", { path: "../README.md", line: null, column: null }],
  ])("parses %s", (value, expected) => expect(parseFileLink(value)).toEqual(expected));

  it.each(["javascript:alert(1)", "javascript:1", "java%0Ascript:alert%281%29", "https://example.com", "file:///tmp/%00secret", "file:///tmp/a?query", "file:///tmp/a#script", "file:///tmp/a#L0", "src/a.rs:0", "src/a.rs:4294967296", "src/a.rs:99999999999999999999", "a\nb"])("rejects invalid target %s", (value) => {
    expect(parseFileLink(value)).toBeNull();
  });

  it("keeps web links separate and finds quoted paths, dotfiles and diagnostic locations", () => {
    const text = 'error in src/main.rs:12:3, "a folder/readme.md" .gitignore https://example.com/main.rs';
    const links = detectFileLinks(text);
    expect(links.map((link) => link.file)).toEqual([
      { path: "src/main.rs", line: 12, column: 3 },
      { path: "a folder/readme.md", line: null, column: null },
      { path: ".gitignore", line: null, column: null },
    ]);
    for (const link of links) expect(text.slice(link.start, link.end)).toBe(link.text);
  });
});
