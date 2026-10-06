/**
 * Live diagnostics for Ask/Tell's two SUO-KIF panes: validates the
 * assertions + query (debounced after edits, one request at a time, the
 * latest edit always validated last) and paints each editor's markers.
 * Off -- markers cleared -- while `enabled` is false (TPTP mode, where the
 * KIF validator would flag valid text).
 */

import { onBeforeUnmount, watch, type Ref } from "vue";
import { call } from "../services/sigma";
import { errMsg } from "../utils/format";

/** The editor surface this needs (MonacoEditor's exposed API). */
interface MarkedEditor {
  setMarkers(markers: { severity: string }[]): void;
}

export function useScratchValidation(opts: {
  assertions: Ref<string>;
  query: Ref<string>;
  assertionsEd: Ref<MarkedEditor | null>;
  queryEd: Ref<MarkedEditor | null>;
  enabled: Ref<boolean>;
}) {
  const { assertions, query, assertionsEd, queryEd, enabled } = opts;
  let timer = 0;
  let busy = false;
  let queued = false;
  let editorsReady = 0;

  /** Validate both panes and paint their markers; returns the diagnostics. */
  async function validate() {
    const r = await call("validateScratch", {
      assertions: assertions.value,
      query: query.value,
    });
    assertionsEd.value?.setMarkers(r.assertions);
    queryEd.value?.setMarkers(r.query);
    return r;
  }

  async function refresh() {
    const a = assertionsEd.value;
    const q = queryEd.value;
    if (!a || !q) return;
    if (!enabled.value) {
      a.setMarkers([]);
      q.setMarkers([]);
      return;
    }
    if (busy) {
      queued = true;
      return;
    }
    busy = true;
    try {
      await validate();
    } catch (e) {
      console.warn("validateScratch:", errMsg(e));
    } finally {
      busy = false;
      if (queued) {
        queued = false;
        refresh();
      }
    }
  }

  function schedule() {
    clearTimeout(timer);
    timer = window.setTimeout(refresh, 400);
  }

  /** Call once per pane's editor as it mounts; the first pass runs when
   *  both are up. */
  function editorReady() {
    editorsReady += 1;
    if (editorsReady === 2) refresh();
  }

  /** Validate both panes now (not after the debounce) and count their
   *  error-level diagnostics. */
  async function paneErrors(): Promise<{ assertions: number; query: number }> {
    const r = await validate();
    const errors = (ds: { severity: string }[]) =>
      ds.filter((d) => d.severity === "error").length;
    return { assertions: errors(r.assertions), query: errors(r.query) };
  }

  watch([assertions, query], schedule);
  watch(enabled, () => refresh());
  onBeforeUnmount(() => clearTimeout(timer));

  return { editorReady, paneErrors };
}
