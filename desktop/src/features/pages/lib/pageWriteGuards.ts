/**
 * Client-side mirrors of the relay's NIP-PG write limits.
 *
 * The relay stays authoritative (`docs/nips/NIP-PG.md`, Relay Rules 4 and 5):
 * these guards only keep an obviously doomed save from leaving the editor, and
 * say why. They are deliberately no stricter than the relay except for one
 * documented case, a title edit that only changes surrounding whitespace.
 */

/** Maximum page title length, in UTF-8 bytes (`MAX_PAGE_TITLE_BYTES`). */
export const PAGE_TITLE_MAX_BYTES = 256;

/** Maximum page content length, in UTF-8 bytes (`MAX_PAGE_CONTENT_BYTES`). */
export const PAGE_CONTENT_MAX_BYTES = 64 * 1024;

const encoder = new TextEncoder();

/** Length of `value` as UTF-8 bytes, which is how the relay measures it. */
export function utf8ByteLength(value: string): number {
  return encoder.encode(value).length;
}

/** The text of a page the user is writing. */
export type PageDraftText = {
  title: string;
  content: string;
};

/** The title that is published: surrounding whitespace is never meaningful. */
export function normalizePageTitle(title: string): string {
  return title.trim();
}

/** Why a draft cannot be published as it stands. */
export type PageDraftIssue =
  | { kind: "title-blank" }
  | { kind: "title-too-long"; bytes: number; max: number }
  | { kind: "content-too-long"; bytes: number; max: number }
  | { kind: "unchanged" };

function sizeIssues(draft: PageDraftText): PageDraftIssue[] {
  const issues: PageDraftIssue[] = [];
  const titleBytes = utf8ByteLength(normalizePageTitle(draft.title));
  if (titleBytes > PAGE_TITLE_MAX_BYTES) {
    issues.push({
      kind: "title-too-long",
      bytes: titleBytes,
      max: PAGE_TITLE_MAX_BYTES,
    });
  }
  const contentBytes = utf8ByteLength(draft.content);
  if (contentBytes > PAGE_CONTENT_MAX_BYTES) {
    issues.push({
      kind: "content-too-long",
      bytes: contentBytes,
      max: PAGE_CONTENT_MAX_BYTES,
    });
  }
  return issues;
}

/**
 * Issues that block saving `draft` as a revision. `head` is the page's current
 * head, or `null` when creating: a revision equal to the head in both title and
 * content is a no-op the relay rejects (Relay Rule 4).
 */
export function validateRevisionDraft(
  draft: PageDraftText,
  head: PageDraftText | null,
): PageDraftIssue[] {
  const issues: PageDraftIssue[] = [];
  if (normalizePageTitle(draft.title).length === 0) {
    issues.push({ kind: "title-blank" });
  }
  issues.push(...sizeIssues(draft));
  if (
    head !== null &&
    normalizePageTitle(draft.title) === normalizePageTitle(head.title) &&
    draft.content === head.content
  ) {
    issues.push({ kind: "unchanged" });
  }
  return issues;
}

/**
 * Issues that block sending `draft` as a suggestion. A suggestion carries
 * content only, so a title edit alone is "unchanged" here; the title is not
 * validated because it is never sent.
 */
export function validateSuggestionDraft(
  draft: PageDraftText,
  head: PageDraftText,
): PageDraftIssue[] {
  const issues: PageDraftIssue[] = [];
  const contentBytes = utf8ByteLength(draft.content);
  if (contentBytes > PAGE_CONTENT_MAX_BYTES) {
    issues.push({
      kind: "content-too-long",
      bytes: contentBytes,
      max: PAGE_CONTENT_MAX_BYTES,
    });
  }
  if (draft.content === head.content) issues.push({ kind: "unchanged" });
  return issues;
}

/** `1.5 KiB`-style size, for limit messages and the content counter. */
export function formatByteSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const kib = bytes / 1024;
  return `${kib >= 10 ? Math.round(kib) : Math.round(kib * 10) / 10} KiB`;
}

/** Human wording for an issue; also what assistive tech reads for the hint. */
export function describePageDraftIssue(issue: PageDraftIssue): string {
  switch (issue.kind) {
    case "title-blank":
      return "Give the page a title.";
    case "title-too-long":
      return `The title is ${issue.bytes} bytes; the limit is ${issue.max}.`;
    case "content-too-long":
      return `The page is ${formatByteSize(issue.bytes)}; the limit is ${formatByteSize(issue.max)}. Shorten it to save.`;
    case "unchanged":
      return "No changes to save yet.";
  }
}
