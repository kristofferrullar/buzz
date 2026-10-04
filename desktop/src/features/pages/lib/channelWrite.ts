/**
 * Which channels the viewer can write pages in, and how a channel is labelled.
 *
 * Write scope mirrors message scope (NIP-PG Relay Rule 1: the author must be
 * able to write to the channel; archived channels reject): a member of a
 * channel that is not archived. The relay stays authoritative; this only decides
 * whether to offer write controls at all, so a read-only viewer never sees a
 * button that cannot work.
 */
import type { Channel } from "@/shared/api/types";

/**
 * Shown for a page whose channel is not in the viewer's channel list, e.g. an
 * open channel they have not joined. The relay still lets them read it.
 */
export const UNKNOWN_CHANNEL_LABEL = "another channel";

/** `#name` for a channel, the DM's own name for a DM, a neutral label if unknown. */
export function channelLabelFor(channel: Channel | undefined): string {
  if (!channel) return UNKNOWN_CHANNEL_LABEL;
  return channel.channelType === "dm" ? channel.name : `#${channel.name}`;
}

/** True when the viewer is a member of `channel` and it is not archived. */
export function isChannelWritable(channel: Channel | undefined): boolean {
  return Boolean(channel?.isMember && !channel.archivedAt);
}

/**
 * Channels offered when creating a page: writable, and not a DM (Space is a
 * team surface), sorted by name so the choice is stable across renders.
 */
export function listWritableChannels(channels: readonly Channel[]): Channel[] {
  return channels
    .filter(
      (channel) => channel.channelType !== "dm" && isChannelWritable(channel),
    )
    .sort((left, right) => left.name.localeCompare(right.name));
}
