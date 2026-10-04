/**
 * Mock-mode NIP-PG page events for the e2e bridge.
 *
 * Extracted from `e2eBridge.ts` so the bridge only registers it: the seeded
 * pages, a relay-faithful `query(filter)` and a `push(event)` for live updates.
 * The desktop reads pages with ordinary REQ filters, so the mock answers those
 * exact filters the way the relay will: `kinds` always present, `#h`/`#d`
 * exact, an inclusive `until`, newest-first (`created_at DESC, id ASC`), and a
 * `limit`. A filter with no `#h` returns every channel the mock viewer can
 * read, as the relay's accessible-channel scoping does.
 */
import type { RelayEvent } from "../shared/api/types.ts";

const KIND_PAGE_REVISION = 52000;
const KIND_PAGE_SUGGESTION = 52001;
const KIND_PAGE_SUGGESTION_RESOLUTION = 52002;

/** Kinds the mock serves from the page store instead of channel history. */
export const MOCK_PAGE_KINDS: ReadonlySet<number> = new Set([
  KIND_PAGE_REVISION,
  KIND_PAGE_SUGGESTION,
  KIND_PAGE_SUGGESTION_RESOLUTION,
]);

/** Channels the seeded pages live in (ids match the bridge's channel list). */
export const MOCK_PAGE_CHANNEL_IDS = {
  general: "9a1657ac-f7aa-5db0-b632-d8bbeb6dfb50",
  engineering: "1c7e1c02-87bb-5e88-b2da-5a7a9432d0c9",
  design: "b5e2f8a1-3c44-5912-9e67-4a8d1f2b3c4e",
} as const;

/** Page ids (`d` tags) of the seeded pages. */
export const MOCK_PAGE_IDS = {
  q4Plan: "0a000000-0000-4000-8000-0000000000a1",
  runbook: "0a000000-0000-4000-8000-0000000000a2",
  designNotes: "0a000000-0000-4000-8000-0000000000a3",
  meetingNotes: "0a000000-0000-4000-8000-0000000000a4",
} as const;

/** Event ids the specs refer to by name. */
export const MOCK_PAGE_EVENT_IDS = {
  q4Draft: "e1".repeat(32),
  q4Second: "e2".repeat(32),
  q4Head: "e3".repeat(32),
  q4StaleSuggestion: "51".repeat(32),
  q4FreshSuggestion: "52".repeat(32),
  q4AppliedSuggestion: "53".repeat(32),
  q4RejectedSuggestion: "54".repeat(32),
  runbookFirst: "e4".repeat(32),
  runbookHead: "e5".repeat(32),
  designNotesOnly: "e6".repeat(32),
  meetingRoot: "e7".repeat(32),
  // Two tips off one parent with equal `created_at`: the lowest id is the head.
  meetingTipLowId: "0a".repeat(32),
  meetingTipHighId: "0b".repeat(32),
} as const;

export type MockPageAuthors = {
  alice: string;
  agent: string;
  bob: string;
  viewer: string;
};

export type MockPageStore = {
  /** Answer a REQ filter the way the relay answers a page query. */
  query: (filter: MockPageFilter) => RelayEvent[];
  /** Store an event so later queries return it. */
  push: (event: RelayEvent) => void;
};

export type MockPageFilter = {
  "#d"?: string[];
  "#h"?: string[];
  kinds?: number[];
  limit?: number;
  since?: number;
  until?: number;
};

const SIG = "mocksig".repeat(20).slice(0, 128);

function event(
  id: string,
  kind: number,
  pubkey: string,
  createdAt: number,
  tags: string[][],
  content: string,
): RelayEvent {
  return {
    id,
    pubkey,
    created_at: createdAt,
    kind,
    tags,
    content,
    sig: SIG,
  };
}

