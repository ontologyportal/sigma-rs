// Load the SUMO KB into the wasm engine in Node, the way the web app does,
// with the web app's default prover profile.

import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { init, Session, Source, Config } from "sigmakee/sdk";

const WASM = fileURLToPath(
  new URL("../../../sigmakee/dist/engine_bg.wasm", import.meta.url),
);

/** The KB directory and constituent files from `~/.sigmakee/config.xml`
 *  (the CLI's config), or null when there is none. */
export function configuredKb() {
  const path = join(homedir(), ".sigmakee", "config.xml");
  if (!existsSync(path)) return null;
  const xml = readFileSync(path, "utf8");
  const dir = xml.match(/<preference name="kbDir" value="([^"]*)"/)?.[1];
  const tests = xml.match(
    /<preference name="inferenceTestDir" value="([^"]*)"/,
  )?.[1];
  const files = [...xml.matchAll(/<constituent filename="([^"]+)"/g)].map(
    (m) => m[1],
  );
  return dir ? { dir, tests, files } : null;
}

/** The web app's Ask profile (src/utils/proverOptions.ts defaultProfile). */
export function webConfig(timeLimitSecs) {
  const c = new Config();
  c.wantProof = true;
  c.timeLimitSecs = timeLimitSecs;
  c.maxSteps = 4000;
  c.maxLits = 8;
  c.forwardClose = true;
  c.selectionTimeLimitSecs = 10;
  c.auditSubsetLimit = 20;
  return c;
}

/** A session over `files` (names under `dir`) plus each file's text. */
export async function loadKb({
  dir,
  files,
  timeLimitSecs = 30,
  log = () => {},
}) {
  await init({ module_or_path: readFileSync(WASM) });
  const session = new Session({ config: webConfig(timeLimitSecs) });
  const texts = new Map();
  const t0 = Date.now();
  for (const f of files) {
    const path = join(dir, f);
    if (!existsSync(path)) {
      log(`skip missing ${path}`);
      continue;
    }
    const text = readFileSync(path, "utf8");
    texts.set(f, text);
    await session.ingest(Source.kif(text, f));
  }
  log(
    `loaded ${texts.size} files in ${((Date.now() - t0) / 1000).toFixed(1)}s`,
  );
  const language =
    session.naturalLanguages().find((l) => /^English/.test(l.symbol))?.symbol ??
    "EnglishLanguage";
  return { session, texts, language, Source };
}

/** Replace one file's text in place (incremental re-ingest). */
export async function reingest(kb, file, text) {
  await kb.session.ingest(kb.Source.kif(text, file));
}
