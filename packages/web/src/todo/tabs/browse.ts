/** Browse (Home) tab: search → results → man page, plus the `/` and Esc
 *  shortcuts that drive it.
 *
 * The results/man-page markup stays built as HTML strings (unchanged from
 * before this module went reactive) rather than re-templated as Vue
 * `v-for`/`v-if` -- the man page in particular has enough conditional,
 * deeply nested structure that a from-scratch re-templating would be a much
 * bigger, harder-to-verify change than swapping its DOM sink for a ref.
 * `browseViewHtml`/`browseHomeVisible` are that sink; `BrowseTab.vue` just
 * `v-html`s the former and `v-show`s the latter. Because a `v-html` patch is
 * applied on Vue's next tick rather than synchronously, `openManPage`
 * `await`s `nextTick()` before doing any querySelector-based wiring
 * (subtabs, the ref filter, the taxonomy graph) against what it just set.
 *
 * The two purely self-contained click/change handlers (the WordNet-only
 * checkbox, a click inside the rendered results/man page) live in
 * `BrowseTab.vue` directly -- nothing outside that component calls them,
 * and unlike `onSearchInput`/`onClearClick` they don't share any private
 * helper (`updateSearchClear`/`clearSearch`) with code that does. */

import { nextTick, ref } from "vue";
import { state } from "../state.ts";
import { call } from "../rpc.ts";
import { $, esc, escAttr, targetEl, isDarkTheme } from "../dom.ts";
import { kifCiteRow } from "../proof-view.ts";
import { taxonomyWidget, fillAncestors } from "./taxonomy.ts";
import { currentTab, navigate, updateParams } from "../router.ts";

export const browseViewHtml = ref("");
export const browseHomeVisible = ref(true);
export const showSearchClear = ref(false);

/** Show the clear button only once there's something to clear. */
function updateSearchClear() {
  showSearchClear.value = !!$("q").value;
}

/** Reset the search box and results back to the tab's empty state, without
 *  touching the URL — for navigating to Browse from elsewhere (nav tab,
 *  the header logo), where the router already owns the address bar. */
export function resetBrowseView() {
  setBrowseQuery("");
  updateSearchClear();
  searchSeq++;
  setBrowseHome(true);
}

/** Same, but also clears the URL's `q` — for the clear button and the Esc
 *  shortcut below, neither of which goes through the router. */
function clearSearch() {
  resetBrowseView();
  updateParams({});
  focusSearch();
}

export function onSearchSubmit(e: SubmitEvent) {
  e.preventDefault();
  const q = $("q").value.trim();
  updateParams({ q });
  runSearch(q);
}

export function onClearClick() {
  clearSearch();
}

// Search-as-you-type: results render live off a short debounce; Enter still
// works via the submit handler above (same runSearch, seq-guarded below).
let searchDebounce = 0;
export function onSearchInput() {
  updateSearchClear();
  clearTimeout(searchDebounce);
  searchDebounce = setTimeout(() => {
    const q = $("q").value.trim();
    updateParams({ q });
    runSearch(q);
  }, 150);
}

// Advanced search options. Not persisted (session-only, like the language
// selector) — re-read fresh on every search rather than mirrored into
// `state`, since `runSearch` is the only reader. Re-runs the current query
// immediately on change so toggling it doesn't require re-typing.
function searchOpts() {
  return { wordnetOnly: $("searchWordnetOnly")?.checked ?? false };
}

/** Show/hide the Browse tab's empty state (welcome card + KB stats). */
export function setBrowseHome(show) {
  browseHomeVisible.value = show;
  if (show) browseViewHtml.value = "";
}

// The welcome card's example queries.
document.addEventListener("click", (e) => {
  const a = targetEl(e).closest<HTMLElement>("a.try-q");
  if (!a) return;
  e.preventDefault();
  setBrowseQuery(a.textContent);
  updateSearchClear();
  updateParams({ q: a.textContent });
  runSearch(a.textContent);
});

