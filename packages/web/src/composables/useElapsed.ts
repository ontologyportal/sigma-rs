import { computed, onBeforeUnmount, ref, watch, type Ref } from "vue";

/** Whole seconds `s` as `42s` or `3m 05s`. */
export function fmtSecs(s: number): string {
  const whole = Math.floor(s);
  if (whole < 60) return `${whole}s`;
  const m = Math.floor(whole / 60);
  return `${m}m ${String(whole % 60).padStart(2, "0")}s`;
}

/**
 * Wall-clock timer for a long-running job, driven by its `running` flag.
 *
 * `seconds` counts up while `running` is true; `lastSecs` is the duration of
 * the most recent completed run (null before the first). `label(limitSecs)`
 * renders the live counter as `12s / 30s`, or just `12s` when the limit is 0
 * (no limit); `fraction(limitSecs)` is the share of the limit used so far
 * (capped at 1), or null with no limit.
 */
export function useElapsed(running: Ref<boolean>) {
  const seconds = ref(0);
  const lastSecs = ref<number | null>(null);
  let startedAt = 0;
  let timer: ReturnType<typeof setInterval> | undefined;

  const stop = () => {
    clearInterval(timer);
    timer = undefined;
  };

  watch(
    running,
    (on) => {
      if (on) {
        startedAt = performance.now();
        seconds.value = 0;
        timer = setInterval(() => {
          seconds.value = (performance.now() - startedAt) / 1000;
        }, 250);
      } else if (timer !== undefined) {
        stop();
        lastSecs.value = (performance.now() - startedAt) / 1000;
      }
    },
    { immediate: true },
  );
  onBeforeUnmount(stop);

  const label = (limitSecs: number) =>
    limitSecs > 0
      ? `${fmtSecs(seconds.value)} / ${fmtSecs(limitSecs)}`
      : fmtSecs(seconds.value);
  const fraction = (limitSecs: number) =>
    limitSecs > 0 ? Math.min(1, seconds.value / limitSecs) : null;
  const lastLabel = computed(() =>
    lastSecs.value === null ? "" : `${lastSecs.value.toFixed(1)}s`,
  );

  return { seconds, lastSecs, label, fraction, lastLabel };
}
