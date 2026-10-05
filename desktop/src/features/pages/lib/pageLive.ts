/**
 * Live-subscription filters for page events.
 *
 * The relay fans a channel-scoped event out only to subscriptions that name its
 * channel in `#h`: a global (`#h`-less) REQ receives no page event at all, even
 * though its history read is scoped to the viewer's channels. Live updates for
 * Space therefore subscribe per channel, in chunks the relay accepts.
 */
import type { RelaySubscriptionFilter } from "@/shared/api/relayClientShared";
import { PAGE_EVENT_KINDS } from "./pageModel";

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
