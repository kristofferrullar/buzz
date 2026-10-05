/**
 * Bounded relay reads for pages.
 *
 * Page events are ordinary Nostr events, so reads are REQ filters over the
 * client's relay connection (the same path projects and the team catalog use);
 * there is no page-specific Tauri command. Every filter names `kinds` — an
 * unscoped kind list is rejected by the relay — and every read is bounded.
 */
import type { RelaySubscriptionFilter } from "@/shared/api/relayClientShared";
import type { RelayEvent } from "@/shared/api/types";
import {
  buildLibrary,
  buildPageDetail,
  parsePageEvents,
  type PageDetail,
  type PageIdentity,
  type PageSummary,
} from "./pageModel";
import {
  KIND_PAGE_REVISION,
  KIND_PAGE_SUGGESTION,
  KIND_PAGE_SUGGESTION_RESOLUTION,
} from "@/shared/constants/kinds";

/**
 * Events per REQ. The relay clamps `limit` to its advertised ceiling (1000),
 * so a page that comes back shorter than this is the end of the window.
 */
export const PAGE_FETCH_LIMIT = 500;

/** REQs per read: the hard bound on a library or page-history fetch. */
export const PAGE_FETCH_MAX_REQUESTS = 4;

export type FetchEvents = (
  filter: RelaySubscriptionFilter,
) => Promise<RelayEvent[]>;

/** Overrides for the paging bounds; production uses the defaults. */
export type PagingOptions = { maxRequests?: number; pageLimit?: number };

export type PagedFetchResult = {
  events: RelayEvent[];
  /**
   * True when the read stopped at its bound (or at a second too dense to page
   * past) before reaching the end of history. Newer events are always complete;
   * older ones may be missing, so callers must say so instead of presenting the
   * result as exhaustive.
   */
  truncated: boolean;
};

/**
 * Walk a filter's history newest-first with the `until` cursor, up to
 * `maxRequests` REQs of `pageLimit` events.
 *
 * `until` is inclusive, so each page re-returns its boundary second and `seen`
 * de-duplicates. A full page that cannot lower the cursor means more than one
 * page of events share one second; the WS filter has no `(created_at, id)`
 * cursor to escape that, so the read stops and reports `truncated` rather than
 * looping or silently claiming completeness.
 */
export async function fetchEventsPaged(
  fetchEvents: FetchEvents,
  base: Omit<RelaySubscriptionFilter, "limit" | "until">,
  {
    maxRequests = PAGE_FETCH_MAX_REQUESTS,
    pageLimit = PAGE_FETCH_LIMIT,
  }: PagingOptions = {},
): Promise<PagedFetchResult> {
  const collected = new Map<string, RelayEvent>();
  let until: number | undefined;

  for (let request = 0; request < maxRequests; request += 1) {
    const page = await fetchEvents({
      ...base,
      limit: pageLimit,
      ...(until === undefined ? {} : { until }),
    });

    let added = 0;
    let oldest = Number.POSITIVE_INFINITY;
    for (const event of page) {
      if (event.created_at < oldest) oldest = event.created_at;
      if (collected.has(event.id)) continue;
      collected.set(event.id, event);
      added += 1;
    }

    if (page.length < pageLimit) {
      return { events: [...collected.values()], truncated: false };
    }
    if (added === 0 || (until !== undefined && oldest >= until)) {
      return { events: [...collected.values()], truncated: true };
    }
    until = oldest;
  }

  return { events: [...collected.values()], truncated: true };
}

export type PagesLibrary = {
  pages: PageSummary[];
  /** The newest revisions were read but older pages may not be listed. */
  truncated: boolean;
};

/**
 * Events strictly newer than the oldest second in `events`, or all of them when
 * that would leave nothing.
 *
 * A read that stopped at its bound can end inside one second, and the relay
 * lists a second's events by id, not by causality: the window may hold a
 * revision but not the one built on it, which would then read as the page's
 * head. Everything newer than the cut second is complete.
 */
function withoutOldestSecond(events: readonly RelayEvent[]): RelayEvent[] {
  let oldest = Number.POSITIVE_INFINITY;
  for (const event of events) {
    if (event.created_at < oldest) oldest = event.created_at;
  }
  const complete = events.filter((event) => event.created_at > oldest);
  // A window that is one dense second has nothing newer to keep; list what was
  // read (flagged truncated) rather than claim the relay has no pages.
  return complete.length > 0 ? complete : [...events];
}

/**
 * Read the Space library: the newest revisions across every channel the viewer
 * can read, grouped into pages. The filter carries no `#h`, so the relay scopes
 * it to the viewer's accessible channels exactly as it does the Home feed.
 *
 * A page's head is never older than its other revisions, so any page with a
 * revision in the window has its head in the window too, except that a window
 * cut inside one second can hold only the older of two revisions made in that
 * second (NIP-PG rule 9). Such a window drops its oldest second before pages
 * are built.
 */
export async function fetchPagesLibrary(
  fetchEvents: FetchEvents,
  paging?: PagingOptions,
): Promise<PagesLibrary> {
  const { events, truncated } = await fetchEventsPaged(
    fetchEvents,
    { kinds: [KIND_PAGE_REVISION] },
    paging,
  );
  const readable = truncated ? withoutOldestSecond(events) : events;
  return {
    pages: buildLibrary(parsePageEvents(readable).revisions),
    truncated,
  };
}

export type PageDetailResult = {
  /** `null` when the page has no readable revision. */
  detail: PageDetail | null;
  /**
   * The revision read hit its bound: older revisions may be missing from the
   * history. The head is always current.
   */
  truncated: boolean;
  /**
   * The suggestion/resolution read hit its bound: older suggestions, or the
   * resolutions that closed them, may be missing, so the pending list is not
   * exhaustive.
   */
  suggestionsTruncated: boolean;
};

/**
 * Read one page: revisions, suggestions and resolutions for exactly its
 * `(h, d)`. NIP-PG requires the relay to answer `#h` + `#d` queries exactly, so
 * a quiet page's history is complete even in a busy channel.
 *
 * Revisions are read on their own so that suggestions and resolutions (newer
 * than the head, and unbounded in number) can never fill the window and push
 * the head out of it. Either read failing fails the whole call: a missing
 * suggestion read must not look like "no suggestions".
 */
export async function fetchPageDetail(
  fetchEvents: FetchEvents,
  identity: PageIdentity,
  paging?: PagingOptions,
): Promise<PageDetailResult> {
  const scope = {
    "#h": [identity.channelId],
    "#d": [identity.pageId],
  };
  const [revisions, activity] = await Promise.all([
    fetchEventsPaged(
      fetchEvents,
      { kinds: [KIND_PAGE_REVISION], ...scope },
      paging,
    ),
    fetchEventsPaged(
      fetchEvents,
      {
        kinds: [KIND_PAGE_SUGGESTION, KIND_PAGE_SUGGESTION_RESOLUTION],
        ...scope,
      },
      paging,
    ),
  ]);
  return {
    detail: buildPageDetail(identity, [
      ...revisions.events,
      ...activity.events,
    ]),
    truncated: revisions.truncated,
    suggestionsTruncated: activity.truncated,
  };
}
