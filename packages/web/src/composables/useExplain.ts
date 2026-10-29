import { onScopeDispose, ref, shallowRef } from "vue";
import {
  ExplainError,
  canonicalJson,
  explainProof,
  type ExplainRequest,
  type ExplainResponse,
} from "../api/explain";
import { useAuthStore } from "../stores/auth";

/** Explanations already fetched on this page, keyed by `canonicalJson`, so
 *  reopening a proof (or a second card showing the same one) costs no call. */
const memo = new Map<string, ExplainResponse>();

/**
 * One proof's explanation: `explain(req)` fetches it (or reuses the page
 * memo), `show(req)` displays a memoized one without fetching, and an
 * in-flight request is aborted when the owning component unmounts or a
 * newer request starts.
 */
export function useExplain() {
  const auth = useAuthStore();
  const result = shallowRef<ExplainResponse | null>(null);
  const error = shallowRef<ExplainError | null>(null);
  const busy = ref(false);
  let ctrl: AbortController | null = null;

  function cancel() {
    ctrl?.abort();
    ctrl = null;
    busy.value = false;
  }

  function show(req: ExplainRequest) {
    cancel();
    result.value = memo.get(canonicalJson(req)) ?? null;
    error.value = null;
  }

  async function explain(req: ExplainRequest) {
    const key = canonicalJson(req);
    const known = memo.get(key);
    if (known) {
      result.value = known;
      error.value = null;
      return;
    }
    cancel();
    const mine = new AbortController();
    ctrl = mine;
    busy.value = true;
    error.value = null;
    try {
      const res = await explainProof(req, auth.token, mine.signal);
      memo.set(key, res);
      if (ctrl === mine) result.value = res;
    } catch (e) {
      if (mine.signal.aborted) return;
      error.value =
        e instanceof ExplainError
          ? e
          : new ExplainError("failed", "The explanation failed.");
    } finally {
      if (ctrl === mine) {
        ctrl = null;
        busy.value = false;
      }
    }
  }

  onScopeDispose(cancel);
  return { result, error, busy, explain, show };
}
