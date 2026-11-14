import { LocalizedError, formatText, message, type DisplayText, type Locale } from "../features/i18n/messages";

export interface ErrorDetails {
  code: string | null;
  message: DisplayText;
}

export function errorDetails(error: unknown): ErrorDetails {
  return { code: errorCode(error), message: localizedErrorText(error) };
}

export function errorCode(error: unknown): string | null {
  if (
    typeof error === "object" &&
    error !== null &&
    "code" in error &&
    typeof error.code === "string"
  ) {
    return error.code;
  }
  return null;
}

export function errorMessage(error: unknown, locale: Locale = "en"): string {
  return formatText(locale, localizedErrorText(error));
}

export function localizedErrorText(error: unknown): DisplayText {
  if (error instanceof LocalizedError) return error.text;
  if (typeof error === "string") {
    return error;
  }
  if (error instanceof Error) {
    return error.message;
  }
  if (
    typeof error === "object" &&
    error !== null &&
    "message" in error &&
    typeof error.message === "string"
  ) {
    return error.message;
  }
  return message("shell.unexpected_error");
}
