/** Warm every deployed feature asset before startup completes. */
export async function cacheFeatureAssets(): Promise<void> {
  // Vite's live module graph must not be persisted across source edits.
  if (!import.meta.env.PROD) return;
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    await Promise.race([
      installFeatureCache(),
      new Promise<never>((_, reject) => {
        timer = setTimeout(
          () => reject(new Error("Feature caching timed out.")),
          180000,
        );
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

async function installFeatureCache(): Promise<void> {
  if (!("serviceWorker" in navigator))
    throw new Error("This browser cannot cache the application's features.");

  const base = import.meta.env.BASE_URL;
  const scriptUrl = new URL(`${base}asset-cache-sw.js`, location.href).href;
  const registration = await navigator.serviceWorker.register(scriptUrl, {
    scope: base,
    updateViaCache: "none",
  });
  const worker = registration.installing;
  if (worker) {
    await new Promise<void>((resolve, reject) => {
      const check = () => {
        if (worker.state === "redundant") {
          worker.removeEventListener("statechange", check);
          reject(new Error("The feature download did not finish."));
        } else if (
          ["installed", "activating", "activated"].includes(worker.state)
        ) {
          worker.removeEventListener("statechange", check);
          resolve();
        }
      };
      worker.addEventListener("statechange", check);
      check();
    });
  }
  await navigator.serviceWorker.ready;
  if (navigator.serviceWorker.controller?.scriptURL !== scriptUrl) {
    await new Promise<void>((resolve) => {
      const check = () => {
        if (navigator.serviceWorker.controller?.scriptURL !== scriptUrl) return;
        navigator.serviceWorker.removeEventListener("controllerchange", check);
        resolve();
      };
      navigator.serviceWorker.addEventListener("controllerchange", check);
      check();
    });
  }
}
