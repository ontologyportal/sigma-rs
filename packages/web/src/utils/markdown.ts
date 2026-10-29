/**
 * Untrusted markdown to sanitized HTML: `marked` then `DOMPurify`, with links
 * opened in a new tab and images replaced by links.
 */

import { marked } from "marked";
import DOMPurify from "dompurify";

/** Open every link rendered markdown contains in a new tab, without
 *  leaking an `opener` handle back to this app. */
DOMPurify.addHook("afterSanitizeAttributes", (node) => {
  if (node.tagName === "A") {
    node.setAttribute("target", "_blank");
    node.setAttribute("rel", "noopener noreferrer");
  }
});

/**
 * Swaps every image for a new-tab link to it, labeled by its alt text.
 * Remote images (e.g. GitHub's release attachments) send no CORP or CORS headers, so this page's
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

/** Markdown to sanitized HTML, safe for `v-html`, for text fetched at
 *  runtime rather than bundled: GitHub release bodies, model-written proof
 *  explanations. */
export function renderMarkdown(markdown: string): string {
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
