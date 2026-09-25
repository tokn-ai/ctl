import { describe, expect, it } from "vitest";
import { formatCommandLine, parseCommandLine } from "./commandLine";

describe("command line words", () => {
  it.each([
    ['cargo run --bin "api server"', ["cargo", "run", "--bin", "api server"]],
    ["echo '' a\\ b pre'quoted'post", ["echo", "", "a b", "prequotedpost"]],
    ['echo "a\\qb" "a\\\"b"', ["echo", "a\\qb", 'a"b']],
    ["echo $HOME '*.rs' '~'", ["echo", "$HOME", "*.rs", "~"]],
    ["echo a\\\nb", ["echo", "ab"]],
    ["  ", []],
  ])("parses %s without shell expansion", (input, words) => {
    expect(parseCommandLine(input)).toEqual({ words, error: null });
  });
  it.each([
    'echo "unfinished',
    "echo 'unfinished",
    "echo trailing\\",
    "echo a | cat",
    "echo a && cat",
    "echo > file",
    "echo \0",
  ])("rejects incomplete or unsupported input: %s", (input) => {
    expect(parseCommandLine(input).error).toBeTruthy();
    expect(parseCommandLine(input).words).toEqual([]);
  });
  it("round trips structured arguments without losing empty strings or escapes", () => {
    const words = [
      "/path with spaces/tool",
      "",
      "a'b",
      'a"b',
      "C:\\Program Files\\tool",
      "$HOME",
      "|",
      "line\nbreak",
    ];
    expect(
      parseCommandLine(formatCommandLine(words[0], words.slice(1))).words,
    ).toEqual(words);
  });
});
