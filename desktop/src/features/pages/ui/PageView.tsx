import { ArrowLeft } from "lucide-react";
import * as React from "react";

import { useChannelNavigation } from "@/shared/context/ChannelNavigationContext";
import { formatItemTimestamp } from "@/shared/lib/datetime";
import { Button } from "@/shared/ui/button";
import { BuzzLoadingState } from "@/shared/ui/BuzzLoadingState";
import { Markdown } from "@/shared/ui/markdown";
import { PageHeader } from "@/shared/ui/PageHeader";
import { usePageQuery } from "../hooks";
import type { PageDetail } from "../lib/pageModel";
import type { PageSelection, PagesHostBindings } from "../types";
import { PageHistoryList } from "./PageHistoryList";
import { PendingSuggestions } from "./PendingSuggestions";

type PageViewProps = PagesHostBindings & {
  communityId: string | null;
  onBack: () => void;
  selection: PageSelection;
};

/**
 * A single page, read-only: rendered markdown, metadata, revision history
 * (selecting a revision shows its content) and pending suggestions.
 *
 * Mount with `key` set to the page identity so the selected revision never
 * carries over from one page to another.
 */
export function PageView({
  communityId,
  onBack,
  selection,
  useAuthorLabels,
}: PageViewProps) {
  const query = usePageQuery(
    communityId,
    selection.channelId,
    selection.pageId,
  );
  const detail = query.data?.detail ?? null;

  return (
    <div
      className="flex min-h-0 flex-1 flex-col overflow-y-auto overflow-x-hidden overscroll-contain px-4 py-7 sm:px-6 sm:py-8"
      data-testid="page-view"
    >
      <div className="mx-auto w-full max-w-6xl space-y-6">
        <Button
          className="-ml-2"
          data-testid="page-back"
          onClick={onBack}
          size="sm"
          type="button"
          variant="ghost"
        >
          <ArrowLeft aria-hidden className="h-4 w-4" />
          Back to Space
        </Button>

        {query.isLoading ? (
          <BuzzLoadingState className="min-h-48" label="Loading page" />
        ) : query.isError ? (
          <div
            className="flex flex-col items-center gap-3 py-16 text-center"
            data-testid="page-error"
            role="alert"
          >
            <p className="text-sm text-destructive">
              Couldn&rsquo;t load this page.
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
        ) : detail ? (
          <PageDetailBody
            detail={detail}
            historyTruncated={query.data?.truncated === true}
            useAuthorLabels={useAuthorLabels}
          />
        ) : (
          <div
            className="flex flex-col items-center gap-2 py-16 text-center"
            data-testid="page-not-found"
          >
            <p className="text-base font-medium">Page not found</p>
            <p className="max-w-sm text-sm text-muted-foreground">
              It may have been removed, or it lives in a channel you can&rsquo;t
              read.
            </p>
          </div>
        )}
      </div>
    </div>
  );
}

type PageDetailBodyProps = Pick<PagesHostBindings, "useAuthorLabels"> & {
  detail: PageDetail;
  historyTruncated: boolean;
};

