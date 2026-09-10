/** Small formatting helpers shared by every view. Imports nothing. */

/** HTML-escape `s` for text content (`&`, `<`, `>`). */
export const esc = (s: unknown): string =>
  String(s).replace(
    /[&<>]/g,
    (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;" })[c],
  );

/** HTML-escape `s` for a double-quoted attribute value. */
export const escAttr = (s: unknown): string => esc(s).replace(/"/g, "&quot;");

/** `n` with locale thousands separators. */
export const fmtNum = (n: unknown): string => Number(n).toLocaleString();

/** `d` as a short locale date ("Jan 5, 2026"). */
export const fmtDate = (d: Date): string =>
  d.toLocaleDateString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
  });

/** `bytes` as a human-readable size -- KB for anything under 1 MB (matching
 *  the loaded-constituent list's units), MB above that: the mapping files
 *  run into the tens of megabytes, where an all-KB number is unreadable. */
export function formatSize(bytes: number): string {
  return bytes >= 1e6
    ? `${(bytes / 1e6).toFixed(1)} MB`
    : `${Math.round(bytes / 1000)} KB`;
}

/** Trigger a real browser download of `text` as `name`. */
export function downloadText(name: string, text: string): void {
  const blob = new Blob([text], { type: "text/plain" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
}

/** The message of a thrown value, whatever its shape. */
export function errMsg(e: unknown): string {
  return String((e as { message?: unknown })?.message ?? e);
}