// Only the newest in-flight search may render: typing "proc" fires four
// requests and the slower ones must not clobber the freshest results.
let searchSeq = 0;

export async function runSearch(query) {
  updateSearchClear();
  if (!query) {
    setBrowseHome(true);
    return;
  }
  const seq = ++searchSeq;
  setBrowseHome(false);
  const opts = searchOpts();
  let { hits } = await call("search", {
    query,
    limit: 100,
    language: state.uiLanguage,
    ...opts,
  });
  if (seq !== searchSeq) return;
  // The language filter is a preference, not a wall: a KB documented in
  // another language must stay searchable on the default (English) setting.
  let langNote = "";
  if (hits.length === 0) {
    ({ hits } = await call("search", { query, limit: 100, ...opts }));
    if (seq !== searchSeq) return;
    if (hits.length)
      langNote = ` (no ${esc(langLabel(state.uiLanguage))} matches — showing all languages)`;
  }
  if (hits.length === 0) {
    const wnHint = opts.wordnetOnly
      ? ` (WordNet only — try unchecking Advanced search, or confirm the lexicon is enabled under <a class="jump" data-tab="kb">Knowledge base</a>)`
      : "";
    browseViewHtml.value =
      `<div class="card hint">No matches for <code>${esc(query)}</code>${wnHint}.</div>`;
    return;
  }
  const items = hits
    .map(
      (h) => `
    <li>
      <a class="sym open" data-sym="${esc(h.symbol)}">${esc(h.symbol)}</a>
      <span class="kinds">${h.kinds.join(" · ") || h.source}${h.sense ? ` · ${esc(h.sense)}` : ""} · <span class="rank" title="${escAttr(rankTooltip(h))}">rank ${h.rank.toFixed(0)}</span></span>
      ${h.text ? `<div class="snippet">${boldifyDoc(h.text)}</div>` : ""}
      ${(() => {
        const mappings = relevantWordnet(h.wordnet, query);
        return mappings.length ? wordnetBlock(mappings) : "";
      })()}
    </li>`,
    )
    .join("");
  browseViewHtml.value = `<div class="card">
       <div class="hint" style="margin-bottom:6px">${hits.length} result${hits.length === 1 ? "" : "s"} for <code>${esc(query)}</code>${langNote}</div>
       <ul class="results">${items}</ul>
     </div>`;
}

/** Plain-text breakdown of a search hit's rank score, one labeled
 *  contribution per line plus the total -- rendered as a native `title`
 *  tooltip on hover (see runSearch's `rank` span). */
function rankTooltip(hit) {
  const lines = hit.rank_breakdown.map(
    (c) => `${c.label}: ${c.value >= 0 ? "+" : ""}${c.value.toFixed(1)}`,
  );
  lines.push(`= ${hit.rank.toFixed(1)}`);
  return lines.join("\n");
}

/** Human label for a language symbol from the header selector, falling back
 *  to the raw symbol name. */
export function langLabel(symbol) {
  const opt = [...($("langSelect")?.options ?? [])].find(
    (o) => o.value === symbol,
  );
  return opt ? opt.textContent : symbol;
}

/** Render a symbol's WordNet mappings (see `SearchHit.wordnet` /
 *  `ManPage.wordnet`) — one entry per anchored synset, spelled out as
 *  `"dog" (n) — subsuming mapping` plus its gloss, rather than the compact
 *  `dog#n#1+` sense tag. Used both under the man page's Documentation
 *  section and inline under a search-result snippet, for any symbol that
 *  has a mapping regardless of how it was found or whether "WordNet only"
 *  is on. */
/** Narrow a symbol's full WordNet mapping list down to the entries a search
 *  query actually matched -- a highly-connected symbol like `Canine` can
 *  carry 50+ mappings, and `DomesticDog` 170+, most of which are irrelevant
 *  to any one query. Matches only a whole lemma in the mapping's word list,
 *  case-insensitively -- "dog" matches the "dog" entry but not "sheep dog" or
 *  "domestic dog" -- so a substring hit inside a longer phrase doesn't drag
 *  in an unrelated mapping. A query that hits none of them (the symbol
 *  surfaced via its documentation or name instead) yields no inline mappings
 *  here -- the full list is still one click away on the man page's WordNet
 *  tab. */
