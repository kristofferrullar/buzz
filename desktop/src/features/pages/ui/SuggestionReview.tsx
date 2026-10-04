import { formatItemTimestamp } from "@/shared/lib/datetime";
import { Button } from "@/shared/ui/button";
import type { PageDetail, SuggestionState } from "../lib/pageModel";
import { resolveSuggestionDiffBase } from "../lib/suggestionReview";
import type { AuthorLabeler, PagesHostBindings } from "../types";
import { PageDiffView } from "./PageDiffView";

type SuggestionReviewProps = Pick<PagesHostBindings, "DiffViewer"> & {
  authorLabel: AuthorLabeler;
  detail: Pick<PageDetail, "head" | "history">;
  onClose: () => void;
  state: SuggestionState;
};

/**
 * The proposed change of one suggestion: its content against the revision it was
 * based on. Shown in place of the page text so the diff has the room it needs;
 * Accept and Reject stay in the rail, where each suggestion owns its own.
 */
export function SuggestionReview({
  DiffViewer,
  authorLabel,
  detail,
  onClose,
  state,
}: SuggestionReviewProps) {
  const { suggestion } = state;
  const base = resolveSuggestionDiffBase(detail, suggestion);

  return (
    <section
      aria-label="Suggested change"
      className="min-w-0 space-y-3 rounded-2xl border border-border/70 bg-muted/20 px-4 py-3"
      data-suggestion-id={suggestion.id}
      data-testid="page-suggestion-review"
    >
      <div className="flex flex-wrap items-start justify-between gap-2">
        <div className="min-w-0 space-y-0.5">
          <h2 className="text-lg font-semibold tracking-tight">
            Suggested change by {authorLabel(suggestion.pubkey)}
          </h2>
          <p
            className="text-sm text-muted-foreground"
            data-testid="page-suggestion-review-base"
          >
            {base.kind === "base"
              ? `Compared with the revision it was based on (${formatItemTimestamp(base.revision.createdAt, { withTime: true })}).`
              : "The revision it was based on isn't loaded, so it is compared with the current page."}
          </p>
        </div>
        <Button
          data-testid="page-suggestion-review-close"
          onClick={onClose}
          size="sm"
          type="button"
          variant="outline"
        >
          Close changes
        </Button>
      </div>
      <PageDiffView
        DiffViewer={DiffViewer}
        label="Proposed changes"
        newContent={suggestion.content}
        oldContent={base.revision.content}
      />
    </section>
  );
}
