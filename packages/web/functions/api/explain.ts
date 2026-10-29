// GET  /api/explain -- whether explanations are available on this deployment.
// POST /api/explain -- explain one proof; see _utils/explain-handler.ts for
// the pipeline and its bindings.

import type { ExplainAvailability } from "../_utils/explain";
import {
  explainEnabled,
  handleExplain,
  type ExplainEnv,
} from "../_utils/explain-handler";

export const onRequestGet: PagesFunction<ExplainEnv> = async (context) => {
  const body: ExplainAvailability = { enabled: explainEnabled(context.env) };
  return new Response(JSON.stringify(body), {
    headers: {
      "Content-Type": "application/json",
      "Cache-Control": "no-store",
    },
  });
};

export const onRequestPost: PagesFunction<ExplainEnv> = (context) =>
  handleExplain(context.request, context.env);
