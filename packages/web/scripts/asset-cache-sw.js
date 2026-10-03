/* global VERSION, ASSETS */
const prefix = `sigma-features:${self.registration.scope}:`;
const cacheName = prefix + VERSION;
const urls = ASSETS.map((path) => new URL(path, self.location.origin).href);
const allowed = new Set(urls);

self.addEventListener("install", (event) => {
  event.waitUntil(
    (async () => {
      const cache = await caches.open(cacheName);
      try {
        // One low-priority download leaves room for tabs the user opens.
        for (const url of urls) {
          const immutable = url.startsWith(self.registration.scope + "assets/");
          const previous = immutable ? await cached(url) : undefined;
          const response =
            previous ||
            (await fetch(url, {
              cache: immutable ? "default" : "no-cache",
              priority: "low",
              signal: AbortSignal.timeout(120000),
            }));
          if (
            !response.ok ||
            /text\/html/i.test(response.headers.get("content-type") || "")
          )
            throw new Error(`Could not cache ${url}`);
          await cache.put(url, response);
        }
      } catch (error) {
        await caches.delete(cacheName);
        throw error;
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
        // Foreground requests bypass the background queue. Only bound the
        // network wait when a cached response is available as a fallback.
        const response = await fetch(request, {
          priority: "high",
          ...(hit ? { signal: AbortSignal.timeout(3000) } : {}),
        });
        if (
          response.ok &&
          !/text\/html/i.test(response.headers.get("content-type") || "")
        ) {
          if (allowed.has(url.href)) {
            const copy = response.clone();
            event.waitUntil(
              caches
                .open(cacheName)
                .then((cache) => cache.put(request, copy))
                .catch(() => {}),
            );
          }
          return response;
        }
        return hit || response;
      } catch (error) {
        if (hit) return hit;
        throw error;
      }
    })(),
  );
});
