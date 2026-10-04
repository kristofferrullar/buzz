import { useQuery, useQueryClient } from "@tanstack/react-query";
import * as React from "react";

import { relayClient } from "@/shared/api/relayClient";
import { createTrailingDebounce } from "@/shared/lib/trailingDebounce";
import {
  fetchPageDetail,
  fetchPagesLibrary,
  type FetchEvents,
  type PageDetailResult,
  type PagesLibrary,
} from "./lib/pageFetch";
import { PAGE_EVENT_KINDS } from "./lib/pageModel";

/**
 * Query keys for pages, always scoped by community. The app also remounts its
 * QueryClient per community, so this is belt and braces: a key can never be
 * shared between two relays.
 */
export const pagesQueryKeys = {
  all: (communityId: string | null) => ["pages", communityId] as const,
  library: (communityId: string | null) =>
    ["pages", communityId, "library"] as const,
  page: (
    communityId: string | null,
    channelId: string | null,
    pageId: string | null,
  ) => ["pages", communityId, "page", channelId, pageId] as const,
};

const readEvents: FetchEvents = (filter) => relayClient.fetchEvents(filter);

/** Page reads stay fresh for this long before a mount refetches them. */
const PAGES_STALE_MS = 30_000;
/**
 * Backstop poll. Live updates make changes prompt; this guarantees a missed
 * live event (failed subscription, dropped frame) is recovered without user
 * action. It only runs while the window is focused.
 */
const PAGES_POLL_MS = 120_000;
/** Collapse a burst of page events (an agent writing several) into one refetch. */
const LIVE_REFRESH_DEBOUNCE_MS = 400;

/** The Space library: every readable page's head, newest first. */
export function usePagesLibraryQuery(communityId: string | null) {
  return useQuery<PagesLibrary>({
    enabled: communityId !== null,
    queryKey: pagesQueryKeys.library(communityId),
    queryFn: () => fetchPagesLibrary(readEvents),
    refetchInterval: PAGES_POLL_MS,
    staleTime: PAGES_STALE_MS,
  });
}

/** One page: head, history and suggestions for exactly its `(h, d)`. */
export function usePageQuery(
  communityId: string | null,
  channelId: string | null,
  pageId: string | null,
) {
  return useQuery<PageDetailResult>({
    enabled: communityId !== null && channelId !== null && pageId !== null,
    queryKey: pagesQueryKeys.page(communityId, channelId, pageId),
    queryFn: () => {
      if (channelId === null || pageId === null) {
        // `enabled` rules this out; a rejection is safer than a fake result.
        return Promise.reject(new Error("No page selected"));
      }
      return fetchPageDetail(readEvents, { channelId, pageId });
    },
    refetchInterval: PAGES_POLL_MS,
    staleTime: PAGES_STALE_MS,
  });
}

/**
 * Keep every page query for the community current while the Space screen is
 * mounted.
 *
 * Page events are regular events, so a live REQ for the three page kinds (no
 * channel tag: the relay scopes it to channels the viewer can read) tells us
 * *that* something changed; the bounded reads above remain the single source of
 * truth for *what* the pages now are. Three triggers invalidate them:
 *
 * - a live page event (debounced, so a burst is one refetch);
 * - the live REQ becoming ready, which closes the gap between a one-shot history
 *   read and the subscription starting: anything published before the
 *   subscription registered is in the refetch, anything after arrives live;
 * - a relay reconnect, which can have dropped events.
 */
export function usePagesLiveUpdates(communityId: string | null): void {
  const queryClient = useQueryClient();

  React.useEffect(() => {
    if (communityId === null) return;
    let disposed = false;
    let unsubscribe: (() => Promise<void>) | null = null;

    const invalidate = () => {
      void queryClient.invalidateQueries({
        queryKey: pagesQueryKeys.all(communityId),
      });
    };
    const debounced = createTrailingDebounce(
      invalidate,
      LIVE_REFRESH_DEBOUNCE_MS,
    );

    void relayClient
      .subscribeLive(
        { kinds: [...PAGE_EVENT_KINDS], limit: 0 },
        debounced.trigger,
        invalidate,
      )
      .then((dispose) => {
        if (disposed) void dispose();
        else unsubscribe = dispose;
      })
      .catch((error) => {
        // Not fatal and not silent: the backstop poll keeps pages current, and
        // a relay reconnect invalidates the page queries below.
        console.error("Couldn't subscribe to live page updates", error);
      });

    const unsubscribeReconnect = relayClient.subscribeToReconnects(invalidate);

    return () => {
      disposed = true;
      debounced.cancel();
      unsubscribeReconnect();
      if (unsubscribe) void unsubscribe();
    };
  }, [communityId, queryClient]);
}
