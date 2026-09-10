import {
  computed,
  onActivated,
  onDeactivated,
  onMounted,
  ref,
  watch,
  type ComputedRef,
} from "vue";
import {
  useRoute,
  type LocationQuery,
  type LocationQueryValue,
} from "vue-router";

type QueryValue = LocationQueryValue | LocationQueryValue[] | undefined;

/** `query` is a computed of the current route's query while this tab is the
 *  active route (frozen at its last value while inactive, so a deactivated
 *  view never reacts to another tab's URL). `onQuery(handler)` runs `handler`
 *  when the tab is activated and whenever `query` changes while active
 *  (immediate on activation). `str(v)` narrows a LocationQueryValue to a
 *  string ('' when absent); `num(v)` to a positive finite number, else null. */
export function useTabQuery(tabs: string[]): {
  query: ComputedRef<LocationQuery>;
  onQuery(handler: (query: LocationQuery) => void): void;
  str(v: QueryValue): string;
  num(v: QueryValue): number | null;
} {
  const route = useRoute();
  const onTab = () => tabs.includes(String(route.name));

  // A freshly created view is active when mounted under its own route;
  // keep-alive then reports every later enter/leave.
  const active = ref(onTab());
  onActivated(() => {
    active.value = true;
  });
  onDeactivated(() => {
    active.value = false;
  });

  let frozen: LocationQuery = route.query;
  const query = computed(() => {
    if (active.value && onTab()) frozen = route.query;
    return frozen;
  });

  function onQuery(handler: (query: LocationQuery) => void) {
    // Mount and the first activation both fire for a new view: run the
    // handler once per distinct query object.
    let lastHandled: LocationQuery | null = null;
    const run = () => {
      const q = query.value;
      if (q === lastHandled) return;
      lastHandled = q;
      handler(q);
    };
    watch(query, run);
    onMounted(() => {
      if (active.value) run();
    });
    onActivated(run);
  }

  const str = (v: QueryValue): string => {
    const one = Array.isArray(v) ? v[0] : v;
    return one == null ? "" : String(one);
  };

  const num = (v: QueryValue): number | null => {
    const s = str(v);
    if (!s) return null;
    const n = Number(s);
    return Number.isFinite(n) && n > 0 ? n : null;
  };

  return { query, onQuery, str, num };
}
