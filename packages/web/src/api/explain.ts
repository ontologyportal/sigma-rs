/**
 * Client for `/api/explain` (functions/api/explain.ts): an English
 * explanation of one proof from the deployment's model. Request building and
 * validation come from the same module the server validates with, so a
 * request the client sends is one the server accepts, and the client can
 * tell in advance when a proof is too large to explain.
 */

import type {
  ExplainAvailability,
  ExplainErrorBody,
  ExplainErrorCode,
  ExplainRequest,
  ExplainResponse,
} from "../../functions/_utils/explain";

export {
  LIMITS as EXPLAIN_LIMITS,
  canonicalJson,
  toExplainRequest,
  validateExplainRequest,
} from "../../functions/_utils/explain";
export type {
  ExplainKind,
  ExplainRequest,
  ExplainResponse,
} from "../../functions/_utils/explain";

export class ExplainError extends Error {
  code: ExplainErrorCode;
  /** Seconds until a retry can succeed, for `rate_limited`. */
  retryAfter: number | null;
  constructor(
    code: ExplainErrorCode,
    message: string,
    retryAfter: number | null = null,
  ) {
    super(message);
    this.name = "ExplainError";
    this.code = code;
    this.retryAfter = retryAfter;
  }
}

const FALLBACK_CODE: Record<number, ExplainErrorCode> = {
  400: "invalid",
  401: "unauthorized",
  403: "forbidden",
  429: "rate_limited",
  503: "unavailable",
};

let availability: Promise<boolean> | null = null;

/** Whether this deployment can explain proofs, asked once per page. False
 *  when the API is not deployed (plain `vite` dev answers with the SPA's
 *  HTML) or the model binding is missing. */
export function explainAvailable(): Promise<boolean> {
  availability ??= fetch("/api/explain", {
    headers: { Accept: "application/json" },
  })
    .then(async (res) => {
      if (!res.ok) return false;
      const body = (await res.json()) as ExplainAvailability;
      return body.enabled === true;
    })
    .catch(() => false);
  return availability;
}

/** Ask the server to explain `req`. The session cookie goes along on its
 *  own; `token` (a personal access token sign-in has no session) is sent as
 *  a bearer token. Throws `ExplainError`. */
export async function explainProof(
  req: ExplainRequest,
  token: string | null,
  signal?: AbortSignal,
): Promise<ExplainResponse> {
  const headers: Record<string, string> = {
    "Content-Type": "application/json",
    Accept: "application/json",
  };
  if (token) headers.Authorization = `Bearer ${token}`;
  let res: Response;
  try {
    res = await fetch("/api/explain", {
      method: "POST",
      headers,
      body: JSON.stringify(req),
      signal,
    });
  } catch (e) {
    if (signal?.aborted) throw e;
    throw new ExplainError(
      "failed",
      "Could not reach the explanation service.",
    );
  }
  if (res.ok) return (await res.json()) as ExplainResponse;
  let body: Partial<ExplainErrorBody> = {};
  try {
    body = (await res.json()) as ExplainErrorBody;
  } catch {
    /* not JSON: fall back to the status */
  }
  const code = body.error ?? FALLBACK_CODE[res.status] ?? "failed";
  if (code === "unavailable") availability = Promise.resolve(false);
  const header = Number(res.headers.get("Retry-After"));
  const retryAfter =
    body.retryAfter ?? (Number.isFinite(header) && header > 0 ? header : null);
  throw new ExplainError(
    code,
    body.message || `Explanation failed (HTTP ${res.status}).`,
    retryAfter,
  );
}
