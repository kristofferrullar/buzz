import * as React from "react";

import { formatItemTimestamp } from "@/shared/lib/datetime";
import { Badge } from "@/shared/ui/badge";
import { Button } from "@/shared/ui/button";
import type { PageRevision, SuggestionState } from "../lib/pageModel";
import { decideSuggestion } from "../lib/suggestionReview";
import type { AuthorLabeler } from "../types";
import type { SuggestionDecision } from "./useSuggestionDecisions";

type PendingSuggestionsProps = {
  authorLabel: AuthorLabeler;
  /** Which decision is in flight, if any; all decisions wait for it. */
  busy: { id: string; decision: SuggestionDecision } | null;
  /** Whether the viewer can write to the page's channel (Accept/Reject shown). */
  canWrite: boolean;
  /** Why the last decision on a suggestion failed, by suggestion id. */
  errors: Readonly<Record<string, string>>;
  head: PageRevision;
  headingRef?: React.Ref<HTMLHeadingElement>;
  onAccept: (suggestionId: string) => void;
  onReject: (suggestionId: string) => void;
  /** Show or hide a suggestion's diff; `null` hides it. */
  onReview: (suggestionId: string | null) => void;
  reviewingId: string | null;
  suggestions: readonly SuggestionState[];
  /** The suggestion read stopped at its bound: the list may omit older ones. */
  truncated?: boolean;
};

/**
 * A page's open suggestions. A suggestion is open until a resolution or an
 * applying revision closes it (NIP-PG); it is stale when its `base` is no longer
 * the page head, because accepting it would overwrite newer edits.
 *
 * Accept and Reject are explicit clicks by a writer: "agents suggest, humans
 * accept" is a client convention, so nothing here ever decides on its own. A
 * read-only viewer sees the list and the diff, and no decision controls.
 */
export function PendingSuggestions({
  authorLabel,
  busy,
  canWrite,
  errors,
  head,
  headingRef,
  onAccept,
  onReject,
  onReview,
  reviewingId,
  suggestions,
  truncated = false,
}: PendingSuggestionsProps) {
  const headingId = React.useId();
  const pending = suggestions.filter((state) => state.closed === null);

  return (
    <section
      aria-labelledby={headingId}
      className="space-y-2"
      data-testid="page-suggestions"
    >
      <h2
        className="text-lg font-semibold tracking-tight focus-visible:outline-hidden"
        id={headingId}
        ref={headingRef}
        tabIndex={-1}
      >
        Pending suggestions
      </h2>
      {pending.length === 0 ? (
        <p
          className="text-sm text-muted-foreground"
          data-testid="page-suggestions-empty"
        >
          No pending suggestions.
        </p>
      ) : (
        <ul className="space-y-1.5">
          {pending.map((state) => (
            <SuggestionRow
              authorLabel={authorLabel}
              busy={busy}
              canWrite={canWrite}
              error={errors[state.suggestion.id] ?? null}
              head={head}
              key={state.suggestion.id}
              onAccept={onAccept}
              onReject={onReject}
              onReview={onReview}
              reviewing={reviewingId === state.suggestion.id}
              state={state}
            />
          ))}
        </ul>
      )}
      {truncated ? (
        <p
          className="text-sm text-muted-foreground"
          data-testid="page-suggestions-truncated"
        >
          Older suggestions aren&rsquo;t shown.
        </p>
      ) : null}
    </section>
  );
}

type SuggestionRowProps = Pick<
  PendingSuggestionsProps,
  "authorLabel" | "busy" | "canWrite" | "head" | "onAccept" | "onReject"
> & {
  error: string | null;
  onReview: (suggestionId: string | null) => void;
  reviewing: boolean;
  state: SuggestionState;
};

function SuggestionRow({
  authorLabel,
  busy,
  canWrite,
  error,
  head,
  onAccept,
  onReject,
  onReview,
  reviewing,
  state,
}: SuggestionRowProps) {
  const { stale, suggestion } = state;
  const author = authorLabel(suggestion.pubkey);
  const reasonId = React.useId();
  const decision = decideSuggestion({ canWrite, head, state });
  const deciding = busy !== null;
  const thisBusy = busy?.id === suggestion.id ? busy.decision : null;
  const acceptBlocked = !decision.accept.enabled;

  return (
    <li
      aria-busy={thisBusy !== null}
      className="space-y-2 rounded-lg border border-border/70 bg-muted/20 px-3 py-2"
      data-stale={stale ? "true" : "false"}
      data-suggestion-id={suggestion.id}
      data-testid="page-suggestion"
    >
      <p className="text-sm font-medium">{author}</p>
      <p className="text-xs text-muted-foreground">
        <time dateTime={new Date(suggestion.createdAt * 1_000).toISOString()}>
          {formatItemTimestamp(suggestion.createdAt, { withTime: true })}
        </time>
        <span aria-hidden> · </span>{" "}
        {stale
          ? "Based on an earlier revision"
          : "Based on the current revision"}
      </p>
      {stale ? (
        <Badge data-testid="page-suggestion-stale" variant="warning">
          Out of date
        </Badge>
      ) : null}

      <div className="flex flex-wrap gap-1.5">
        <Button
          aria-label={`View changes suggested by ${author}`}
          aria-pressed={reviewing}
          data-testid="page-suggestion-view"
          onClick={() => onReview(reviewing ? null : suggestion.id)}
          size="sm"
          type="button"
          variant={reviewing ? "secondary" : "outline"}
        >
          View changes
        </Button>
        {decision.canDecide ? (
          <>
            {/* aria-disabled, not disabled: a blocked Accept stays focusable so
                its reason is announced, and the click is refused in the hook. */}
            <Button
              aria-describedby={acceptBlocked ? reasonId : undefined}
              aria-disabled={acceptBlocked || deciding}
              aria-label={`Accept suggestion by ${author}`}
              className="aria-disabled:cursor-not-allowed aria-disabled:opacity-50"
              data-testid="page-suggestion-accept"
              onClick={() => {
                if (!acceptBlocked && !deciding) onAccept(suggestion.id);
              }}
              size="sm"
              type="button"
            >
              {thisBusy === "accept" ? "Accepting…" : "Accept"}
            </Button>
            <Button
              aria-disabled={deciding}
              aria-label={`Reject suggestion by ${author}`}
              className="aria-disabled:cursor-not-allowed aria-disabled:opacity-50"
              data-testid="page-suggestion-reject"
              onClick={() => {
                if (!deciding) onReject(suggestion.id);
              }}
              size="sm"
              type="button"
              variant="outline"
            >
              {thisBusy === "reject" ? "Rejecting…" : "Reject"}
            </Button>
          </>
        ) : null}
      </div>

      {decision.canDecide && decision.accept.reason ? (
        <p
          className="text-xs text-muted-foreground"
          data-testid="page-suggestion-accept-reason"
          id={reasonId}
        >
          {decision.accept.reason}
        </p>
      ) : null}
      {error ? (
        <p
          className="text-xs text-destructive"
          data-testid="page-suggestion-error"
          role="alert"
        >
          {error}
        </p>
      ) : null}
    </li>
  );
}
