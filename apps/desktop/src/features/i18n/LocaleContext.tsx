import { createContext, useCallback, useContext, useEffect, useMemo, useState, type ReactNode } from "react";
import { formatter, formatText, translate, type DisplayText, type Locale, type LocalePreference, type MessageValues } from "./messages";

const PREFERENCE_KEY = "ctmux.desktop.language";

function validPreference(value: string | null): LocalePreference {
  return value === "en" || value === "zh-Hans" ? value : "system";
}

function readPreference(): LocalePreference {
  try { return validPreference(window.localStorage.getItem(PREFERENCE_KEY)); }
  catch { return "system"; }
}

export function resolveLocale(preference: LocalePreference, languages: readonly string[]): Locale {
  if (preference !== "system") return preference;
  for (const language of languages) {
    try {
      const locale = new Intl.Locale(language);
      if (locale.language === "en") return "en";
      if (locale.language === "zh" && locale.maximize().script === "Hans") return "zh-Hans";
    } catch { /* Ignore malformed OS language preferences. */ }
  }
  return "en";
}

function systemLocale() {
  return resolveLocale("system", navigator.languages?.length ? navigator.languages : [navigator.language]);
}

function localeContext(locale: Locale, preference: LocalePreference, setPreference: (value: LocalePreference) => void) {
  return {
    locale,
    preference,
    setPreference,
    intl: formatter(locale),
    t: (id: string, values?: MessageValues) => translate(locale, id, values),
    formatText: (value: DisplayText) => formatText(locale, value),
  };
}

const Context = createContext(localeContext("en", "system", () => {}));

export function useLocale() {
  return useContext(Context);
}

export function LocaleProvider({ children }: { children: ReactNode }) {
  const [preference, updatePreference] = useState(readPreference);
  const [system_locale, setSystemLocale] = useState(systemLocale);
  const locale = preference === "system" ? system_locale : preference;
  const setPreference = useCallback((next: LocalePreference) => {
    updatePreference(next);
    try { window.localStorage.setItem(PREFERENCE_KEY, next); }
    catch { /* The selected language still applies when storage is unavailable. */ }
  }, []);
  useEffect(() => {
    const storage = (event: StorageEvent) => {
      if (event.key === PREFERENCE_KEY || event.key === null) updatePreference(readPreference());
    };
    const language = () => setSystemLocale(systemLocale());
    window.addEventListener("storage", storage);
    window.addEventListener("languagechange", language);
    return () => {
      window.removeEventListener("storage", storage);
      window.removeEventListener("languagechange", language);
    };
  }, []);
  useEffect(() => {
    document.documentElement.lang = locale;
  }, [locale]);
  const value = useMemo(() => localeContext(locale, preference, setPreference), [locale, preference, setPreference]);
  return <Context.Provider value={value}>{children}</Context.Provider>;
}
