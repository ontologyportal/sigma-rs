import { onScopeDispose, unref, watch, type Ref } from "vue";

type Target = Ref<HTMLElement | null> | (() => HTMLElement | null);

function resolve(t: Target): HTMLElement | null {
  return typeof t === "function" ? t() : unref(t);
}

/** Calls `onOutside` for any pointerdown outside every element in `targets`
 *  (refs or getters, nulls skipped), and on Escape. Listens only while
 *  `active` is true; listeners are removed when the owning scope ends. */
export function useOutsideClick(
  targets: Target[],
  onOutside: () => void,
  active: Ref<boolean>,
) {
  const onPointer = (e: PointerEvent) => {
    const node = e.target as Node | null;
    if (!node) return;
    for (const t of targets) {
      const el = resolve(t);
      if (el && el.contains(node)) return;
    }
    onOutside();
  };
  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Escape") onOutside();
  };

  let listening = false;
  const attach = () => {
    if (listening) return;
    document.addEventListener("pointerdown", onPointer, true);
    document.addEventListener("keydown", onKey);
    listening = true;
  };
  const detach = () => {
    if (!listening) return;
    document.removeEventListener("pointerdown", onPointer, true);
    document.removeEventListener("keydown", onKey);
    listening = false;
  };

  watch(active, (on) => (on ? attach() : detach()), { immediate: true });
  onScopeDispose(detach);
}
