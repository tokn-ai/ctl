import { createIntl, createIntlCache, ReactIntlErrorCode } from "react-intl";

export type Locale = "en" | "zh-Hans";
export type LocalePreference = "system" | Locale;
export type MessageValues = Readonly<Record<string, string | number | boolean | null | LocalizedMessage>>;

export interface LocalizedMessage {
  id: string;
  values?: MessageValues;
}

/** Literal names and external diagnostics can share a field with app-owned copy. */
export type DisplayText = string | LocalizedMessage;

export function message(id: string, values?: MessageValues): LocalizedMessage {
  return values === undefined ? { id } : { id, values };
}

export class LocalizedError extends Error {
  readonly text: LocalizedMessage;

  constructor(id: string, values?: MessageValues) {
    super(translate("en", id, values));
    this.text = message(id, values);
  }
}

function catalog(locale: Locale) {
  const modules = locale === "en"
    ? import.meta.glob<Record<string, string>>("./catalogs/en/*.json", { eager: true, import: "default" })
    : import.meta.glob<Record<string, string>>("./catalogs/zh-Hans/*.json", { eager: true, import: "default" });
  return Object.assign({}, ...Object.values(modules)) as Record<string, string>;
}

const english = catalog("en");
const chinese = catalog("zh-Hans");
const cache = createIntlCache();
const formatters = Object.fromEntries((["en", "zh-Hans"] as const).map((locale) => [
  locale,
  createIntl({
    locale,
    defaultLocale: "en",
    messages: locale === "en" ? english : chinese,
    onError: (error) => {
      if (error.code !== ReactIntlErrorCode.MISSING_TRANSLATION || import.meta.env.DEV) console.error(error);
    },
  }, cache),
])) as Record<Locale, ReturnType<typeof createIntl>>;

export function formatter(locale: Locale) {
  return formatters[locale];
}

export function translate(locale: Locale, id: string, values?: MessageValues): string {
  const resolved = values && Object.fromEntries(Object.entries(values).map(([key, value]) => [
    key,
    value !== null && typeof value === "object" ? formatText(locale, value) : value,
  ]));
  return formatter(locale).formatMessage({ id, defaultMessage: english[id] }, resolved, { ignoreTag: true });
}

export function formatText(locale: Locale, text: DisplayText): string {
  return typeof text === "string" ? text : translate(locale, text.id, text.values);
}

/** Incident identity depends on message data, independently of the display locale. */
export function textSignature(text: DisplayText): unknown {
  if (typeof text === "string") return text;
  return [text.id, Object.entries(text.values ?? {})
    .sort(([left], [right]) => left.localeCompare(right, "en"))
    .map(([key, value]) => [key, value !== null && typeof value === "object" ? textSignature(value) : value])];
}
