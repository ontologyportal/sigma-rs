import { reactive } from "vue";
import { errMsg } from "../utils/format";

/** A status/log line's state, for `<StatusLine :text :error>`. */
export interface Status {
  text: string;
  error: boolean;
  /** Show `text`, red when `error`. */
  set(text: string, error?: boolean): void;
  /** Show an error's message in red. */
  fail(e: unknown): void;
  clear(): void;
}

export function useStatus(initial = ""): Status {
  const status = reactive<Status>({
    text: initial,
    error: false,
    set(text: string, error = false) {
      status.text = text;
      status.error = error;
    },
    fail(e: unknown) {
      status.text = errMsg(e);
      status.error = true;
    },
    clear() {
      status.text = "";
      status.error = false;
    },
  });
  return status;
}
