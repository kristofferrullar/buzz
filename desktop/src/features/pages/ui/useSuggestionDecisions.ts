import * as React from "react";

import type { PageDetail, SuggestionState } from "../lib/pageModel";
import { describeDecisionFailure } from "../lib/pageWriteErrors";
import { buildAcceptRevision, decideSuggestion } from "../lib/suggestionReview";
import { usePageWriter } from "../writeHooks";

export type SuggestionDecision = "accept" | "reject";

type UseSuggestionDecisionsArgs = {
  canWrite: boolean;
  communityId: string | null;
  detail: PageDetail;
  /** A decision was published and the page queries have refreshed. */
  onDecided: (decision: SuggestionDecision) => void;
};

/**
 * Accept and reject for the suggestions rail.
 *
 * - **Accept is exactly one event**: a revision carrying `prev` (the head) and
 *   `suggestion`. No resolution is published alongside it; the revision closes
 *   the suggestion by itself (NIP-PG).
 * - **Reject is exactly one event**: a `rejected` resolution.
 * - **One decision at a time.** Accepting moves the head, which makes every
 *   other open suggestion stale, so decisions never overlap.
 * - Each click is re-judged against the *latest* detail, so a handler built
 *   before the head moved cannot apply a suggestion that has since gone stale,
 *   and a late result cannot write over a newer rail.
 */
export function useSuggestionDecisions({
  canWrite,
  communityId,
  detail,
  onDecided,
}: UseSuggestionDecisionsArgs) {
  const writer = usePageWriter(communityId);
  const detailRef = React.useRef(detail);
  detailRef.current = detail;
  const canWriteRef = React.useRef(canWrite);
  canWriteRef.current = canWrite;
  const onDecidedRef = React.useRef(onDecided);
  onDecidedRef.current = onDecided;
  const mountedRef = React.useRef(true);
  const inFlightRef = React.useRef(false);

  const [busy, setBusy] = React.useState<{
    id: string;
    decision: SuggestionDecision;
  } | null>(null);
  const [errors, setErrors] = React.useState<Readonly<Record<string, string>>>(
    {},
  );

  React.useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  const decide = React.useCallback(
    async (suggestionId: string, decision: SuggestionDecision) => {
      if (inFlightRef.current) return;
      const current = detailRef.current;
      const state: SuggestionState | undefined = current.suggestions.find(
        (candidate) => candidate.suggestion.id === suggestionId,
      );
      if (!state) return;
      const verdict = decideSuggestion({
        canWrite: canWriteRef.current,
        head: current.head,
        state,
      });
      if (!verdict.canDecide) return;
      if (decision === "accept" && !verdict.accept.enabled) return;

      inFlightRef.current = true;
      setBusy({ id: suggestionId, decision });
      setErrors((previous) => {
        if (!(suggestionId in previous)) return previous;
        const { [suggestionId]: _cleared, ...rest } = previous;
        return rest;
      });

      const { channelId, pageId } = current;
      const result =
        decision === "accept"
          ? await writer.publishRevision({
              channelId,
              pageId,
              ...buildAcceptRevision(current.head, state.suggestion),
            })
          : await writer.rejectSuggestion({
              channelId,
              pageId,
              suggestion: suggestionId,
            });

      inFlightRef.current = false;
      if (!mountedRef.current) return;
      setBusy(null);
      if (result.ok) {
        onDecidedRef.current(decision);
      } else {
        setErrors((previous) => ({
          ...previous,
          [suggestionId]: describeDecisionFailure(result.error),
        }));
      }
    },
    [writer],
  );

  const accept = React.useCallback(
    (suggestionId: string) => decide(suggestionId, "accept"),
    [decide],
  );
  const reject = React.useCallback(
    (suggestionId: string) => decide(suggestionId, "reject"),
    [decide],
  );

  return { accept, busy, errors, reject };
}
