/**
 * Pulls one component's section out of this app's release notes (see
 * `fetchAppRelease`) and renders it to sanitized HTML for the version
 * dialog. The release body covers Web, CLI, and VSCode in one Markdown
 * document under top-level `###` headings; only the Web section is ever
 * shown here.
 */

import { marked } from "marked";
import DOMPurify from "dompurify";

const HEADING = /^(#{1,6})\s+(.*)$/;

/**
 * The body text of the first Markdown heading matching `title` (case-
 * insensitive), up to the next heading of the same or shallower level, or
 * null if no such heading exists.
 */
export function extractReleaseSection(
  markdown: string,
  title: string,
): string | null {
  const lines = markdown.split(/\r?\n/);
  let start = -1;
  let level = 0;
  for (let i = 0; i < lines.length; i++) {
    const m = lines[i].match(HEADING);
    if (m && m[2].trim().toLowerCase() === title.toLowerCase()) {
      start = i + 1;
      level = m[1].length;
      break;
    }
  }
  if (start === -1) return null;
  let end = lines.length;
  for (let i = start; i < lines.length; i++) {
    const m = lines[i].match(HEADING);
    if (m && m[1].length <= level) {
      end = i;
      break;
    }
  }
  const section = lines.slice(start, end).join("\n").trim();
  return section || null;
}

/** Open every link a rendered release note contains in a new tab, without
 *  leaking an `opener` handle back to this app. */
DOMPurify.addHook("afterSanitizeAttributes", (node) => {
  if (node.tagName === "A") {
    node.setAttribute("target", "_blank");
    node.setAttribute("rel", "noopener noreferrer");
  }
});

/** Markdown to sanitized HTML, safe for `v-html` -- the source is a GitHub
 *  release body fetched at runtime, not bundled content. */
export function renderReleaseNotes(markdown: string): string {
  return DOMPurify.sanitize(marked.parse(markdown, { async: false }));
}
