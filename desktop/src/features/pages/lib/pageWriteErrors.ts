/**
 * Maps a rejected page write to a distinct, actionable outcome.
 *
 * The relay answers every page rejection with a stable `<prefix>: <reason>`
 * string (`conflict:`, `invalid:`, `restricted:`; see `docs/nips/NIP-PG.md`),
 * and the Tauri layer wraps it (`relay rejected event: ...`,
 * `relay returned 4xx: ...`, `relay unreachable: ...`). Classification looks for
 * the prefixed token anywhere in the text, never for an exact string, so a
 * wrapper change cannot turn a conflict into an anonymous failure. An
 * unrecognised message is shown verbatim: a rejection is never swallowed or
 * rewritten into success.
 */
import {
  PAGE_CONTENT_MAX_BYTES,
  PAGE_TITLE_MAX_BYTES,
  formatByteSize,
} from "./pageWriteGuards";

/** What the editor or rail should do about a failed write. */
export type PageWriteErrorKind =
  /** The head moved (or the page already exists): reload and re-apply. */
  | "conflict"
  /** The suggestion was closed or applied by someone else first. */
  | "suggestion-closed"
  /** The page was deleted; nothing can be saved to it. */
  | "page-gone"
  | "too-large"
  | "no-change"
  | "permission"
  | "archived"
  | "rate-limited"
  | "offline"
  | "unknown";

/** Finer detail of a `conflict`, because the right recovery differs. */
export type PageConflictReason =
  /** `prev` is no longer the head (or is not known): someone else saved. */
  | "head-moved"
  /** Another writer holds the page right now; the same save can be retried. */
  | "busy"
  /** A page with this id exists already (a retried create). */
  | "page-exists"
  | "other";

export type PageWriteError = {
  kind: PageWriteErrorKind;
  /** Set when `kind` is `conflict`. */
  conflictReason?: PageConflictReason;
  /** Actionable text for the user. */
  message: string;
  /** The raw rejection text, for support; never the only thing shown. */
  detail: string;
};

const KEEP_DRAFT = "Your draft is still here.";

function conflictReasonOf(text: string): PageConflictReason {
  if (/page is busy|busy with another writer/i.test(text)) return "busy";
  if (/page already exists/i.test(text)) return "page-exists";
  if (/stale prev|prev revision not found/i.test(text)) return "head-moved";
  return "other";
}

/**
 * Wording for a failed accept or reject. `PageWriteError.message` speaks to an
 * editor with a draft ("Your draft is still here"); a reviewer has none, so the
 * same outcomes are described in terms of the suggestion instead.
 */
export function describeDecisionFailure(error: PageWriteError): string {
  switch (error.kind) {
    case "conflict":
    case "suggestion-closed":
      return "Someone else changed the page or handled this suggestion first, so it can't be applied. The list is up to date now.";
    case "page-gone":
      return "This page was deleted, so the suggestion can't be applied.";
    case "no-change":
      return "The page already matches this suggestion. Reject it to close it.";
    case "too-large":
      return `Applying this would put the page over its size limit (${formatByteSize(PAGE_CONTENT_MAX_BYTES)}).`;
    case "permission":
      return "You don't have permission to change pages in this channel.";
    case "archived":
      return "This channel is archived, so its pages can't be changed.";
    case "rate-limited":
      return "You're acting too quickly. Wait a moment and try again.";
    case "offline":
      return "Can't reach the relay. Try again when you're back online.";
    case "unknown":
      return `Couldn't complete that: ${error.detail}`;
  }
}

/** The relay's reason with its `restricted:`-style prefix and any wrapper removed. */
function relayReason(detail: string): string {
  const match =
    /\b(?:restricted|blocked|auth-required|forbidden|error):\s*(.+)$/i.exec(
      detail,
    );
  return match?.[1] ?? detail;
}

/** Message text of an unknown thrown value (`Error`, string, `{message}`). */
export function writeFailureText(error: unknown): string {
  if (error instanceof Error) return error.message;
  if (typeof error === "string") return error;
  if (
    typeof error === "object" &&
    error !== null &&
    "message" in error &&
    typeof error.message === "string"
  ) {
    return error.message;
  }
  return "Unknown error";
}

/** Classify a thrown write failure. Pure; the input is untrusted relay text. */
export function classifyPageWriteError(error: unknown): PageWriteError {
  const detail = writeFailureText(error);
  const make = (
    kind: PageWriteErrorKind,
    message: string,
    conflictReason?: PageConflictReason,
  ): PageWriteError =>
    conflictReason === undefined
      ? { kind, message, detail }
      : { kind, message, detail, conflictReason };

  // Connectivity first: it is also a prefix the Tauri layer owns.
  if (detail.includes("relay unreachable:")) {
    return make(
      "offline",
      `Can't reach the relay. ${KEEP_DRAFT} Try again when you're back online.`,
    );
  }
  if (/relay rate-limited:|\brate-limited:/i.test(detail)) {
    return make(
      "rate-limited",
      `You're sending changes too quickly. ${KEEP_DRAFT} Wait a moment and try again.`,
    );
  }

  if (/(^|[\s:])conflict:/.test(detail)) {
    if (/page is deleted|page does not exist/i.test(detail)) {
      return make(
        "page-gone",
        `This page was deleted, so it can't be changed. Copy your text before you close the editor.`,
      );
    }
    if (/suggestion is /i.test(detail)) {
      return make(
        "suggestion-closed",
        "Someone else already handled this suggestion, or the page has changed since it was made. The list is up to date now.",
      );
    }
    const reason = conflictReasonOf(detail);
    if (reason === "busy") {
      return make(
        "conflict",
        `Another change to this page was being saved at the same moment. ${KEEP_DRAFT} Try again.`,
        reason,
      );
    }
    if (reason === "page-exists") {
      return make(
        "conflict",
        "A page with this ID already exists. An earlier save probably went through.",
        reason,
      );
    }
    return make(
      "conflict",
      `Someone else changed this page while you were editing. ${KEEP_DRAFT}`,
      reason,
    );
  }

  if (/no-op revision|\bno-op\b|no changes/i.test(detail)) {
    return make(
      "no-change",
      "Nothing changed: this matches the current version of the page.",
    );
  }
  if (/\bexceeds\b/i.test(detail) && /\btitle\b/i.test(detail)) {
    return make(
      "too-large",
      `The title is over the ${PAGE_TITLE_MAX_BYTES}-byte limit. Shorten it and save again. ${KEEP_DRAFT}`,
    );
  }
  if (/\bexceeds\b|too large|too long|oversize/i.test(detail)) {
    return make(
      "too-large",
      `The page is over the ${formatByteSize(PAGE_CONTENT_MAX_BYTES)} limit. Shorten it and save again. ${KEEP_DRAFT}`,
    );
  }
  if (/channel is archived|\barchived\b/i.test(detail)) {
    return make(
      "archived",
      `This channel is archived, so its pages can't be changed. Copy your text if you want to keep it.`,
    );
  }
  if (
    /\b(restricted|blocked|auth-required|forbidden):|not a channel member|permission/i.test(
      detail,
    )
  ) {
    return make(
      "permission",
      `You don't have permission to change pages in this channel (${relayReason(detail)}). ${KEEP_DRAFT}`,
    );
  }

  return make("unknown", `Couldn't complete that: ${detail}. ${KEEP_DRAFT}`);
}
