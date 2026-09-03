/** Fetching the WordNet-SUMO lexicon mapping files from the upstream SUMO
 *  repo -- the browser-side mirror of `sigmakee_rs_sdk::lexicon::load_http`
 *  (crates/sdk/src/lexicon.rs). Same repo/ref as the main KIF constituents
 *  (`WORDNET_DIR` sits under the same `rawUrl` base as `MERGE`/`MIDLEVEL`),
 *  so a WordNet fetch tracks whatever branch the KIF fetch does.
 **/

import { rawUrl, WORDNET_DIR } from "./constants.ts";

async function fetchText(url) {
  const r = await fetch(url);
  if (!r.ok) throw new Error(`${url}: HTTP ${r.status}`);
  return r.text();
}

/** The four required mapping files, in `(file name, pos)` order -- mirrors
 *  `MAPPING_FILES` in crates/sdk/src/lexicon.rs. */
export const WORDNET_MAPPING_FILES = [
  ["WordNetMappings30-noun.txt", "noun"],
  ["WordNetMappings30-verb.txt", "verb"],
  ["WordNetMappings30-adj.txt", "adj"],
  ["WordNetMappings30-adv.txt", "adv"],
];

/** The optional companions: sense ordering, then the two irregular
 *  inflection tables. Mirrors `INDEX_SENSE`/`EXC_FILES` in the SDK. */
export const WORDNET_INDEX_SENSE = "index.sense";
export const WORDNET_EXC_FILES = ["noun.exc", "verb.exc"];

/** Byte size of `text` as UTF-8 -- `.length` undercounts any non-ASCII
 *  content (rare in these files, but the glosses do carry the occasional
 *  accented name or symbol), unlike the KIF constituent list's `.length`
 *  shortcut (KIF is ASCII-only per AGENTS.md, so the two never diverge
 *  there). */
function byteSize(text) {
  return new TextEncoder().encode(text).length;
}

/**
 * Fetch the four required WordNetMappings30-*.txt files plus the optional
 * index.sense/noun.exc/verb.exc companions from the upstream SUMO repo.
 *
 * A required file's fetch failure propagates (mirrors the SDK's
 * `load_with`: any required-file failure aborts the whole load). An
 * optional companion's failure degrades silently -- `indexSense` is `null`
 * and/or `exceptions` is `''`, same as a missing file in a local
 * WordNetMappings directory.
 *
 * `files` lists every file actually fetched (name + byte size), in fetch
 * order -- for display (see tabs/kb-tab.ts's WordNet panel), independent of
 * `texts`/`indexSense`/`exceptions`' shape (the two `.exc` files are
 * concatenated into one `exceptions` string there, losing their individual
 * sizes, so `files` is the only place those survive separately).
 */
export async function fetchWordNetMappings() {
  // Concurrent fetches settle in arrival order, not declaration order --
  // `Promise.all` preserves the latter for its own array, so `files` is
  // built from each group's already-ordered result rather than a shared
  // array mutated from scattered `.then()` callbacks.
  const texts = await Promise.all(
    WORDNET_MAPPING_FILES.map(([name, pos]) =>
      fetchText(rawUrl(`${WORDNET_DIR}/${name}`)).then((text) => [
        text,
        pos,
        name,
      ]),
    ),
  );
  const mappingFiles = texts.map(([text, , name]) => ({
    name,
    size: byteSize(text),
  }));

  const indexSenseText = await fetchText(
    rawUrl(`${WORDNET_DIR}/${WORDNET_INDEX_SENSE}`),
  ).catch(() => null);
  const indexSenseFile =
    indexSenseText === null
      ? []
      : [{ name: WORDNET_INDEX_SENSE, size: byteSize(indexSenseText) }];

  const excResults = await Promise.all(
    WORDNET_EXC_FILES.map((name) =>
      fetchText(rawUrl(`${WORDNET_DIR}/${name}`))
        .then((text) => ({ name, text }))
        .catch(() => null),
    ),
  );
  const excHits = excResults.filter((r) => r !== null);
  const excFiles = excHits.map(({ name, text }) => ({
    name,
    size: byteSize(text),
  }));
  const exceptions = excHits.map((r) => r.text).join("\n");

  return {
    texts: texts.map(([text, pos]) => [text, pos]),
    indexSense: indexSenseText,
    exceptions,
    files: [...mappingFiles, ...indexSenseFile, ...excFiles],
  };
}
