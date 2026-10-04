/**
 * Pure NIP-PG page model: parses page events and derives head, history and
 * suggestion state from them.
 *
 * Nothing here touches the relay, React or the clock, so every derivation rule
 * in `docs/nips/NIP-PG.md` is unit-testable in isolation. The relay stores
 * regular, append-only events; "head", "stale" and "closed" are all projections
 * the client computes from the log, never stored fields.
 */
import type { RelayEvent } from "@/shared/api/types";
import {
  KIND_PAGE_REVISION,
  KIND_PAGE_SUGGESTION,
  KIND_PAGE_SUGGESTION_RESOLUTION,
} from "@/shared/constants/kinds";

/** The three NIP-PG event kinds a page query must always name explicitly. */
export const PAGE_EVENT_KINDS: readonly number[] = [
  KIND_PAGE_REVISION,
  KIND_PAGE_SUGGESTION,
  KIND_PAGE_SUGGESTION_RESOLUTION,
];

/** A page is identified by the pair `(h, d)`: its channel and its page id. */
export type PageIdentity = {
  channelId: string;
  pageId: string;
};

/** A stored `PAGE_REVISION` (kind 52000). */
export type PageRevision = PageIdentity & {
  id: string;
  pubkey: string;
  createdAt: number;
  /** Id of the revision this one was based on; `null` on a page's first revision. */
  prev: string | null;
  title: string;
  /** Markdown, UTF-8. */
  content: string;
  /** Id of the suggestion this revision applied, if any. */
  suggestionId: string | null;
};

/** A stored `PAGE_SUGGESTION` (kind 52001). */
export type PageSuggestion = PageIdentity & {
  id: string;
  pubkey: string;
  createdAt: number;
  /** Id of the revision the suggestion edits. */
  base: string;
  /** Proposed full markdown. */
  content: string;
};

/** A stored `PAGE_SUGGESTION_RESOLUTION` (kind 52002). */
export type PageResolution = PageIdentity & {
  id: string;
  pubkey: string;
  createdAt: number;
  suggestionId: string;
  status: "accepted" | "rejected";
  /** Resulting revision id; present when accepted. */
  revisionId: string | null;
};

export type ParsedPageEvents = {
  revisions: PageRevision[];
  suggestions: PageSuggestion[];
  resolutions: PageResolution[];
};

/** Stable map key for a page. JSON keeps `(h, d)` unambiguous for any `d`. */
export function pageKey({ channelId, pageId }: PageIdentity): string {
  return JSON.stringify([channelId, pageId]);
}

/** Newest first; equal timestamps order by ascending event id (relay order). */
export function compareNewestFirst(
  left: { createdAt: number; id: string },
  right: { createdAt: number; id: string },
): number {
  return right.createdAt - left.createdAt || left.id.localeCompare(right.id);
}

function singleTagValue(tags: string[][], name: string): string | null {
  let found: string | null = null;
  for (const tag of tags) {
    if (tag[0] !== name) continue;
    // NIP-PG requires exactly one `h` and one `d`; a second value is malformed.
    if (found !== null) return null;
    const value = tag[1];
    if (typeof value !== "string" || value.length === 0) return null;
    found = value;
  }
  return found;
}

function identityOf(event: RelayEvent): PageIdentity | null {
  const channelId = singleTagValue(event.tags, "h");
  const pageId = singleTagValue(event.tags, "d");
  return channelId && pageId ? { channelId, pageId } : null;
}

/**
 * Parse a `PAGE_REVISION` event. Returns `null` for anything that breaks the
 * NIP-PG shape (wrong kind, missing or duplicate `h`/`d`, blank title) so a
 * hostile or foreign event can never reach the head derivation.
 */
export function parsePageRevision(event: RelayEvent): PageRevision | null {
  if (event.kind !== KIND_PAGE_REVISION) return null;
  const identity = identityOf(event);
  if (!identity) return null;
  const title = event.tags.find((tag) => tag[0] === "title")?.[1];
  if (typeof title !== "string" || title.trim().length === 0) return null;
  const prev = event.tags.find((tag) => tag[0] === "prev")?.[1];
  const suggestionId = event.tags.find((tag) => tag[0] === "suggestion")?.[1];
  return {
    ...identity,
    id: event.id,
    pubkey: event.pubkey,
    createdAt: event.created_at,
    prev: prev ? prev : null,
    title,
    content: event.content,
    suggestionId: suggestionId ? suggestionId : null,
  };
}

