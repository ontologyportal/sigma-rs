/** Startup: boots the worker, opens OPFS, installs WordNet, then fetches +
 *  ingests every saved constituent -- driving the LoadingScreen's progress
 *  bar until the app is usable. */

import { defineStore } from "pinia";
import { call } from "../services/sigma";
import { BASE } from "../constants";
import { useKBStore } from "./kb";
import { useWordNetStore } from "./wordnet";

export const useBootStore = defineStore("boot", {
  state: () => ({
    /** True once the fetch+ingest loop has finished and the app is usable
     *  (promote+validate may still be running in the background). */
    finished: false,
    /** True if boot threw -- `error` then holds the message to show. */
    failed: false,
    error: "",
    msg: "Starting the engine...",
    step: 0,
    total: 1,
    /** OPFS root, opened once at boot. */
    opfsRoot: null as FileSystemDirectoryHandle | null,
  }),
  actions: {
    progress(msg: string) {
      this.msg = msg;
      this.step += 1;
    },
    resetProgress(total: number) {
      this.step = 0;
      this.total = Math.max(1, total);
    },

    /** Boot the worker's wasm engine. The worker resolves the optional
     *  Vampire runner against this URL; its own file sits in the bundle's
     *  asset directory, so it cannot derive the base itself. */
    async bootWorker() {
      await call("boot", { baseUrl: new URL(BASE, location.href).href });
    },

    async start() {
      const kb = useKBStore();
      const wordnet = useWordNetStore();
      try {
        this.msg = "Starting the engine...";
        await this.bootWorker();
        this.opfsRoot = await navigator.storage.getDirectory();

        // One progress step per saved constituent, plus two for WordNet
        // (fetch, then install).
        this.resetProgress(kb.saved.length + 2);

        this.progress("Fetching WordNet lexicon...");
        await wordnet.install();
        this.progress("Loading WordNet lexicon...");

        await kb.loadSavedConstituents((name, i, total) => {
          this.progress(`Fetching ${name} (${i}/${total})`);
        });

        this.finished = true;
        // Fire-and-forget: toast -> promote all -> validate -> untoast, off
        // the critical path -- the app is usable (browse/search) before the
        // KB is fully axiomatized.
        kb.reprocess();
      } catch (e) {
        this.failed = true;
        this.error =
          String((e && (e as Error).message) || e) +
          "  (Try checking your network connection.)";
      }
    },
  },
});
