import { onBeforeUnmount, onMounted, watch, type Ref } from "vue";
import { navigate } from "../router";
import { call } from "../services/sigma";

/** Delegated click handling for `a.sym-link[data-sym]` produced by the
 *  highlighters inside `root`: probes `call('manpage')`, marks dead symbols
 *  with `.sym-dead` + title, otherwise `navigate('browse', { sym })`. Also
 *  handles `a.open[data-sym]` / `a.xref[data-sym]` the same way without
 *  probing. */
export function useSymbolLinks(root: Ref<HTMLElement | null>): void {
  let attached: HTMLElement | null = null;

  const onClick = async (e: Event) => {
    const target = e.target as HTMLElement | null;
    const link = target?.closest<HTMLElement>(
      "a.sym-link[data-sym], a.open[data-sym], a.xref[data-sym]",
    );
    if (!link) return;
    // preventDefault also cancels the enclosing <summary>'s expand toggle
    // when the symbol sits inside a citation row.
    e.preventDefault();
    const sym = link.dataset.sym;
    if (!link.classList.contains("sym-link")) {
      navigate("browse", { sym });
      return;
    }
    if (link.classList.contains("sym-dead")) return;
    // Probe before navigating: a symbol with no man page (Skolems that slipped
    // the lexical filter, numerals, ill-formed tokens) must not yank the user
    // away from a proof they are reading just to show an error card.
    try {
      const { page } = await call("manpage", { symbol: sym });
      if (!page) {
        link.classList.add("sym-dead");
        link.title = `no man page for ${sym}`;
        return;
      }
    } catch {
      return;
    }
    navigate("browse", { sym });
  };

  const attach = (el: HTMLElement | null) => {
    if (attached === el) return;
    attached?.removeEventListener("click", onClick);
    attached = el;
    attached?.addEventListener("click", onClick);
  };

  onMounted(() => {
    attach(root.value);
    watch(root, (el) => attach(el));
  });
  onBeforeUnmount(() => attach(null));
}