/** Parse a `PAGE_SUGGESTION` event; `null` when it breaks the NIP-PG shape. */
export function parsePageSuggestion(event: RelayEvent): PageSuggestion | null {
  if (event.kind !== KIND_PAGE_SUGGESTION) return null;
  const identity = identityOf(event);
  if (!identity) return null;
  const base = event.tags.find((tag) => tag[0] === "base")?.[1];
  if (!base) return null;
  return {
    ...identity,
    id: event.id,
    pubkey: event.pubkey,
    createdAt: event.created_at,
    base,
    content: event.content,
  };
}

/** Parse a `PAGE_SUGGESTION_RESOLUTION` event; `null` when malformed. */
export function parsePageResolution(event: RelayEvent): PageResolution | null {
  if (event.kind !== KIND_PAGE_SUGGESTION_RESOLUTION) return null;
  const identity = identityOf(event);
  if (!identity) return null;
  const suggestionId = event.tags.find((tag) => tag[0] === "e")?.[1];
  const status = event.tags.find((tag) => tag[0] === "status")?.[1];
  if (!suggestionId) return null;
  if (status !== "accepted" && status !== "rejected") return null;
  const revisionId = event.tags.find((tag) => tag[0] === "rev")?.[1];
  return {
    ...identity,
    id: event.id,
    pubkey: event.pubkey,
    createdAt: event.created_at,
    suggestionId,
    status,
    revisionId: revisionId ? revisionId : null,
  };
}

/**
 * Parse a mixed batch of page events, dropping malformed events and
 * de-duplicating by event id (paged history re-returns boundary rows).
 */
export function parsePageEvents(
  events: readonly RelayEvent[],
): ParsedPageEvents {
  const revisions = new Map<string, PageRevision>();
  const suggestions = new Map<string, PageSuggestion>();
  const resolutions = new Map<string, PageResolution>();
  for (const event of events) {
    const revision = parsePageRevision(event);
    if (revision) {
      revisions.set(revision.id, revision);
      continue;
    }
    const suggestion = parsePageSuggestion(event);
    if (suggestion) {
      suggestions.set(suggestion.id, suggestion);
      continue;
    }
    const resolution = parsePageResolution(event);
    if (resolution) resolutions.set(resolution.id, resolution);
  }
  return {
    revisions: [...revisions.values()],
    suggestions: [...suggestions.values()],
    resolutions: [...resolutions.values()],
  };
}

function isBetterHead(candidate: PageRevision, current: PageRevision): boolean {
  return (
    candidate.createdAt > current.createdAt ||
    (candidate.createdAt === current.createdAt && candidate.id < current.id)
  );
}

/**
 * The head of one page's revisions, per NIP-PG: the newest revision in the
 * `prev` chain. A tip is a revision no other revision names as its `prev`.
 * Normally there is exactly one; imports, restores or events from another relay
 * can produce several, and then the head is the tip with the greatest
 * `created_at`, ties broken by the lowest event id. A deleted head simply is
 * not in `revisions`, which makes its `prev` the new tip.
 *
 * `revisions` must all belong to the same `(h, d)`.
 */
export function selectHead(
  revisions: readonly PageRevision[],
): PageRevision | null {
  if (revisions.length === 0) return null;
  const referenced = new Set<string>();
  for (const revision of revisions) {
    if (revision.prev) referenced.add(revision.prev);
  }
  let head: PageRevision | null = null;
  for (const revision of revisions) {
    if (referenced.has(revision.id)) continue;
    if (head === null || isBetterHead(revision, head)) head = revision;
  }
  if (head) return head;
  // Every revision is named as someone's `prev`: only possible with corrupt
  // data. Fall back to the newest revision rather than hiding the page.
  return [...revisions].sort(compareNewestFirst)[0] ?? null;
}

/** Ids on the `prev` chain from `head` back to the root (or the window edge). */
export function headChainIds(
  revisions: readonly PageRevision[],
  head: PageRevision,
): Set<string> {
  const byId = new Map(revisions.map((revision) => [revision.id, revision]));
  const chain = new Set<string>();
  let cursor: PageRevision | undefined = head;
  // `chain` doubles as a cycle guard, so a corrupt log cannot loop forever.
  while (cursor && !chain.has(cursor.id)) {
    chain.add(cursor.id);
    cursor = cursor.prev ? byId.get(cursor.prev) : undefined;
  }
  return chain;
}

/** One row of the library: a page's head projected for listing. */
export type PageSummary = PageIdentity & {
  key: string;
  title: string;
  headId: string;
  /** `created_at` of the head revision. */
  updatedAt: number;
  /** Author of the head revision. */
  updatedBy: string;
  /** Revisions seen for this page in the queried window. */
  revisionCount: number;
};

/**
 * Group revisions by `(h, d)` and project each page's head. Newest-updated
 * first; equal timestamps order by page key so the list is deterministic.
 */
