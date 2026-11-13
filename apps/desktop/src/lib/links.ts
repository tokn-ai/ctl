/** Do not let URL parsers repair a malformed displayed web address. */
export function validWebUrl(value: string): boolean {
  if (value.trim() !== value || /[\u0000-\u001f\u007f-\u009f]/.test(value)) return false;
  const authority = /^https?:\/\/([^/?#]*)/i.exec(value)?.[1];
  if (!authority || authority.includes("\\")) return false;
  try {
    const url = new URL(value);
    return (url.protocol === "http:" || url.protocol === "https:") && !!url.hostname;
  } catch { return false; }
}