function revision(input: {
  authorPubkey: string;
  channelId: string;
  content: string;
  createdAt: number;
  id: string;
  pageId: string;
  prev?: string;
  suggestion?: string;
  title: string;
}): RelayEvent {
  const tags = [
    ["h", input.channelId],
    ["d", input.pageId],
    ["title", input.title],
  ];
  if (input.prev) tags.push(["prev", input.prev]);
  if (input.suggestion) tags.push(["suggestion", input.suggestion]);
  return event(
    input.id,
    KIND_PAGE_REVISION,
    input.authorPubkey,
    input.createdAt,
    tags,
    input.content,
  );
}

function suggestion(input: {
  authorPubkey: string;
  base: string;
  channelId: string;
  content: string;
  createdAt: number;
  id: string;
  pageId: string;
}): RelayEvent {
  return event(
    input.id,
    KIND_PAGE_SUGGESTION,
    input.authorPubkey,
    input.createdAt,
    [
      ["h", input.channelId],
      ["d", input.pageId],
      ["base", input.base],
    ],
    input.content,
  );
}

/** The seeded pages: a linear history, a tie between two tips, and suggestions. */
export function createMockPageEvents(
  authors: MockPageAuthors,
  nowSeconds: number,
): RelayEvent[] {
  const ids = MOCK_PAGE_EVENT_IDS;
  const { general, engineering, design } = MOCK_PAGE_CHANNEL_IDS;
  const ago = (seconds: number) => nowSeconds - seconds;
  const HOUR = 3_600;
  const DAY = 24 * HOUR;

  const q4HeadContent =
    "# Q4 Plan\n\n## Goals\n\n- Ship Space\n- Grow agent usage\n\n## Risks\n\n- Relay capacity";

  return [
    // Q4 Plan: three linear revisions; the head applied a suggestion.
    revision({
      authorPubkey: authors.alice,
      channelId: general,
      content: "# Q4 Plan\n\nDraft outline.",
      createdAt: ago(3 * DAY),
      id: ids.q4Draft,
      pageId: MOCK_PAGE_IDS.q4Plan,
      title: "Q4 Plan (draft)",
    }),
    revision({
      authorPubkey: authors.alice,
      channelId: general,
      content: "# Q4 Plan\n\n## Goals\n\n- Ship Space\n- Grow agent usage",
      createdAt: ago(2 * DAY),
      id: ids.q4Second,
      pageId: MOCK_PAGE_IDS.q4Plan,
      prev: ids.q4Draft,
      title: "Q4 Plan",
    }),
    revision({
      authorPubkey: authors.viewer,
      channelId: general,
      content: q4HeadContent,
      createdAt: ago(HOUR),
      id: ids.q4Head,
      pageId: MOCK_PAGE_IDS.q4Plan,
      prev: ids.q4Second,
      suggestion: ids.q4AppliedSuggestion,
      title: "Q4 Plan",
    }),
    // Closed by a stored revision carrying its `suggestion` tag (no resolution).
    suggestion({
      authorPubkey: authors.bob,
      base: ids.q4Second,
      channelId: general,
      content: q4HeadContent,
      createdAt: ago(30 * HOUR),
      id: ids.q4AppliedSuggestion,
      pageId: MOCK_PAGE_IDS.q4Plan,
    }),
    // Closed by a `rejected` resolution.
    suggestion({
      authorPubkey: authors.bob,
      base: ids.q4Draft,
      channelId: general,
      content: "# Q4 Plan\n\nA different idea.",
      createdAt: ago(60 * HOUR),
      id: ids.q4RejectedSuggestion,
      pageId: MOCK_PAGE_IDS.q4Plan,
    }),
    event(
      "c1".repeat(32),
      KIND_PAGE_SUGGESTION_RESOLUTION,
      authors.alice,
      ago(58 * HOUR),
      [
        ["h", general],
        ["d", MOCK_PAGE_IDS.q4Plan],
        ["e", ids.q4RejectedSuggestion],
        ["status", "rejected"],
      ],
      "",
    ),
    // Pending and stale: based on the second revision, but the head moved on.
    suggestion({
      authorPubkey: authors.bob,
      base: ids.q4Second,
      channelId: general,
      content: "# Q4 Plan\n\nBob's alternative.",
      createdAt: ago(20 * HOUR),
      id: ids.q4StaleSuggestion,
      pageId: MOCK_PAGE_IDS.q4Plan,
    }),
    // Pending and current: based on the head.
    suggestion({
      authorPubkey: authors.agent,
      base: ids.q4Head,
      channelId: general,
      content: `${q4HeadContent}\n\n- Hiring`,
      createdAt: ago(30 * 60),
      id: ids.q4FreshSuggestion,
      pageId: MOCK_PAGE_IDS.q4Plan,
    }),

    // Runbook in #engineering: two revisions.
    revision({
      authorPubkey: authors.alice,
      channelId: engineering,
      content: "# Deploy runbook\n\n1. Build\n2. Ship",
      createdAt: ago(5 * DAY),
      id: ids.runbookFirst,
      pageId: MOCK_PAGE_IDS.runbook,
      title: "Deploy runbook",
    }),
    revision({
      authorPubkey: authors.bob,
      channelId: engineering,
      content: "# Deploy runbook\n\n1. Build\n2. Ship\n3. Verify",
      createdAt: ago(DAY),
      id: ids.runbookHead,
      pageId: MOCK_PAGE_IDS.runbook,
      prev: ids.runbookFirst,
      title: "Deploy runbook",
    }),

    // Design notes in #design: a single revision, the oldest page.
    revision({
      authorPubkey: authors.bob,
      channelId: design,
      content: "# Design notes\n\nSpacing uses a 4px grid.",
      createdAt: ago(9 * DAY),
      id: ids.designNotesOnly,
      pageId: MOCK_PAGE_IDS.designNotes,
      title: "Design notes",
    }),

    // Meeting notes: two tips off one parent with equal created_at.
    revision({
      authorPubkey: authors.alice,
      channelId: general,
      content: "# Meeting notes\n\nAgenda.",
      createdAt: ago(4 * DAY + HOUR),
      id: ids.meetingRoot,
      pageId: MOCK_PAGE_IDS.meetingNotes,
      title: "Meeting notes",
    }),
    revision({
      authorPubkey: authors.bob,
      channelId: general,
      content: "# Meeting notes\n\nBranch B wins on a naive sort.",
      createdAt: ago(4 * DAY),
      id: ids.meetingTipHighId,
      pageId: MOCK_PAGE_IDS.meetingNotes,
      prev: ids.meetingRoot,
      title: "Meeting notes",
    }),
    revision({
      authorPubkey: authors.alice,
      channelId: general,
      content: "# Meeting notes\n\nBranch A has the lowest id.",
      createdAt: ago(4 * DAY),
      id: ids.meetingTipLowId,
      pageId: MOCK_PAGE_IDS.meetingNotes,
      prev: ids.meetingRoot,
      title: "Meeting notes",
    }),
  ];
}

