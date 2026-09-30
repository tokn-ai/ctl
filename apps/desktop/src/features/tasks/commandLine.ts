/** POSIX word quoting only: no shell execution, expansion, or operators. */
export function parseCommandLine(input: string): {
  words: string[];
  error: string | null;
} {
  const words: string[] = [];
  let word = "";
  let started = false;
  let quote: "'" | '"' | null = null;
  for (let i = 0; i < input.length; i += 1) {
    const character = input[i];
    if (character === "\0")
      return { words: [], error: "Commands cannot contain null characters." };
    if (quote === "'") {
      if (character === "'") quote = null;
      else word += character;
    } else if (character === "\\") {
      const next = input[i + 1];
      if (next === undefined)
        return { words: [], error: "Finish the escape after the backslash." };
      if (quote === '"' && !["$", "`", '"', "\\", "\n"].includes(next)) {
        word += character;
      } else {
        i += 1;
        if (next !== "\n") {
          word += next;
          started = true;
        }
      }
    } else if (quote === '"') {
      if (character === '"') quote = null;
      else word += character;
    } else if (character === "'" || character === '"') {
      quote = character;
      started = true;
    } else if (/[ \t\n]/.test(character)) {
      if (started) {
        words.push(word);
        word = "";
        started = false;
      }
    } else if ("|&;<>()".includes(character)) {
      return {
        words: [],
        error:
          "Shell operators require an explicit shell, for example sh -c 'command | other'.",
      };
    } else {
      started = true;
      word += character;
    }
  }
  if (quote)
    return {
      words: [],
      error: `Close the ${quote === "'" ? "single" : "double"} quote.`,
    };
  if (started) words.push(word);
  return { words, error: null };
}

export function formatCommandLine(
  program: string,
  args: readonly string[],
): string {
  if (!program && !args.length) return "";
  return [program, ...args]
    .map((word) =>
      /^[a-zA-Z0-9_./:@%+=,-]+$/.test(word)
        ? word
        : `'${word.split("'").join("'\\''")}'`,
    )
    .join(" ");
}
