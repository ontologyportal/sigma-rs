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

/**
 * Swaps every image for a new-tab link to it, labeled by its alt text.
 * GitHub's release attachments send no CORP or CORS headers, so this page's
 * `Cross-Origin-Embedder-Policy: require-corp` blocks them as `<img>`s.
 */
function imagesToLinks(root: DocumentFragment): void {
  for (const img of root.querySelectorAll("img")) {
    const src = img.getAttribute("src") ?? "";
    if (!/^https?:\/\//i.test(src)) {
      img.remove();
      continue;
    }
    const link = document.createElement("a");
    link.href = src;
    link.target = "_blank";
    link.rel = "noopener noreferrer";
    link.textContent = `View image: ${img.alt || src}`;
    img.replaceWith(link);
  }
}

/** Markdown to sanitized HTML, safe for `v-html` -- the source is a GitHub
 *  release body fetched at runtime, not bundled content. */
export function renderReleaseNotes(markdown: string): string {
  const fragment = DOMPurify.sanitize(
    marked.parse(markdown, { async: false }),
    {
      RETURN_DOM_FRAGMENT: true,
    },
  );
  imagesToLinks(fragment);
  const container = document.createElement("div");
  container.append(fragment);
  return container.innerHTML;
}
