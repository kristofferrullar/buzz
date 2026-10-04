/**
 * Local autosave of an unsaved page draft.
 *
 * A convenience, never the source of truth: the relay holds the page, and a
 * draft that is lost (private window, cleared site data, blocked storage, a full
 * quota) only costs the user their unsaved typing. So every access is wrapped,
 * a failure degrades to "no autosave" and nothing here ever throws into the
 * editor. The record is keyed by community and page, versioned, shape-checked on
 * read (it is untrusted input from disk), and size-capped on write.
 */
import type { PageDraftText } from "./pageWriteGuards";

const STORAGE_PREFIX = "buzz.pages.draft.v1";

/**
 * Upper bound on a stored draft, in UTF-16 code units. The relay limit is
 * 64 KiB of content; this leaves room for the title and JSON escaping while
 * keeping one draft well inside the webview's localStorage quota.
 */
export const MAX_STORED_DRAFT_CHARS = 256 * 1024;

/** Minimal slice of `Storage` the drafts use, so tests can pass a fake. */
export type DraftStorage = Pick<Storage, "getItem" | "removeItem" | "setItem">;

export type StoredPageDraft = PageDraftText & {
  /** Revision the draft was written against; `null` for a page being created. */
  baseRevisionId: string | null;
  /** Channel chosen for a page being created. */
  channelId: string | null;
  /** Unix seconds when the draft was last stored. */
  savedAt: number;
};

/**
 * Storage key for a draft. `pageId` is `null` for the one in-progress new page
 * per community. `JSON.stringify` keeps the parts unambiguous for any id.
 */
export function pageDraftKey(
  communityId: string | null,
  channelId: string | null,
  pageId: string | null,
): string {
  return `${STORAGE_PREFIX}:${JSON.stringify([communityId, channelId, pageId])}`;
}

/** The browser's `localStorage`, or `null` where touching it throws. */
export function browserDraftStorage(): DraftStorage | null {
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

function parseStoredDraft(raw: string): StoredPageDraft | null {
  let value: unknown;
  try {
    value = JSON.parse(raw);
  } catch {
    return null;
  }
  if (typeof value !== "object" || value === null) return null;
  const record = value as Record<string, unknown>;
  if (
    record.v !== 1 ||
    typeof record.title !== "string" ||
    typeof record.content !== "string" ||
    typeof record.savedAt !== "number"
  ) {
    return null;
  }
  const baseRevisionId =
    typeof record.baseRevisionId === "string" ? record.baseRevisionId : null;
  const channelId =
    typeof record.channelId === "string" ? record.channelId : null;
  return {
    title: record.title,
    content: record.content,
    baseRevisionId,
    channelId,
    savedAt: record.savedAt,
  };
}

/** Read a stored draft; `null` when absent, unreadable or malformed. */
export function readPageDraft(
  storage: DraftStorage | null,
  key: string,
): StoredPageDraft | null {
  if (!storage) return null;
  try {
    const raw = storage.getItem(key);
    return raw === null ? null : parseStoredDraft(raw);
  } catch {
    return null;
  }
}

/** Store a draft. Returns false when it was not stored (too large, quota, blocked). */
export function writePageDraft(
  storage: DraftStorage | null,
  key: string,
  draft: StoredPageDraft,
): boolean {
  if (!storage) return false;
  if (draft.title.length + draft.content.length > MAX_STORED_DRAFT_CHARS) {
    return false;
  }
  try {
    storage.setItem(key, JSON.stringify({ v: 1, ...draft }));
    return true;
  } catch {
    return false;
  }
}

/** Remove a stored draft; a failure to remove is not worth surfacing. */
export function clearPageDraft(
  storage: DraftStorage | null,
  key: string,
): void {
  if (!storage) return;
  try {
    storage.removeItem(key);
  } catch {
    // Nothing to do: the next successful write or clear replaces it.
  }
}

/**
 * Remove the stored draft only if it still holds exactly `text`. A save that
 * finishes after the user moved on (reopened the editor, typed more) must not
 * delete the newer draft.
 */
export function clearPageDraftIfSaved(
  storage: DraftStorage | null,
  key: string,
  text: PageDraftText,
): void {
  const stored = readPageDraft(storage, key);
  if (
    stored &&
    stored.title === text.title &&
    stored.content === text.content
  ) {
    clearPageDraft(storage, key);
  }
}
