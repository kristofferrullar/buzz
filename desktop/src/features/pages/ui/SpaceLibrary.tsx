import { ChevronRight, FileText, RefreshCw } from "lucide-react";
import * as React from "react";

import type { Channel } from "@/shared/api/types";
import { useChannelNavigation } from "@/shared/context/ChannelNavigationContext";
import { formatItemTimestamp } from "@/shared/lib/datetime";
import { Button } from "@/shared/ui/button";
import { BuzzLoadingState } from "@/shared/ui/BuzzLoadingState";
import { PageHeader } from "@/shared/ui/PageHeader";
import { usePagesLibraryQuery } from "../hooks";
import type { PageSummary } from "../lib/pageModel";
import type { PageSelection, PagesHostBindings } from "../types";
import { ROVING_ITEM_PROPS, useRovingList } from "./useRovingList";

const UNKNOWN_CHANNEL_LABEL = "another channel";

function channelLabelFor(channel: Channel | undefined): string {
  if (!channel) return UNKNOWN_CHANNEL_LABEL;
  return channel.channelType === "dm" ? channel.name : `#${channel.name}`;
}

type SpaceLibraryProps = PagesHostBindings & {
  communityId: string | null;
  onOpenPage: (selection: PageSelection) => void;
};

/**
 * The Space library: every page the viewer can read, across channels, newest
 * edit first. Read-only; rows open the page view.
 */
export function SpaceLibrary({
  communityId,
  onOpenPage,
  useAuthorLabels,
}: SpaceLibraryProps) {
  const query = usePagesLibraryQuery(communityId);
  const { channels } = useChannelNavigation();
  const pages = query.data?.pages;

  const authorPubkeys = React.useMemo(
    () => (pages ?? []).map((page) => page.updatedBy),
    [pages],
  );
  const authorLabel = useAuthorLabels(authorPubkeys);
  const channelsById = React.useMemo(
    () => new Map(channels.map((channel) => [channel.id, channel])),
    [channels],
  );
  const { listProps, onRowFocus, tabStopIndex } = useRovingList(
    pages?.length ?? 0,
  );
  const handleOpen = React.useCallback(
    (channelId: string, pageId: string) => onOpenPage({ channelId, pageId }),
    [onOpenPage],
  );

  return (
    <div
      className="flex min-h-0 flex-1 flex-col overflow-y-auto overflow-x-hidden overscroll-contain px-4 py-7 sm:px-6 sm:py-8"
      data-testid="pages-library"
    >
      <div className="mx-auto w-full max-w-3xl space-y-6">
        <PageHeader
          action={
            <Button
              aria-label="Refresh pages"
              disabled={query.isFetching}
              onClick={() => void query.refetch()}
              size="icon"
              type="button"
              variant="ghost"
            >
              <RefreshCw
                aria-hidden
                className={`h-4 w-4 ${query.isFetching ? "animate-spin" : ""}`}
              />
            </Button>
          }
          description="Documents from every channel you can read."
          title="Space"
        />

        {query.isLoading ? (
          <BuzzLoadingState className="min-h-48" label="Loading pages" />
        ) : query.isError ? (
          <div
            className="flex flex-col items-center gap-3 py-16 text-center"
            data-testid="pages-library-error"
            role="alert"
          >
            <p className="text-sm text-destructive">
              Couldn&rsquo;t load pages.
            </p>
            <p className="text-sm text-muted-foreground">
              {query.error instanceof Error ? query.error.message : null}
            </p>
            <Button
              onClick={() => void query.refetch()}
              size="sm"
              type="button"
              variant="outline"
            >
              Try again
            </Button>
          </div>
        ) : pages && pages.length > 0 ? (
          <>
            <ul
              aria-label="Pages"
              className="space-y-2"
              data-testid="pages-library-list"
              {...listProps}
            >
              {pages.map((page, index) => (
                <SpaceLibraryRow
                  authorLabel={authorLabel(page.updatedBy)}
                  channelLabel={channelLabelFor(
                    channelsById.get(page.channelId),
                  )}
                  index={index}
                  isTabStop={index === tabStopIndex}
                  key={page.key}
                  onOpen={handleOpen}
                  onRowFocus={onRowFocus}
                  page={page}
                />
              ))}
            </ul>
            {query.data?.truncated ? (
              <p
                className="text-sm text-muted-foreground"
                data-testid="pages-library-truncated"
              >
                Showing pages with the most recent edits. Older pages
                aren&rsquo;t listed.
              </p>
            ) : null}
          </>
        ) : (
          <div
            className="flex flex-col items-center gap-2 py-16 text-center"
            data-testid="pages-library-empty"
          >
            <FileText
              aria-hidden
              className="h-8 w-8 text-muted-foreground/60"
            />
            <p className="text-base font-medium">No pages yet</p>
            <p className="max-w-sm text-sm text-muted-foreground">
              Pages from any channel you can read will appear here, newest edit
              first.
            </p>
          </div>
        )}
      </div>
    </div>
  );
}

type SpaceLibraryRowProps = {
  authorLabel: string;
  channelLabel: string;
  index: number;
  isTabStop: boolean;
  onOpen: (channelId: string, pageId: string) => void;
  onRowFocus: (index: number) => void;
  page: PageSummary;
};

const SpaceLibraryRow = React.memo(function SpaceLibraryRow({
  authorLabel,
  channelLabel,
  index,
  isTabStop,
  onOpen,
  onRowFocus,
  page,
}: SpaceLibraryRowProps) {
  return (
    <li>
      {/* One native button owns the row: a single screen-reader stop whose
          name is its text (title, channel, author, time). */}
      <button
        {...ROVING_ITEM_PROPS}
        className="flex w-full min-w-0 items-center gap-3 rounded-xl border border-border/70 bg-muted/20 px-4 py-3 text-left transition-colors hover:bg-muted/40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
        data-channel-id={page.channelId}
        data-page-id={page.pageId}
        data-testid="pages-library-row"
        onClick={() => onOpen(page.channelId, page.pageId)}
        onFocus={() => onRowFocus(index)}
        tabIndex={isTabStop ? 0 : -1}
        type="button"
      >
        <FileText
          aria-hidden
          className="h-4 w-4 shrink-0 text-muted-foreground"
        />
        <span className="min-w-0 flex-1">
          <span className="block truncate text-base font-medium">
            {page.title}
          </span>
          <span className="block truncate text-sm text-muted-foreground">
            in {channelLabel}
            <span aria-hidden> · </span> updated by {authorLabel}
            <span aria-hidden> · </span>{" "}
            <time dateTime={new Date(page.updatedAt * 1_000).toISOString()}>
              {formatItemTimestamp(page.updatedAt, { withTime: true })}
            </time>
          </span>
        </span>
        <ChevronRight
          aria-hidden
          className="h-4 w-4 shrink-0 text-muted-foreground"
        />
      </button>
    </li>
  );
});
