import { formatText, message, type Locale, type LocalizedMessage } from "../i18n/messages";

/** POSIX word quoting only: no shell execution, expansion, or operators. */
export function parseCommandLine(input: string, locale: Locale = "en"): { words: string[]; error: string | null } {
  const { words, error } = parseCommandLineWithFeedback(input);
  return { words, error: error ? formatText(locale, error) : null };
}

export function parseCommandLineWithFeedback(input: string): { words: string[]; error: LocalizedMessage | null } {
  const words: string[] = [];
  let word = "";
  let started = false;
  let quote: "'" | '"' | null = null;
  for (let i = 0; i < input.length; i += 1) {
    const character = input[i];
    if (character === "\0")
      return { words: [], error: message("tasks.command_null") };
    if (quote === "'") {
      if (character === "'") quote = null;
      else word += character;
    } else if (character === "\\") {
      const next = input[i + 1];
      if (next === undefined)
        return { words: [], error: message("tasks.command_escape") };
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
        error: message("tasks.command_operators"),
      };
    } else {
      started = true;
      word += character;
    }
  }
  if (quote)
    return {
      words: [],
      error: message("tasks.command_close_quote", { quote: quote === "'" ? "single" : "double" }),
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
