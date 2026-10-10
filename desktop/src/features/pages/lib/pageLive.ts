/**
 * Live-subscription filters for page events.
 *
 * The relay fans a channel-scoped event out only to subscriptions that name its
 * channel in `#h`: a global (`#h`-less) REQ receives no page event at all, even
 * though its history read is scoped to the viewer's channels. Live updates for
 * Space therefore subscribe per channel, in chunks the relay accepts.
 */
import type { RelaySubscriptionFilter } from "@/shared/api/relayClientShared";
import type { RelayEvent } from "@/shared/api/types";
import { KIND_PAGE_REVISION } from "@/shared/constants/kinds";
import { PAGE_EVENT_KINDS, type PageIdentity } from "./pageModel";

/** `#h` values per live REQ. The relay refuses a REQ naming more than 128. */
export const PAGE_LIVE_CHANNELS_PER_REQ = 100;

/**
 * Channels watched live in total (so at most 10 REQs). Pages in the rest are
 * still read by every refetch and by the backstop poll; they only lose the
 * prompt push.
 */
export const PAGE_LIVE_MAX_CHANNELS = 1_000;

/**
 * One live REQ filter per chunk of channels, for the three page kinds.
 * `limit: 0` asks for no history; the bounded reads own history. Channel ids
 * are de-duplicated and sorted so the same set always yields the same filters.
 */
export function buildPageLiveFilters(
  channelIds: readonly string[],
): RelaySubscriptionFilter[] {
  const watched = [...new Set(channelIds)]
    .filter((channelId) => channelId.length > 0)
    .sort()
    .slice(0, PAGE_LIVE_MAX_CHANNELS);
  const filters: RelaySubscriptionFilter[] = [];
  for (
    let start = 0;
    start < watched.length;
    start += PAGE_LIVE_CHANNELS_PER_REQ
  ) {
    filters.push({
      kinds: [...PAGE_EVENT_KINDS],
      "#h": watched.slice(start, start + PAGE_LIVE_CHANNELS_PER_REQ),
      limit: 0,
    });
  }
  return filters;
}

/**
 * What a burst of live page events obliges the client to re-read. The library
 * lists page heads, so only a revision changes it; a page view only changes for
 * events of its own `(h, d)`. Refetching everything on every event would make
 * each edit anywhere re-download the whole library window.
 */
export type PageRefresh = {
  /** A revision arrived: the library's heads may have changed. */
  library: boolean;
  /** Pages (by `(h, d)`) that got an event, keyed to dedupe a burst. */
  pages: Map<string, PageIdentity>;
  /** An event could not be attributed to a page: refresh everything. */
  everything: boolean;
};

export function emptyPageRefresh(): PageRefresh {
  return { library: false, pages: new Map(), everything: false };
}

/** Fold one live event into the pending refresh. */
export function notePageEvent(refresh: PageRefresh, event: RelayEvent): void {
  if (event.kind === KIND_PAGE_REVISION) refresh.library = true;
  const channelId = event.tags.find((tag) => tag[0] === "h")?.[1];
  const pageId = event.tags.find((tag) => tag[0] === "d")?.[1];
  if (!channelId || !pageId) {
    refresh.everything = true;
    return;
  }
  refresh.pages.set(JSON.stringify([channelId, pageId]), {
    channelId,
    pageId,
  });
}