function hasTag(event: RelayEvent, name: string, values: string[]): boolean {
  return event.tags.some((tag) => tag[0] === name && values.includes(tag[1]));
}

/** Create the in-memory page store the bridge answers REQs from. */
export function createMockPageStore(
  authors: MockPageAuthors,
  nowSeconds: number = Math.floor(Date.now() / 1000),
): MockPageStore {
  const events = createMockPageEvents(authors, nowSeconds);
  return {
    push: (next) => {
      if (!events.some((existing) => existing.id === next.id)) {
        events.push(next);
      }
    },
    query: (filter) =>
      events
        .filter((candidate) =>
          filter.kinds ? filter.kinds.includes(candidate.kind) : false,
        )
        .filter((candidate) =>
          filter["#h"] ? hasTag(candidate, "h", filter["#h"]) : true,
        )
        .filter((candidate) =>
          filter["#d"] ? hasTag(candidate, "d", filter["#d"]) : true,
        )
        .filter((candidate) =>
          filter.until === undefined
            ? true
            : candidate.created_at <= filter.until,
        )
        .filter((candidate) =>
          filter.since === undefined
            ? true
            : candidate.created_at >= filter.since,
        )
        .sort(
          (left, right) =>
            right.created_at - left.created_at ||
            left.id.localeCompare(right.id),
        )
        .slice(0, filter.limit ?? 500),
  };
}
