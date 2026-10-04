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
  PAGE_EVENT_KINDS,
  parsePageEvents,
  type PageDetail,
  type PageIdentity,
  type PageSummary,
} from "./pageModel";
import { KIND_PAGE_REVISION } from "@/shared/constants/kinds";

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
 * Read the Space library: the newest revisions across every channel the viewer
 * can read, grouped into pages. The filter carries no `#h`, so the relay scopes
 * it to the viewer's accessible channels exactly as it does the Home feed.
 *
 * A page's newest revision is newer than all its others, so any page with a
 * revision in the window has its true head in the window too.
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
  return {
    pages: buildLibrary(parsePageEvents(events).revisions),
    truncated,
  };
}

export type PageDetailResult = {
  /** `null` when the page has no readable revision. */
  detail: PageDetail | null;
  /** Older history or suggestions may be missing; the head is always current. */
  truncated: boolean;
};

/**
 * Read one page: revisions, suggestions and resolutions for exactly its
 * `(h, d)`. NIP-PG requires the relay to answer `#h` + `#d` queries exactly, so
 * a quiet page's history is complete even in a busy channel.
 */
export async function fetchPageDetail(
  fetchEvents: FetchEvents,
  identity: PageIdentity,
  paging?: PagingOptions,
): Promise<PageDetailResult> {
  const { events, truncated } = await fetchEventsPaged(
    fetchEvents,
    {
      kinds: [...PAGE_EVENT_KINDS],
      "#h": [identity.channelId],
      "#d": [identity.pageId],
    },
    paging,
  );
  return { detail: buildPageDetail(identity, events), truncated };
}
