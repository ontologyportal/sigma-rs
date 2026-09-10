/** Startup: boots the worker, opens OPFS, then either restores the KB from
 *  the OPFS snapshot cache or fetches + ingests every saved constituent --
 *  driving the LoadingScreen's progress bar until the app is usable. */

import { defineStore } from "pinia";
import { call } from "../services/sigma";
import { tryRestore } from "../services/kb-cache";
import { BASE } from "../constants";
import { useKBStore } from "./kb";
import { useWordNetStore } from "./wordnet";
import { useTestsStore } from "./tests";
import { useChangesStore } from "./changes";

export const useBootStore = defineStore("boot", {
  state: () => ({
    /** True once the KB is loaded and the app is usable (promote+validate
     *  may still be running in the background). */
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

    /** Reconcile tracked local changes against upstream in the background.
     *  Not awaited: both steps are no-ops when nothing is tracked, and
     *  neither should hold up a page that is already usable. */
    syncChanges() {
      const changes = useChangesStore();
      (async () => {
        await changes.refreshProposals();
        await changes.refreshUpstreamShas({ force: true });
      })().catch(() => {
        /* offline or rate-limited: the badge keeps the last known state */
      });
    },

    async start() {
      const kb = useKBStore();
      const wordnet = useWordNetStore();
      const tests = useTestsStore();
      try {
        this.msg = "Starting the engine...";
        await this.bootWorker();
        this.opfsRoot = await navigator.storage.getDirectory();

        // A cache hit is a short, fixed sequence (restore, WordNet fetch +
        // install, cache restored); a miss is one step per saved constituent
        // plus two for WordNet.
        this.resetProgress(4);
        const restored = await tryRestore((msg) => this.progress(msg));
        if (!restored) {
          this.resetProgress(kb.saved.length + 2);
          this.progress("Fetching WordNet lexicon...");
          await wordnet.install();
          this.progress("Loading WordNet lexicon...");
          await kb.loadSavedConstituents((name, i, total) => {
            this.progress(`Fetching ${name} (${i}/${total})`);
          });
        }

        this.finished = true;
        // Off the critical path: the app is usable (browse/search) before
        // the KB is fully axiomatized or the tests are back.
        if (!restored) kb.reprocess();
        else kb.refreshLangSelect();
        tests.restore();
        this.syncChanges();
      } catch (e) {
        this.failed = true;
        this.error =
          String((e && (e as Error).message) || e) +
          "  (Try checking your network connection.)";
      }
    },
  },
});
