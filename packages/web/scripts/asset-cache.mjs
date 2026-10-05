import { createHash } from "node:crypto";
import { readdir, readFile, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";

/** Cache shipped feature assets, never documents, APIs, or remote data. */
export function isFeatureAsset(file) {
  return /\.(?:js|mjs|css|wasm|woff2?|ttf|png|svg|ico|gif)$/.test(file);
}

/** Keep optional prover binaries and graph libraries on demand. */
export function isPrecachedAsset(file) {
  return (
    isFeatureAsset(file) &&
    !/^(?:vampire|eprover)\//.test(file) &&
    !/^assets\/cytoscape[^/]*\.js$/.test(file)
  );
}

async function filesIn(dir, prefix = "") {
  const files = [];
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    const name = prefix + entry.name;
    if (entry.isDirectory())
      files.push(...(await filesIn(join(dir, entry.name), name + "/")));
    else if (isFeatureAsset(name) && name !== "asset-cache-sw.js")
      files.push(name);
  }
  return files.sort();
}

/** Precache editing assets; allow optional tools to be cached on first use. */
export default function assetCache() {
  let config;
  return {
    name: "sigma-feature-cache",
    apply: "build",
    configResolved(value) {
      config = value;
    },
    async closeBundle() {
      const dir = resolve(config.root, config.build.outDir);
      const files = await filesIn(dir);
      const hash = createHash("sha256");
      const source = await readFile(
        new URL("./asset-cache-sw.js", import.meta.url),
        "utf8",
      );
      hash.update(source);
      for (const file of files) {
        hash.update(file);
        hash.update(await readFile(join(dir, file)));
      }
      const version = hash.digest("hex").slice(0, 20);
      await writeFile(
        join(dir, "asset-cache-sw.js"),
        `const VERSION = ${JSON.stringify(version)};\nconst ASSETS = ${JSON.stringify(files.map((file) => config.base + file))};\nconst PRECACHE = ${JSON.stringify(files.filter(isPrecachedAsset).map((file) => config.base + file))};\n${source}`,
      );
    },
  };
}
