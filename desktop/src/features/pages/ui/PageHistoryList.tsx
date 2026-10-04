import { cn } from "@/shared/lib/cn";
import { formatItemTimestamp } from "@/shared/lib/datetime";
import { Badge } from "@/shared/ui/badge";
import type { PageHistoryEntry } from "../lib/pageModel";
import type { AuthorLabeler } from "../types";
import { ROVING_ITEM_PROPS, useRovingList } from "./useRovingList";

type PageHistoryListProps = {
  authorLabel: AuthorLabeler;
  history: readonly PageHistoryEntry[];
  /** Id of the revision currently rendered in the page body. */
  selectedId: string;
  onSelect: (revisionId: string) => void;
};

/**
 * Read-only revision history, newest first. Selecting a row shows that
 * revision's rendered content; it never changes the page. One native button
 * per row; the selected row carries `aria-current`.
 */
export function PageHistoryList({
  authorLabel,
  history,
  onSelect,
  selectedId,
}: PageHistoryListProps) {
  const { listProps, onRowFocus, tabStopIndex } = useRovingList(history.length);

  return (
    <ul
      aria-label="Revisions"
      className="space-y-1.5"
      data-testid="page-history-list"
      {...listProps}
    >
      {history.map(({ isHead, onHeadChain, revision }, index) => {
        const selected = revision.id === selectedId;
        return (
          <li key={revision.id}>
            <button
              {...ROVING_ITEM_PROPS}
              aria-current={selected ? "true" : undefined}
              className={cn(
                "flex w-full min-w-0 flex-col gap-1 rounded-lg border px-3 py-2 text-left transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
                selected
                  ? "border-primary/50 bg-primary/10"
                  : "border-border/70 bg-muted/20 hover:bg-muted/40",
              )}
              data-revision-id={revision.id}
              data-testid="page-history-row"
              onClick={() => onSelect(revision.id)}
              onFocus={() => onRowFocus(index)}
              tabIndex={index === tabStopIndex ? 0 : -1}
              type="button"
            >
              <span className="block truncate text-sm font-medium">
                {revision.title}
              </span>
              <span className="block truncate text-xs text-muted-foreground">
                {authorLabel(revision.pubkey)}
                <span aria-hidden> · </span>{" "}
                <time
                  dateTime={new Date(revision.createdAt * 1_000).toISOString()}
                >
                  {formatItemTimestamp(revision.createdAt, { withTime: true })}
                </time>
              </span>
              {isHead || revision.suggestionId || !onHeadChain ? (
                <span className="flex flex-wrap gap-1">
                  {isHead ? <Badge variant="success">Current</Badge> : null}
                  {revision.suggestionId ? (
                    <Badge variant="info">Applied suggestion</Badge>
                  ) : null}
                  {!onHeadChain ? (
                    <Badge variant="outline">Other branch</Badge>
                  ) : null}
                </span>
              ) : null}
            </button>
          </li>
        );
      })}
    </ul>
  );
}