function relevantWordnet(mappings, query) {
  const q = query.trim().toLowerCase();
  if (!q) return [];
  return mappings.filter((m) =>
    m.words
      .toLowerCase()
      .split(",")
      .some((w) => w.trim() === q),
  );
}

function wordnetBlock(mappings) {
  return mappings
    .map(
      (m) => `
    <div class="wn-entry">
      <div>"${esc(m.words)}" (${esc(m.pos)}) — ${esc(m.mapping)} mapping</div>
      <div class="hint">${esc(m.gloss)}</div>
    </div>`,
    )
    .join("");
}

/** Render `&%Symbol` cross-reference markers as bold plain text (no link) —
 *  used in search-result snippets, where each row already links to the symbol. */
function boldifyDoc(text) {
  return String(text)
    .split(/(&%[A-Za-z0-9_-]+)/)
    .map((part) => {
      const m = part.match(/^&%([A-Za-z0-9_-]+)$/);
      return m ? `<b>${esc(m[1])}</b>` : esc(part);
    })
    .join("");
}

/** Turn `&%Symbol` cross-reference markers in documentation text into man-page links. */
function linkifyDoc(text) {
  return String(text)
    .split(/(&%[A-Za-z0-9_-]+)/)
    .map((part) => {
      const m = part.match(/^&%([A-Za-z0-9_-]+)$/);
      return m
        ? `<a class="open xref" data-sym="${esc(m[1])}">${esc(m[1])}</a>`
        : esc(part);
    })
    .join("");
}

/** A bare symbol name (no `&%` marker — the Signature block's arg/range
 *  types are already just class names) as the same man-page link linkifyDoc
 *  produces, so they're clickable like every other cross-reference on the
 *  page instead of inert text. */
function linkifySymbol(symbol) {
  return `<a class="open xref" data-sym="${esc(symbol)}">${esc(symbol)}</a>`;
}

/** Doc entries in the selected language, falling back to English then to all,
 *  so a symbol never renders blank just because it lacks the chosen language. */
function docsForLanguage(entries) {
  const pick = (lang) => entries.filter((d) => d.language === lang);
  return pick(state.uiLanguage).length
    ? pick(state.uiLanguage)
    : pick("EnglishLanguage").length
      ? pick("EnglishLanguage")
      : entries;
}