function PageDetailBody({
  detail,
  historyTruncated,
  useAuthorLabels,
}: PageDetailBodyProps) {
  const { channels, nonDmChannelNames } = useChannelNavigation();
  const [selectedRevisionId, setSelectedRevisionId] = React.useState<
    string | null
  >(null);

  const authorPubkeys = React.useMemo(
    () => [
      ...new Set([
        ...(detail.createdBy ? [detail.createdBy] : []),
        ...detail.history.map(({ revision }) => revision.pubkey),
        ...detail.suggestions.map(({ suggestion }) => suggestion.pubkey),
      ]),
    ],
    [detail],
  );
  const authorLabel = useAuthorLabels(authorPubkeys);

  // The selection is only an id: if that revision disappears (deleted, or a
  // newer read no longer contains it) the view falls back to the head instead
  // of rendering stale content.
  const headEntry = detail.history.find((entry) => entry.isHead);
  const viewedEntry =
    detail.history.find((entry) => entry.revision.id === selectedRevisionId) ??
    headEntry;
  const viewed = viewedEntry?.revision ?? detail.head;
  const viewingHead = viewedEntry?.isHead ?? true;
  const deferredContent = React.useDeferredValue(viewed.content);

  const channel = channels.find(
    (candidate) => candidate.id === detail.channelId,
  );
  const channelLabel = channel
    ? channel.channelType === "dm"
      ? channel.name
      : `#${channel.name}`
    : "another channel";
  const revisionCount = detail.history.length;

  return (
    <>
      <PageHeader
        description={
          <>
            in {channelLabel}
            <span aria-hidden> · </span> Updated by{" "}
            {authorLabel(detail.head.pubkey)}{" "}
            <time
              dateTime={new Date(detail.head.createdAt * 1_000).toISOString()}
            >
              {formatItemTimestamp(detail.head.createdAt, { withTime: true })}
            </time>
            {detail.createdBy ? (
              <>
                <span aria-hidden> · </span> Created by{" "}
                {authorLabel(detail.createdBy)}
              </>
            ) : null}
            <span aria-hidden> · </span> {revisionCount}{" "}
            {revisionCount === 1 ? "revision" : "revisions"}
          </>
        }
        title={<span data-testid="page-title">{viewed.title}</span>}
      />

      {viewingHead ? null : (
        <div
          className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-sm"
          data-testid="page-revision-banner"
          role="status"
        >
          <span>
            Viewing an earlier revision by {authorLabel(viewed.pubkey)} from{" "}
            {formatItemTimestamp(viewed.createdAt, { withTime: true })}.
          </span>
          <Button
            data-testid="page-view-current"
            onClick={() => setSelectedRevisionId(null)}
            size="sm"
            type="button"
            variant="outline"
          >
            View current revision
          </Button>
        </div>
      )}

      <div className="grid gap-6 lg:grid-cols-[minmax(0,1fr)_18rem]">
        <article
          aria-label="Page content"
          className="min-w-0 rounded-2xl border border-border/70 bg-muted/20 px-4 py-3"
          data-testid="page-content"
        >
          {viewed.content.trim().length === 0 ? (
            <p className="text-sm text-muted-foreground">
              This revision is empty.
            </p>
          ) : (
            <Markdown
              blockCode
              channelNames={nonDmChannelNames}
              content={deferredContent}
              hardLineBreaks={false}
            />
          )}
        </article>

        <aside className="min-w-0 space-y-6">
          <PendingSuggestions
            authorLabel={authorLabel}
            suggestions={detail.suggestions}
          />
          <HistorySection
            authorLabel={authorLabel}
            detail={detail}
            historyTruncated={historyTruncated}
            onSelect={setSelectedRevisionId}
            selectedId={viewed.id}
          />
        </aside>
      </div>
    </>
  );
}

function HistorySection({
  authorLabel,
  detail,
  historyTruncated,
  onSelect,
  selectedId,
}: {
  authorLabel: (pubkey: string) => string;
  detail: PageDetail;
  historyTruncated: boolean;
  onSelect: (revisionId: string) => void;
  selectedId: string;
}) {
  const headingId = React.useId();
  return (
    <section
      aria-labelledby={headingId}
      className="space-y-2"
      data-testid="page-history"
    >
      <h2 className="text-lg font-semibold tracking-tight" id={headingId}>
        History
      </h2>
      <PageHistoryList
        authorLabel={authorLabel}
        history={detail.history}
        onSelect={onSelect}
        selectedId={selectedId}
      />
      {historyTruncated ? (
        <p
          className="text-sm text-muted-foreground"
          data-testid="page-history-truncated"
        >
          Older revisions aren&rsquo;t shown.
        </p>
      ) : null}
    </section>
  );
}
