import * as React from "react";

import { buildPageDiff, PAGE_DIFF_FILE_NAME } from "../lib/pageDiff";
import type { PagesHostBindings } from "../types";

type PageDiffViewProps = Pick<PagesHostBindings, "DiffViewer"> & {
  /**
   * Shown in full when the versions are too different to diff in time, so a
   * reviewer still sees what they are being asked to accept. Omit where the
   * reader already has the text (the editor).
   */
  fallbackText?: string;
  /** Accessible name of the diff region. */
  label: string;
  newContent: string;
  oldContent: string;
};

/**
 * A line diff between two versions of a page's markdown, drawn by the host's
 * shared diff viewer. The diff is computed here (bounded, normalised; see
 * `buildPageDiff`) and only when this view is mounted, so it costs nothing while
 * a suggestion is collapsed.
 */
export function PageDiffView({
  DiffViewer,
  fallbackText,
  label,
  newContent,
  oldContent,
}: PageDiffViewProps) {
  // Typing in the editor re-renders this on every keystroke; the diff trails it.
  const deferredNew = React.useDeferredValue(newContent);
  const diff = React.useMemo(
    () => buildPageDiff(oldContent, deferredNew),
    [oldContent, deferredNew],
  );

  return (
    <section aria-label={label} className="space-y-2" data-testid="page-diff">
      {diff.status === "changed" ? (
        <>
          <p
            className="text-xs text-muted-foreground"
            data-testid="page-diff-stats"
          >
            <span className="font-medium text-status-added">
              +{diff.additions}
            </span>{" "}
            <span className="font-medium text-status-deleted">
              &minus;{diff.deletions}
            </span>{" "}
            lines
          </p>
          <DiffViewer
            content={diff.patch}
            fallbackFilePath={PAGE_DIFF_FILE_NAME}
          />
        </>
      ) : diff.status === "same" ? (
        <p
          className="text-sm text-muted-foreground"
          data-testid="page-diff-same"
        >
          No differences.
        </p>
      ) : (
        <>
          <p
            className="text-sm text-muted-foreground"
            data-testid="page-diff-too-different"
          >
            These versions differ too much to compare line by line.
            {fallbackText === undefined
              ? ""
              : " The proposed text is shown in full instead."}
          </p>
          {fallbackText === undefined ? null : (
            // Plain text in a text node (never parsed as markup), bounded by the
            // relay's content limit.
            <section
              aria-label="Proposed text"
              className="max-h-96 overflow-auto whitespace-pre-wrap break-words rounded-lg border border-border/60 bg-background/40 p-3 font-mono text-xs outline-hidden focus-visible:ring-1 focus-visible:ring-ring"
              data-testid="page-diff-fallback-text"
              // biome-ignore lint/a11y/noNoninteractiveTabindex: the scrollable text must receive keyboard focus
              tabIndex={0}
            >
              {fallbackText}
            </section>
          )}
        </>
      )}
    </section>
  );
}
