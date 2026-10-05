import * as React from "react";

import { formatItemTimestamp } from "@/shared/lib/datetime";
import { Badge } from "@/shared/ui/badge";
import type { SuggestionState } from "../lib/pageModel";
import type { AuthorLabeler } from "../types";

type PendingSuggestionsProps = {
  authorLabel: AuthorLabeler;
  suggestions: readonly SuggestionState[];
  /** The suggestion read stopped at its bound: the list may omit older ones. */
  truncated?: boolean;
};

/**
 * Read-only list of a page's open suggestions. A suggestion is open until a
 * resolution or an applying revision closes it (NIP-PG); it is stale when its
 * `base` is no longer the page head, because accepting it would overwrite
 * newer edits.
 */
export function PendingSuggestions({
  authorLabel,
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
      <h2 className="text-lg font-semibold tracking-tight" id={headingId}>
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
          {pending.map(({ stale, suggestion }) => (
            <li
              className="space-y-1 rounded-lg border border-border/70 bg-muted/20 px-3 py-2"
              data-stale={stale ? "true" : "false"}
              data-suggestion-id={suggestion.id}
              data-testid="page-suggestion"
              key={suggestion.id}
            >
              <p className="text-sm font-medium">
                {authorLabel(suggestion.pubkey)}
              </p>
              <p className="text-xs text-muted-foreground">
                <time
                  dateTime={new Date(
                    suggestion.createdAt * 1_000,
                  ).toISOString()}
                >
                  {formatItemTimestamp(suggestion.createdAt, {
                    withTime: true,
                  })}
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
            </li>
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
