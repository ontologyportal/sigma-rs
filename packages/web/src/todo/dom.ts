/** Small DOM/formatting helpers shared by every view. Imports nothing. */

// Generic, defaulting to `any`: callers treat the result as whatever
// element they know it to be (button, input, select, ...), exactly as
// they did before this file had types. Opt into real narrowing at a call
// site with `$<HTMLInputElement>('id')`.
export const $ = <T extends HTMLElement = any>(id: string): T => document.getElementById(id) as T;

/** `e.target` narrowed to `HTMLElement`, for delegated-click patterns
 *  (`targetEl(e).closest(...)`). `Event.target` is typed `EventTarget | null`,
 *  which lacks Element methods — this is the same escape hatch as `$`. */
export const targetEl = (e: Event): HTMLElement => e.target as HTMLElement;
export const esc = (s) => String(s).replace(/[&<>]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;' }[c]));
export const escAttr = (s) => esc(s).replace(/"/g, '&quot;');
export const fmtNum = (n) => Number(n).toLocaleString();
export const fmtDate = (d) => d.toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' });

/** `true` when the page currently renders dark — the explicit header choice
 *  wins, the OS preference is the fallback. */
export function isDarkTheme() {
  const t = document.documentElement.dataset.theme;
  if (t) return t === 'dark';
  return window.matchMedia?.('(prefers-color-scheme: dark)').matches ?? false;
}

/** Disclosure panel: flip (or force) visibility and keep aria-expanded paired
 *  with it, so the two never drift apart. */
export function togglePanel(btnId: string, panelId: string, force?: boolean) {
  const panel = $(panelId);
  const open = force !== undefined ? force : panel.hidden;
  panel.hidden = !open;
  $(btnId).setAttribute('aria-expanded', String(open));
  return open;
}

/** Run `fn` with `button` disabled and labelled "Working…", reporting failures
 *  in the Knowledge base tab's log line. */
export async function withBusy(button, fn) {
  const prev = button.textContent;
  button.disabled = true; button.textContent = 'Working…';
  try { await fn(); }
  catch (e) { $('kbLog').textContent = String(e && e.message || e); $('kbLog').style.color = 'var(--bad)'; }
  finally { button.disabled = false; button.textContent = prev; }
}

/** Trigger a real browser download of `text` as `name`. */
export function downloadText(name, text) {
  const blob = new Blob([text], { type: 'text/plain' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url; a.download = name;
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
}