export async function openManPage(symbol) {
  const { page: p } = await call("manpage", { symbol });
  setBrowseHome(false);
  if (!p) {
    browseViewHtml.value =
      `<div class="card hint">No man page for <code>${esc(symbol)}</code>.</div>`;
    return;
  }

  const docs = (v) =>
    docsForLanguage(v)
      .map(
        (d) =>
          `<div>${linkifyDoc(d.text)} <span class="hint">(${esc(d.language)})</span></div>`,
      )
      .join("");
  const sig = () => {
    const parts = [];

    if (p.arity != null)
      parts.push(`arity ${p.arity < 0 ? 'variable' : p.arity}`);
    for (const d of p.domains)
      parts.push(
        `arg ${d.position}: ${linkifySymbol(d.sort.class)}${d.sort.subclass ? " (class)" : ""}`,
      );
    if (p.range)
      parts.push(
        `${p.range.subclass ? "rangeSubclass" : "range"}: ${linkifySymbol(p.range.class)}`,
      );
    return parts.length
      ? parts.join("<br>")
      : '<span class="hint">none declared</span>';
  };
  const field = (title, html) =>
    `<div class="field"><h3>${title}</h3><div class="val">${html}</div></div>`;

  const refsNote = (p) => {
    const shown = p.references.length;
    const omitted = Math.max(0, p.appears_in_count - shown);
    if (!shown) {
      return omitted
        ? `appears only in ${omitted} documentation/taxonomy/format entr${omitted === 1 ? "y" : "ies"} (not shown)`
        : "appears in no formulas";
    }
    const excl = omitted
      ? ` (${omitted} documentation/taxonomy/format entr${omitted === 1 ? "y" : "ies"} omitted)`
      : "";
    return `appears in ${shown} formula${shown === 1 ? "" : "s"}${excl}, listed below`;
  };
  const refsBlock = p.references.length
    ? `<div class="hint" style="margin-bottom:4px">${refsNote(p)}</div>
       ${refFilterControl(p.references)}
       <div id="refListWrap">${renderRefList(p.references, "", p.name)}</div>`
    : `<div class="hint">${refsNote(p)}</div>`;

  const hasWordnet = p.wordnet.length > 0;
  const subtabs = hasWordnet
    ? `<div class="man-subtabs" role="tablist">
         <button type="button" data-subtab="overview" aria-selected="true">Overview</button>
         <button type="button" data-subtab="wordnet">WordNet <span class="hint">(${p.wordnet.length})</span></button>
       </div>`
    : "";

  browseViewHtml.value = `
    <div class="card man">
      <div class="man-head">
        <a class="hint back" style="cursor:pointer">← back to results</a>
        <h2>${esc(p.name)}</h2>
        <div class="kinds">${p.kinds.join(" · ") || "symbol"}</div>
        ${subtabs}
      </div>
      <div data-man-panel="overview">
        ${p.documentation.length ? field("Documentation", docs(p.documentation)) : ""}
        ${field("Taxonomy", taxonomyWidget(p))}
        ${p.arity != null || p.domains.length || p.range ? field("Signature", sig()) : ""}
        ${p.term_format.length ? field("Term format", docs(p.term_format)) : ""}
        ${p.format.length ? field("Format", docs(p.format)) : ""}
        ${field("References", refsBlock)}
      </div>
      ${hasWordnet ? `<div data-man-panel="wordnet" hidden>${wordnetBlock(p.wordnet)}</div>` : ""}
    </div>`;

  // `v-html` patches the DOM on Vue's next tick, not synchronously -- wait
  // for it before querying what was just set.
  await nextTick();

  $("browseView").querySelector(".back").onclick = () =>
    runSearch($("q").value.trim());
  const refSel = $("refFilter");
  if (refSel)
    refSel.onchange = () => {
      $("refListWrap").innerHTML = renderRefList(
        p.references,
        refSel.value,
        p.name,
      );
    };
  if (hasWordnet) {
    const buttons = [
      ...$<HTMLElement>("browseView").querySelectorAll<HTMLElement>(
        "[data-subtab]",
      ),
    ];
    for (const btn of buttons) {
      btn.onclick = () => {
        for (const b of buttons)
          b.setAttribute("aria-selected", String(b === btn));
        for (const panel of $<HTMLElement>(
          "browseView",
        ).querySelectorAll<HTMLElement>("[data-man-panel]"))
          panel.hidden = panel.dataset.manPanel !== btn.dataset.subtab;
      };
    }
  }
  fillAncestors(p); // fire-and-forget: the chain above the symbol streams in
}

// -- The man page's reference list --------------------------------------------

/** Reference-filter <select>, offering only the categories present among `refs`.
 *  Value encoding: '' = all; 'fact' = any plain fact (a relation atom, possibly
 *  under `not`); 'fact:<n>' = plain fact with the symbol at argument n (0 = the
 *  relation itself); '=>' '<=>' 'and' 'or' = a top-level logical operator.
 *  `kind`/`arg_pos` come from the core classification in `manpage_to_js`. */