export function buildLibrary(
  revisions: readonly PageRevision[],
): PageSummary[] {
  const groups = new Map<string, PageRevision[]>();
  for (const revision of revisions) {
    const key = pageKey(revision);
    const group = groups.get(key);
    if (group) group.push(revision);
    else groups.set(key, [revision]);
  }
  const summaries: PageSummary[] = [];
  for (const [key, group] of groups) {
    const head = selectHead(group);
    if (!head) continue;
    summaries.push({
      key,
      channelId: head.channelId,
      pageId: head.pageId,
      title: head.title,
      headId: head.id,
      updatedAt: head.createdAt,
      updatedBy: head.pubkey,
      revisionCount: group.length,
    });
  }
  return summaries.sort(
    (left, right) =>
      right.updatedAt - left.updatedAt || left.key.localeCompare(right.key),
  );
}

/** How a suggestion was closed: by a resolution event or by an applying revision. */
export type SuggestionClosure = "resolution" | "revision";

/**
 * Suggestion ids that are closed, per NIP-PG: a resolution (accepted or
 * rejected, by any writer) references it, or a stored revision carries its
 * `suggestion` tag. A resolution wins when both exist.
 */
export function closedSuggestions(
  resolutions: readonly PageResolution[],
  revisions: readonly PageRevision[],
): Map<string, SuggestionClosure> {
  const closed = new Map<string, SuggestionClosure>();
  for (const revision of revisions) {
    if (revision.suggestionId) closed.set(revision.suggestionId, "revision");
  }
  for (const resolution of resolutions) {
    closed.set(resolution.suggestionId, "resolution");
  }
  return closed;
}

export type SuggestionState = {
  suggestion: PageSuggestion;
  closed: SuggestionClosure | null;
  /** The suggestion's `base` is no longer the page head. */
  stale: boolean;
};

/** Derive each suggestion's closed/stale state against the page head. */
export function deriveSuggestionStates({
  headId,
  resolutions,
  revisions,
  suggestions,
}: {
  headId: string;
  resolutions: readonly PageResolution[];
  revisions: readonly PageRevision[];
  suggestions: readonly PageSuggestion[];
}): SuggestionState[] {
  const closed = closedSuggestions(resolutions, revisions);
  return [...suggestions].sort(compareNewestFirst).map((suggestion) => ({
    suggestion,
    closed: closed.get(suggestion.id) ?? null,
    stale: suggestion.base !== headId,
  }));
}

/** One revision in a page's history list. */
export type PageHistoryEntry = {
  revision: PageRevision;
  isHead: boolean;
  /** False for revisions on a tip other than the head's `prev` chain. */
  onHeadChain: boolean;
};

/** Everything the page view renders, derived from one page's events. */
export type PageDetail = PageIdentity & {
  key: string;
  head: PageRevision;
  /** Newest first. */
  history: PageHistoryEntry[];
  /** Author and time of the root revision, when it is in the queried window. */
  createdBy: string | null;
  createdAt: number | null;
  /** Every suggestion seen, newest first, each with its closed/stale state. */
  suggestions: SuggestionState[];
};

/**
 * Derive a page's detail from the events of exactly one `(h, d)`. Events that
 * belong to another page are ignored rather than trusted. Returns `null` when
 * the page has no revision (unknown page, or none the viewer can read).
 */
export function buildPageDetail(
  identity: PageIdentity,
  events: readonly RelayEvent[],
): PageDetail | null {
  const parsed = parsePageEvents(events);
  const sameChannelAndPage = (candidate: PageIdentity) =>
    candidate.channelId === identity.channelId &&
    candidate.pageId === identity.pageId;
  const revisions = parsed.revisions.filter(sameChannelAndPage);
  const head = selectHead(revisions);
  if (!head) return null;
  const chain = headChainIds(revisions, head);
  const history = [...revisions].sort(compareNewestFirst).map((revision) => ({
    revision,
    isHead: revision.id === head.id,
    onHeadChain: chain.has(revision.id),
  }));
  const root = revisions
    .filter((revision) => revision.prev === null)
    // Several roots: the earliest, ties to the lowest event id (NIP-PG).
    .sort(
      (left, right) =>
        left.createdAt - right.createdAt || left.id.localeCompare(right.id),
    )[0];
  return {
    channelId: identity.channelId,
    pageId: identity.pageId,
    key: pageKey(identity),
    head,
    history,
    createdBy: root?.pubkey ?? null,
    createdAt: root?.createdAt ?? null,
    suggestions: deriveSuggestionStates({
      headId: head.id,
      resolutions: parsed.resolutions.filter(sameChannelAndPage),
      revisions,
      suggestions: parsed.suggestions.filter(sameChannelAndPage),
    }),
  };
}
