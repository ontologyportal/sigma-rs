/* global VERSION, ASSETS */
const prefix = `sigma-features:${self.registration.scope}:`;
const cacheName = prefix + VERSION;
const urls = ASSETS.map((path) => new URL(path, self.location.origin).href);
const allowed = new Set(urls);

self.addEventListener("install", (event) => {
  event.waitUntil(
    (async () => {
      const cache = await caches.open(cacheName);
      let next = 0;
      // Bound concurrent downloads, including the large prover binaries.
      const results = await Promise.allSettled(
        Array.from({ length: 4 }, async () => {
          for (let i = next++; i < urls.length; i = next++) {
            const response = await fetch(urls[i], {
              cache: "reload",
              signal: AbortSignal.timeout(120000),
            });
            if (
              !response.ok ||
              /text\/html/i.test(response.headers.get("content-type") || "")
            )
              throw new Error(`Could not cache ${urls[i]}`);
            await cache.put(urls[i], response);
          }
        }),
      );
      const failure = results.find((result) => result.status === "rejected");
      if (failure) {
        await caches.delete(cacheName);
        throw failure.reason;
      }
    })(),
  );
  // Updates wait for existing tabs to close, preserving their running engine.
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    (async () => {
      for (const name of await caches.keys()) {
        if (name.startsWith(prefix) && name !== cacheName)
          await caches.delete(name);
      }
      await self.clients.claim();
    })(),
  );
});

async function cached(request) {
  // A new build can be installed but waiting while older tabs remain open.
  const names = (await caches.keys()).filter((name) => name.startsWith(prefix));
  for (const name of names.reverse()) {
    const hit = await (await caches.open(name)).match(request);
    if (hit) return hit;
  }
}

self.addEventListener("fetch", (event) => {
  const request = event.request;
  const url = new URL(request.url);
  const base = new URL(self.registration.scope);
  if (
    request.method !== "GET" ||
    request.mode === "navigate" ||
    url.origin !== base.origin ||
    url.search ||
    (!allowed.has(url.href) &&
      !["assets/", "vampire/", "eprover/"].some((dir) =>
        url.pathname.startsWith(base.pathname + dir),
      ))
  )
    return;

  event.respondWith(
    (async () => {
      const hit = await cached(request);
      // Hashed chunks are immutable; stable prover URLs revalidate when online.
      if (hit && url.pathname.startsWith(base.pathname + "assets/")) return hit;
      try {
        const response = await fetch(request, {
          signal: AbortSignal.timeout(3000),
        });
        if (
          response.ok &&
          !/text\/html/i.test(response.headers.get("content-type") || "")
        )
          return response;
        return hit || response;
      } catch (error) {
        if (hit) return hit;
        throw error;
      }
    })(),
  );
});
