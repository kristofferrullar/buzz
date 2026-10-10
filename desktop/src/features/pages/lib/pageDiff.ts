/**
 * Diff input preparation for the suggestion review and the editor's "Changes"
 * pane.
 *
 * The app already renders unified diffs (`DiffViewer`, built on
 * `react-diff-view`), so this module only turns two markdown strings into the
 * unified-diff text that viewer takes, using `diff` (jsdiff) for the line diff
 * rather than a hand-rolled one. Two inputs would otherwise mislead:
 *
 * - CRLF vs LF, which would mark every line changed; and
 * - a missing final newline, which jsdiff reports as a changed last line plus a
 *   "no newline" marker the viewer does not understand.
 *
 * The diff is bounded: a wholesale rewrite of two 64 KiB documents is a
 * pathological case for any line diff, so the computation is time-boxed and
 * reports `too-different` instead of freezing the window.
 */
import { FILE_HEADERS_ONLY, formatPatch, structuredPatch } from "diff";

/**
 * File name written into the patch headers. The host passes the same name as
 * the viewer's `fallbackFilePath` so the per-file header is collapsed.
 */
export const PAGE_DIFF_FILE_NAME = "page.md";

/** Lines of unchanged context around each hunk. */
const CONTEXT_LINES = 3;

/** Upper bound on one diff computation, in milliseconds. */
export const PAGE_DIFF_TIMEOUT_MS = 400;

export type PageDiff =
  /** Identical after normalisation: there is nothing to show. */
  | { status: "same" }
  | {
      status: "changed";
      /** Unified diff text for `DiffViewer`. */
      patch: string;
      additions: number;
      deletions: number;
    }
  /** The line diff did not finish inside its time box. */
  | { status: "too-different" };

/**
 * Normalise line endings and the final newline so only real content changes
 * show up as changed lines.
 */
export function normalizeForDiff(text: string): string {
  const unix = text.replace(/\r\n?/g, "\n");
  return unix.length === 0 || unix.endsWith("\n") ? unix : `${unix}\n`;
}

/**
 * Unified diff from `oldContent` to `newContent`, or why there is none.
 * `timeoutMs` exists for tests; production uses the default.
 */
export function buildPageDiff(
  oldContent: string,
  newContent: string,
  { timeoutMs = PAGE_DIFF_TIMEOUT_MS }: { timeoutMs?: number } = {},
): PageDiff {
  const oldText = normalizeForDiff(oldContent);
  const newText = normalizeForDiff(newContent);
  if (oldText === newText) return { status: "same" };

  const patch = structuredPatch(
    PAGE_DIFF_FILE_NAME,
    PAGE_DIFF_FILE_NAME,
    oldText,
    newText,
    "",
    "",
    { context: CONTEXT_LINES, timeout: timeoutMs },
  );
  if (patch === undefined) return { status: "too-different" };
  if (patch.hunks.length === 0) return { status: "same" };

  let additions = 0;
  let deletions = 0;
  for (const hunk of patch.hunks) {
    for (const line of hunk.lines) {
      if (line.startsWith("+")) additions += 1;
      else if (line.startsWith("-")) deletions += 1;
    }
  }
  return {
    status: "changed",
    patch: formatPatch(patch, FILE_HEADERS_ONLY),
    additions,
    deletions,
  };
}
