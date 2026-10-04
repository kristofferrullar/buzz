/**
 * What a reviewer can do with an open suggestion, and what to diff it against.
 *
 * NIP-PG ("Accepting a suggestion"): a suggestion carries full replacement
 * content, so applying it overwrites the page. It can only be accepted while its
 * `base` is the current head; acceptance is exactly one `PAGE_REVISION` naming
 * the suggestion; a suggestion that already equals the head is closed with a
 * rejection (a no-op revision cannot be stored). Nothing here decides for the
 * user: every outcome still needs an explicit click.
 */
import type {
  PageDetail,
  PageRevision,
  PageSuggestion,
  SuggestionState,
} from "./pageModel";

export type SuggestionDecision = {
  /** The viewer may accept or reject at all (a writer of the page's channel). */
  canDecide: boolean;
  accept: {
    enabled: boolean;
    /** Why Accept is disabled; shown beside the button, never only a tooltip. */
    reason: string | null;
  };
};

export const STALE_ACCEPT_REASON =
  "The page changed after this suggestion was made, so it can't be applied. Reject it and ask for a new one.";
export const APPLIED_ACCEPT_REASON =
  "This suggestion already matches the current page. Reject it to close it.";

/**
 * Whether `state` can be accepted against `head`, for a viewer who can (or
 * cannot) write to the channel. Reject is available to every writer, including
 * on a stale suggestion, so a stale one never becomes undismissable.
 */
export function decideSuggestion({
  canWrite,
  head,
  state,
}: {
  canWrite: boolean;
  head: PageRevision;
  state: SuggestionState;
}): SuggestionDecision {
  if (!canWrite || state.closed !== null) {
    return { canDecide: false, accept: { enabled: false, reason: null } };
  }
  if (state.stale) {
    return {
      canDecide: true,
      accept: { enabled: false, reason: STALE_ACCEPT_REASON },
    };
  }
  if (state.suggestion.content === head.content) {
    return {
      canDecide: true,
      accept: { enabled: false, reason: APPLIED_ACCEPT_REASON },
    };
  }
  return { canDecide: true, accept: { enabled: true, reason: null } };
}

/** The single revision accepting a suggestion publishes. */
export type AcceptRevision = {
  title: string;
  content: string;
  prev: string;
  suggestion: string;
};

/**
 * Fields of the one `PAGE_REVISION` that applies `suggestion` on top of `head`.
 * `prev` is the head and equals the suggestion's `base` (the caller has already
 * refused a stale one); the title is the head's because a suggestion changes
 * content only.
 */
export function buildAcceptRevision(
  head: PageRevision,
  suggestion: PageSuggestion,
): AcceptRevision {
  return {
    title: head.title,
    content: suggestion.content,
    prev: head.id,
    suggestion: suggestion.id,
  };
}

export type SuggestionDiffBase =
  /** The suggestion's own base revision: the comparison NIP-PG defines. */
  | { kind: "base"; revision: PageRevision }
  /**
   * The base is not in the loaded history (older revisions were truncated or the
   * revision was deleted), so the comparison falls back to the current head.
   */
  | { kind: "head-fallback"; revision: PageRevision };

/** The revision a suggestion's diff is drawn against. */
export function resolveSuggestionDiffBase(
  detail: Pick<PageDetail, "head" | "history">,
  suggestion: PageSuggestion,
): SuggestionDiffBase {
  const base = detail.history.find(
    (entry) => entry.revision.id === suggestion.base,
  )?.revision;
  return base
    ? { kind: "base", revision: base }
    : { kind: "head-fallback", revision: detail.head };
}