function refFilterControl(refs) {
  const facts = refs.filter((r) => r.kind === "fact");
  const positions = [
    ...new Set(facts.map((r) => r.arg_pos).filter((n) => n != null)),
  ].sort((a: number, b: number) => a - b);
  const ops = [
    ["=>", "Implications (⇒)"],
    ["<=>", "Biconditionals (⇔)"],
    ["and", "Conjunctions (and)"],
    ["or", "Disjunctions (or)"],
  ].filter(([k]) => refs.some((r) => r.kind === k));
  const opt = (v, label) => `<option value="${esc(v)}">${esc(label)}</option>`;
  const posLabel = (n) =>
    n === 0 ? "symbol as the relation (arg 0)" : `symbol as argument ${n}`;
  const count = (pred) => refs.filter(pred).length;
  return `<label class="ref-filter"><span class="hint">Filter</span>
    <select id="refFilter">
      ${opt("", `All (${refs.length})`)}
      ${facts.length ? opt("fact", `Plain facts (${facts.length})`) : ""}
      ${positions.map((n) => opt(`fact:${n}`, `  ${posLabel(n)} (${count((r) => r.kind === "fact" && r.arg_pos === n)})`)).join("")}
      ${ops.map(([k, label]) => opt(k, `${label} (${count((r) => r.kind === k)})`)).join("")}
    </select></label>`;
}

/** Subset of `refs` matching an encoded filter value (see refFilterControl). */
function filterRefs(refs, filter) {
  if (!filter) return refs;
  if (filter === "fact") return refs.filter((r) => r.kind === "fact");
  if (filter.startsWith("fact:")) {
    const n = Number(filter.slice(5));
    return refs.filter((r) => r.kind === "fact" && r.arg_pos === n);
  }
  return refs.filter((r) => r.kind === filter);
}

// Formulas from these files are shown first, in this order, then everything
// else in its existing relative order.
const FILE_SORT_PRIORITY = ["Merge.kif", "Mid-level-ontology.kif"];

function fileSortRank(file) {
  const i = FILE_SORT_PRIORITY.indexOf(file);
  return i === -1 ? FILE_SORT_PRIORITY.length : i;
}

/** Stable sort of refs by source-file priority: Merge.kif, then MILO, then
 *  everything else. */
function sortRefsByFile(refs) {
  return refs
    .map((r, i) => [r, i])
    .sort(
      ([a, ai], [b, bi]) =>
        fileSortRank(a.file) - fileSortRank(b.file) || ai - bi,
    )
    .map(([r]) => r);
}

/** The filtered <ol> of reference rows for man-page subject `name`. */
function renderRefList(refs, filter, name) {
  const shown = sortRefsByFile(filterRefs(refs, filter));
  if (!shown.length)
    return '<span class="hint">no formulas match this filter</span>';
  const rows = shown
    .map((r) =>
      kifCiteRow({ kif: r.kif, file: r.file, line: r.line, focusSymbol: name }),
    )
    .join("");
  return `<ol class="refs">${rows}</ol>`;
}

// -- Keyboard shortcuts: `/` focuses search, Esc backs out of a man page ------

document.addEventListener("keydown", (e) => {
  const t = e.target;
  const typing =
    t instanceof HTMLInputElement ||
    t instanceof HTMLTextAreaElement ||
    t instanceof HTMLSelectElement ||
    (t instanceof HTMLElement && t.isContentEditable);
  if (e.key === "/" && !e.ctrlKey && !e.metaKey && !e.altKey && !typing) {
    e.preventDefault();
    navigate("browse");
    $("q").focus();
    $("q").select();
  } else if (
    e.key === "Escape" &&
    currentTab() === "browse" &&
    (!typing || t === $("q"))
  ) {
    escapeBrowse(t === $("q"));
  }
});

function escapeBrowse(inSearch: boolean) {
  const params = new URLSearchParams(location.search);
  const q = params.get("q") || $("q").value.trim();
  if (params.get("sym")) {
    updateParams({ q });
    if (q) runSearch(q);
    else setBrowseHome(true);
  } else if (inSearch && $("q").value) {
    clearSearch();
  }
}
