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

/** Tauri commands the desktop uses to publish page events (the TS-to-Rust contract). */
export const MOCK_PAGE_WRITE_COMMANDS: ReadonlySet<string> = new Set([
  "publish_page_revision",
  "publish_page_suggestion",
  "reject_page_suggestion",
]);

/**
 * A one-shot fault a spec arms (`window.__BUZZ_E2E_PAGE_WRITE_FAULT__`) to make
 * the next write fail the way the relay can:
 *
 * - `concurrent-revision`: another author's revision lands on the head just
 *   before the write is processed, and the viewer is *not* told (no live
 *   event), so the write loses the compare-and-swap exactly like a real race.
 * - `reject`: the relay refuses the write with this text.
 * - `delay`: the relay answers after `ms`, so specs can observe the in-flight state.
 */
export type MockPageWriteFault =
  | { kind: "concurrent-revision"; content: string; title?: string }
  | { kind: "reject"; message: string }
  | { kind: "delay"; ms: number };

export type MockPageWriteContext = {
  fault?: MockPageWriteFault | null;
  nowSeconds?: number;
  viewerPubkey: string;
};

export type MockPageStore = {
  /** Answer a REQ filter the way the relay answers a page query. */
  query: (filter: MockPageFilter) => RelayEvent[];
  /** Store an event so later queries return it. */
  push: (event: RelayEvent) => void;
  /**
   * Handle a page write command the way the relay's ingest does: the same
   * checks, in the same order, with the same rejection text, storing nothing
   * when it rejects. Returns the stored event; throws `relay rejected event: ...`
   * (what the Tauri layer reports) otherwise.
   */
  publish: (
    command: string,
    args: Record<string, unknown>,
    context: MockPageWriteContext,
  ) => RelayEvent;
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

function tagValue(event: RelayEvent, name: string): string | undefined {
  return event.tags.find((tag) => tag[0] === name)?.[1];
}

const textEncoder = new TextEncoder();
const MOCK_MAX_TITLE_BYTES = 256;
const MOCK_MAX_CONTENT_BYTES = 64 * 1024;

/** Channels whose pages the mock viewer can read but not write (not a member). */
const MOCK_READ_ONLY_CHANNEL_IDS: ReadonlySet<string> = new Set([
  MOCK_PAGE_CHANNEL_IDS.design,
]);

function randomEventId(): string {
  const bytes = new Uint8Array(32);
  crypto.getRandomValues(bytes);
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join(
    "",
  );
}

/**
 * A page's head: the tip of its revision chain with the greatest `created_at`,
 * ties to the lowest id (NIP-PG "Rebuild Invariant"). Kept independent of the
 * production model on purpose, so the mock does not share its bugs.
 */
function currentHead(
  events: readonly RelayEvent[],
  channelId: string,
  pageId: string,
): RelayEvent | null {
  const revisions = events.filter(
    (candidate) =>
      candidate.kind === KIND_PAGE_REVISION &&
      tagValue(candidate, "h") === channelId &&
      tagValue(candidate, "d") === pageId,
  );
  const referenced = new Set(
    revisions.map((revision) => tagValue(revision, "prev")),
  );
  const tips = revisions
    .filter((revision) => !referenced.has(revision.id))
    .sort(
      (left, right) =>
        right.created_at - left.created_at || left.id.localeCompare(right.id),
    );
  return tips[0] ?? null;
}

/** Create the in-memory page store the bridge answers REQs from. */
export function createMockPageStore(
  authors: MockPageAuthors,
  nowSeconds: number = Math.floor(Date.now() / 1000),
): MockPageStore {
  const events = createMockPageEvents(authors, nowSeconds);

  const find = (id: string, kind: number, h: string, d: string) =>
    events.find(
      (candidate) =>
        candidate.id === id &&
        candidate.kind === kind &&
        tagValue(candidate, "h") === h &&
        tagValue(candidate, "d") === d,
    );
  /** Closed per NIP-PG: a resolution references it, or a revision applied it. */
  const closure = (suggestionId: string): "resolved" | "applied" | null => {
    if (
      events.some(
        (candidate) =>
          candidate.kind === KIND_PAGE_SUGGESTION_RESOLUTION &&
          tagValue(candidate, "e") === suggestionId,
      )
    ) {
      return "resolved";
    }
    return events.some(
      (candidate) =>
        candidate.kind === KIND_PAGE_REVISION &&
        tagValue(candidate, "suggestion") === suggestionId,
    )
      ? "applied"
      : null;
  };

  const publish: MockPageStore["publish"] = (command, args, context) => {
    const reject = (message: string): never => {
      throw new Error(`relay rejected event: ${message}`);
    };
    const channelId = String(args.channelId ?? "");
    const pageId = String(args.pageId ?? "");
    const now = context.nowSeconds ?? Math.floor(Date.now() / 1000);
    const fault = context.fault ?? null;

    if (fault?.kind === "reject") reject(fault.message);
    if (MOCK_READ_ONLY_CHANNEL_IDS.has(channelId)) {
      reject("restricted: not a channel member");
    }
    if (fault?.kind === "concurrent-revision") {
      const head = currentHead(events, channelId, pageId);
      if (head) {
        // Lands silently: the viewer's UI has not heard of it, so the write
        // below races it and loses, like a real concurrent save.
        events.push(
          revision({
            authorPubkey: authors.alice,
            channelId,
            content: fault.content,
            createdAt: Math.max(now, head.created_at + 1),
            id: randomEventId(),
            pageId,
            prev: head.id,
            title: fault.title ?? tagValue(head, "title") ?? "Untitled",
          }),
        );
      }
    }

    const head = currentHead(events, channelId, pageId);
    const createdAt = Math.max(now, head?.created_at ?? 0);
    let stored: RelayEvent;

    if (command === "publish_page_revision") {
      const title = String(args.title ?? "");
      const content = String(args.content ?? "");
      const prev = typeof args.prev === "string" ? args.prev : null;
      const suggestionId =
        typeof args.suggestion === "string" ? args.suggestion : null;

      if (title.trim().length === 0) reject("invalid: page title is blank");
      if (textEncoder.encode(title).length > MOCK_MAX_TITLE_BYTES) {
        reject(`invalid: page title exceeds ${MOCK_MAX_TITLE_BYTES} bytes`);
      }
      const contentBytes = textEncoder.encode(content).length;
      if (contentBytes > MOCK_MAX_CONTENT_BYTES) {
        reject(
          `invalid: page content exceeds maximum size of ${MOCK_MAX_CONTENT_BYTES} bytes (got ${contentBytes})`,
        );
      }
      if (prev === null) {
        if (head) {
          reject(`conflict: page already exists (head ${head.id})`);
        }
      } else {
        if (!head) reject("conflict: page does not exist");
        if (head && head.id !== prev) {
          reject(`conflict: stale prev (head ${head.id})`);
        }
      }
      if (suggestionId !== null) {
        const target = find(
          suggestionId,
          KIND_PAGE_SUGGESTION,
          channelId,
          pageId,
        );
        if (!target) reject("invalid: suggestion event not found");
        if (closure(suggestionId) !== null) {
          reject("conflict: suggestion is already closed");
        }
        if (target && tagValue(target, "base") !== prev) {
          reject(
            "conflict: suggestion is stale (its base is not the revision's prev)",
          );
        }
      }
      if (head && tagValue(head, "title") === title && head.content === content) {
        reject(
          "invalid: no-op revision (title and content equal the page head)",
        );
      }
      stored = revision({
        authorPubkey: context.viewerPubkey,
        channelId,
        content,
        createdAt,
        id: randomEventId(),
        pageId,
        prev: prev ?? undefined,
        suggestion: suggestionId ?? undefined,
        title,
      });
    } else if (command === "publish_page_suggestion") {
      const content = String(args.content ?? "");
      const base = String(args.base ?? "");
      const contentBytes = textEncoder.encode(content).length;
      if (contentBytes > MOCK_MAX_CONTENT_BYTES) {
        reject(
          `invalid: page content exceeds maximum size of ${MOCK_MAX_CONTENT_BYTES} bytes (got ${contentBytes})`,
        );
      }
      if (!find(base, KIND_PAGE_REVISION, channelId, pageId)) {
        reject("invalid: base event not found");
      }
      stored = suggestion({
        authorPubkey: context.viewerPubkey,
        base,
        channelId,
        content,
        createdAt: now,
        id: randomEventId(),
        pageId,
      });
    } else if (command === "reject_page_suggestion") {
      const target = String(args.suggestion ?? "");
      if (!find(target, KIND_PAGE_SUGGESTION, channelId, pageId)) {
        reject("invalid: e event not found");
      }
      const closed = closure(target);
      if (closed === "resolved") {
        reject("conflict: suggestion is already resolved");
      }
      if (closed === "applied") {
        reject("conflict: suggestion is already applied");
      }
      stored = event(
        randomEventId(),
        KIND_PAGE_SUGGESTION_RESOLUTION,
        context.viewerPubkey,
        now,
        [
          ["h", channelId],
          ["d", pageId],
          ["e", target],
          ["status", "rejected"],
        ],
        "",
      );
    } else {
      return reject(`invalid: unknown page write ${command}`);
    }

    events.push(stored);
    return stored;
  };

  return {
    publish,
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
