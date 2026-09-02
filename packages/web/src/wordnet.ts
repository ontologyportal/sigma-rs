/** Fetching the WordNet-SUMO lexicon mapping files from the upstream SUMO
 *  repo -- the browser-side mirror of `sigmakee_rs_sdk::lexicon::load_http`
 *  (crates/sdk/src/lexicon.rs). Same repo/ref as the main KIF constituents
 *  (`WORDNET_DIR` sits under the same `rawUrl` base as `MERGE`/`MIDLEVEL`),
 *  so a WordNet fetch tracks whatever branch the KIF fetch does.
 *
 *  Fetch-only for now: this returns raw texts. Parsing them into a
 *  `WordNet` happens on the Rust/wasm side once `Session::loadWordNet` (or
 *  equivalent) is wired up -- these texts are exactly its future inputs. */

import { rawUrl, WORDNET_DIR } from './constants.ts';
import { fetchText } from './sources.ts';

/** The four required mapping files, in `(file name, pos)` order -- mirrors
 *  `MAPPING_FILES` in crates/sdk/src/lexicon.rs. */
export const WORDNET_MAPPING_FILES = [
  ['WordNetMappings30-noun.txt', 'noun'],
  ['WordNetMappings30-verb.txt', 'verb'],
  ['WordNetMappings30-adj.txt', 'adj'],
  ['WordNetMappings30-adv.txt', 'adv'],
];

/** The optional companions: sense ordering, then the two irregular
 *  inflection tables. Mirrors `INDEX_SENSE`/`EXC_FILES` in the SDK. */
export const WORDNET_INDEX_SENSE = 'index.sense';
export const WORDNET_EXC_FILES = ['noun.exc', 'verb.exc'];

/**
 * Fetch the four required WordNetMappings30-*.txt files plus the optional
 * index.sense/noun.exc/verb.exc companions from the upstream SUMO repo.
 *
 * A required file's fetch failure propagates (mirrors the SDK's
 * `load_with`: any required-file failure aborts the whole load). An
 * optional companion's failure degrades silently -- `indexSense` is `null`
 * and/or `exceptions` is `''`, same as a missing file in a local
 * WordNetMappings directory.
 */
export async function fetchWordNetMappings() {
  const texts = await Promise.all(
    WORDNET_MAPPING_FILES.map(([name, pos]) =>
      fetchText(rawUrl(`${WORDNET_DIR}/${name}`)).then((text) => [text, pos])),
  );

  const indexSense = await fetchText(rawUrl(`${WORDNET_DIR}/${WORDNET_INDEX_SENSE}`)).catch(() => null);

  const excParts = await Promise.all(
    WORDNET_EXC_FILES.map((name) => fetchText(rawUrl(`${WORDNET_DIR}/${name}`)).catch(() => null)),
  );
  const exceptions = excParts.filter((t) => t !== null).join('\n');

  return { texts, indexSense, exceptions };
}
