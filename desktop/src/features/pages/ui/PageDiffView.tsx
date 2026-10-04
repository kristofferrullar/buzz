import * as React from "react";

import { buildPageDiff, PAGE_DIFF_FILE_NAME } from "../lib/pageDiff";
import type { PagesHostBindings } from "../types";

type PageDiffViewProps = Pick<PagesHostBindings, "DiffViewer"> & {
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
        <p
          className="text-sm text-muted-foreground"
          data-testid="page-diff-too-different"
        >
          These versions differ too much to compare line by line.
        </p>
      )}
    </section>
  );
}
