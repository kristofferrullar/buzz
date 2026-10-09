import assert from "node:assert/strict";
import test from "node:test";

import {
  buildLibrary,
  buildPageDetail,
  closedSuggestions,
  deriveSuggestionStates,
  headChainIds,
  pageKey,
  parsePageEvents,
  parsePageRevision,
  selectHead,
} from "./pageModel.ts";

const H = "11111111-1111-4111-8111-111111111111";
const D = "22222222-2222-4222-8222-222222222222";
const ALICE = "a".repeat(64);
const BOB = "b".repeat(64);

const hex = (char) => char.repeat(64);

function revision({
  id,
  prev,
  createdAt,
  title = "Plan",
  content = "body",
  h = H,
  d = D,
  pubkey = ALICE,
  suggestion,
}) {
  const tags = [
    ["h", h],
    ["d", d],
    ["title", title],
  ];
  if (prev) tags.push(["prev", prev]);
  if (suggestion) tags.push(["suggestion", suggestion]);
  return {
    id,
    pubkey,
    created_at: createdAt,
    kind: 52000,
    tags,
    content,
    sig: "0".repeat(128),
  };
}

function suggestion({ id, base, createdAt, pubkey = BOB, content = "edit" }) {
  return {
    id,
    pubkey,
    created_at: createdAt,
    kind: 52001,
    tags: [
      ["h", H],
      ["d", D],
      ["base", base],
    ],
    content,
    sig: "0".repeat(128),
  };
}

function resolution({ id, target, status = "rejected", rev, createdAt }) {
  const tags = [
    ["h", H],
    ["d", D],
    ["e", target],
    ["status", status],
  ];
  if (rev) tags.push(["rev", rev]);
  return {
    id,
    pubkey: ALICE,
    created_at: createdAt,
    kind: 52002,
    tags,
    content: "",
    sig: "0".repeat(128),
  };
}

const revisionsOf = (events) => parsePageEvents(events).revisions;

test("head of a linear chain is its newest revision, whatever order events arrive in", () => {
  const events = [
    revision({ id: hex("3"), prev: hex("2"), createdAt: 300 }),
    revision({ id: hex("1"), createdAt: 100 }),
    revision({ id: hex("2"), prev: hex("1"), createdAt: 200 }),
  ];
  assert.equal(selectHead(revisionsOf(events))?.id, hex("3"));
  assert.equal(selectHead(revisionsOf([...events].reverse()))?.id, hex("3"));
});

test("with several tips the head has the greatest created_at", () => {
  const events = [
    revision({ id: hex("1"), createdAt: 100 }),
    revision({ id: hex("2"), prev: hex("1"), createdAt: 200 }),
    // A second tip off the same parent, newer than the first.
    revision({ id: hex("9"), prev: hex("1"), createdAt: 250 }),
  ];
  assert.equal(selectHead(revisionsOf(events))?.id, hex("9"));
});

test("tips with equal created_at are tied by the lowest event id", () => {
  const events = [
    revision({ id: hex("1"), createdAt: 100 }),
    revision({ id: hex("e"), prev: hex("1"), createdAt: 200 }),
    revision({ id: hex("5"), prev: hex("1"), createdAt: 200 }),
    revision({ id: hex("c"), prev: hex("1"), createdAt: 200 }),
  ];
  assert.equal(selectHead(revisionsOf(events))?.id, hex("5"));
  assert.equal(selectHead(revisionsOf([...events].reverse()))?.id, hex("5"));
});

test("a revision that another names as prev is never the head, even when it is newer", () => {
  // Clock skew: the successor carries an older timestamp than its parent.
  const events = [
    revision({ id: hex("1"), createdAt: 500 }),
    revision({ id: hex("2"), prev: hex("1"), createdAt: 400 }),
  ];
  assert.equal(selectHead(revisionsOf(events))?.id, hex("2"));
});

test("when the head is deleted its prev becomes the head", () => {
  const events = [
    revision({ id: hex("1"), createdAt: 100 }),
    revision({ id: hex("2"), prev: hex("1"), createdAt: 200 }),
    revision({ id: hex("3"), prev: hex("2"), createdAt: 300 }),
  ];
  const withoutHead = events.filter((event) => event.id !== hex("3"));
  assert.equal(selectHead(revisionsOf(withoutHead))?.id, hex("2"));
});

test("selectHead of nothing is null and of a corrupt cycle is still the newest revision", () => {
  assert.equal(selectHead([]), null);
  const cyclic = revisionsOf([
    revision({ id: hex("1"), prev: hex("2"), createdAt: 100 }),
    revision({ id: hex("2"), prev: hex("1"), createdAt: 200 }),
  ]);
  assert.equal(selectHead(cyclic)?.id, hex("2"));
});

test("headChainIds walks prev links and survives a cycle", () => {
  const revisions = revisionsOf([
    revision({ id: hex("1"), createdAt: 100 }),
    revision({ id: hex("2"), prev: hex("1"), createdAt: 200 }),
    revision({ id: hex("9"), prev: hex("1"), createdAt: 150 }),
  ]);
  const head = selectHead(revisions);
  assert.deepEqual([...headChainIds(revisions, head)].sort(), [
    hex("1"),
    hex("2"),
  ]);

  const cyclic = revisionsOf([
    revision({ id: hex("1"), prev: hex("2"), createdAt: 100 }),
    revision({ id: hex("2"), prev: hex("1"), createdAt: 200 }),
  ]);
  assert.equal(headChainIds(cyclic, cyclic[1]).size, 2);
});

test("malformed page events are dropped, not guessed at", () => {
  const good = revision({ id: hex("1"), createdAt: 100 });
  const noD = {
    ...good,
    id: hex("2"),
    tags: [
      ["h", H],
      ["title", "x"],
    ],
  };
  const twoH = {
    ...good,
    id: hex("3"),
    tags: [...good.tags, ["h", "33333333-3333-4333-8333-333333333333"]],
  };
  const blankTitle = {
    ...good,
    id: hex("4"),
    tags: [
      ["h", H],
      ["d", D],
      ["title", "   "],
    ],
  };
  const wrongKind = { ...good, id: hex("5"), kind: 9 };
  const suggestionWithoutBase = {
    ...suggestion({ id: hex("6"), base: hex("1"), createdAt: 120 }),
    tags: [
      ["h", H],
      ["d", D],
    ],
  };
  const resolutionWithBadStatus = {
    ...resolution({ id: hex("7"), target: hex("6"), createdAt: 130 }),
    tags: [
      ["h", H],
      ["d", D],
      ["e", hex("6")],
      ["status", "maybe"],
    ],
  };
  const parsed = parsePageEvents([
    good,
    noD,
    twoH,
    blankTitle,
    wrongKind,
    suggestionWithoutBase,
    resolutionWithBadStatus,
  ]);
  assert.deepEqual(
    parsed.revisions.map((entry) => entry.id),
    [hex("1")],
  );
  assert.equal(parsed.suggestions.length, 0);
  assert.equal(parsed.resolutions.length, 0);
  assert.equal(parsePageRevision(noD), null);
});

test("duplicate events from overlapping paged reads collapse to one revision", () => {
  const event = revision({ id: hex("1"), createdAt: 100 });
  assert.equal(revisionsOf([event, event, { ...event }]).length, 1);
});

test("the library groups by (h, d): the same d in two channels is two pages", () => {
  const otherChannel = "44444444-4444-4444-8444-444444444444";
  const library = buildLibrary(
    revisionsOf([
      revision({ id: hex("1"), createdAt: 100, title: "Runbook" }),
      revision({
        id: hex("2"),
        createdAt: 200,
        title: "Runbook",
        h: otherChannel,
      }),
    ]),
  );
  assert.equal(library.length, 2);
  assert.notEqual(library[0].key, library[1].key);
});

test("the library lists pages newest-updated first with the head's title and author", () => {
  const second = "55555555-5555-4555-8555-555555555555";
  const library = buildLibrary(
    revisionsOf([
      revision({ id: hex("1"), createdAt: 100, title: "Old title" }),
      revision({
        id: hex("2"),
        prev: hex("1"),
        createdAt: 300,
        title: "New title",
        pubkey: BOB,
      }),
      revision({ id: hex("3"), createdAt: 200, title: "Other", d: second }),
    ]),
  );
  assert.deepEqual(
    library.map((page) => [page.title, page.updatedAt, page.updatedBy]),
    [
      ["New title", 300, BOB],
      ["Other", 200, ALICE],
    ],
  );
  assert.equal(library[0].revisionCount, 2);
  assert.equal(library[0].headId, hex("2"));
});

test("a suggestion is closed by a resolution that references it", () => {
  const closed = closedSuggestions(
    parsePageEvents([
      resolution({ id: hex("8"), target: hex("6"), createdAt: 300 }),
    ]).resolutions,
    [],
  );
  assert.equal(closed.get(hex("6")), "resolution");
});

test("a suggestion is closed by a stored revision carrying its suggestion tag, with no resolution", () => {
  const events = [
    revision({ id: hex("1"), createdAt: 100 }),
    suggestion({ id: hex("6"), base: hex("1"), createdAt: 150 }),
    revision({
      id: hex("2"),
      prev: hex("1"),
      createdAt: 200,
      suggestion: hex("6"),
    }),
  ];
  const detail = buildPageDetail({ channelId: H, pageId: D }, events);
  assert.equal(detail.suggestions.length, 1);
  assert.equal(detail.suggestions[0].closed, "revision");
});

test("an unreferenced suggestion stays pending, and is stale exactly when its base is not the head", () => {
  const events = [
    revision({ id: hex("1"), createdAt: 100 }),
    suggestion({ id: hex("6"), base: hex("1"), createdAt: 150 }),
    revision({ id: hex("2"), prev: hex("1"), createdAt: 200 }),
    suggestion({ id: hex("7"), base: hex("2"), createdAt: 250 }),
  ];
  const detail = buildPageDetail({ channelId: H, pageId: D }, events);
  const byId = Object.fromEntries(
    detail.suggestions.map((state) => [state.suggestion.id, state]),
  );
  assert.equal(byId[hex("6")].closed, null);
  assert.equal(byId[hex("6")].stale, true);
  assert.equal(byId[hex("7")].closed, null);
  assert.equal(byId[hex("7")].stale, false);
});

test("a rejected suggestion is closed even though its base is the head", () => {
  const events = [
    revision({ id: hex("1"), createdAt: 100 }),
    suggestion({ id: hex("6"), base: hex("1"), createdAt: 150 }),
    resolution({ id: hex("8"), target: hex("6"), createdAt: 200 }),
  ];
  const detail = buildPageDetail({ channelId: H, pageId: D }, events);
  assert.equal(detail.suggestions[0].closed, "resolution");
  assert.equal(detail.suggestions[0].stale, false);
});

// Full input space for a suggestion's state: how it was closed (nothing, a
// resolution, an applying revision, both) against whether its base is the head.
const CLOSURE_CASES = [
  { name: "nothing", withResolution: false, withRevision: false, closed: null },
  {
    name: "a resolution",
    withResolution: true,
    withRevision: false,
    closed: "resolution",
  },
  {
    name: "an applying revision",
    withResolution: false,
    withRevision: true,
    closed: "revision",
  },
  // A resolution is the explicit record, so it wins when both exist.
  {
    name: "both",
    withResolution: true,
    withRevision: true,
    closed: "resolution",
  },
];

for (const baseIsHead of [true, false]) {
  for (const { name, withResolution, withRevision, closed } of CLOSURE_CASES) {
    const baseLabel = baseIsHead ? "is" : "is not";
    test(
      "suggestion state: closed by " +
        name +
        ", base " +
        baseLabel +
        " the head",
      () => {
        const headId = hex("2");
        const base = baseIsHead ? headId : hex("1");
        const suggestionId = hex("6");
        const parsed = parsePageEvents([
          revision({ id: hex("1"), createdAt: 100 }),
          revision({ id: headId, prev: hex("1"), createdAt: 200 }),
          suggestion({ id: suggestionId, base, createdAt: 150 }),
        ]);
        const resolutions = withResolution
          ? parsePageEvents([
              resolution({
                id: hex("8"),
                target: suggestionId,
                createdAt: 300,
              }),
            ]).resolutions
          : [];
        const revisions = withRevision
          ? [
              ...parsed.revisions,
              ...parsePageEvents([
                revision({
                  id: hex("3"),
                  prev: hex("9"),
                  createdAt: 50,
                  suggestion: suggestionId,
                }),
              ]).revisions,
            ]
          : parsed.revisions;

        const [state] = deriveSuggestionStates({
          headId,
          resolutions,
          revisions,
          suggestions: parsed.suggestions,
        });
        assert.equal(state.closed, closed);
        assert.equal(state.stale, !baseIsHead);
      },
    );
  }
}

test("page detail ignores events that belong to a different page", () => {
  const otherPage = "66666666-6666-4666-8666-666666666666";
  const events = [
    revision({ id: hex("1"), createdAt: 100, title: "Mine" }),
    // Newer, but a different page: must not become this page's head.
    revision({ id: hex("2"), createdAt: 900, title: "Theirs", d: otherPage }),
  ];
  const detail = buildPageDetail({ channelId: H, pageId: D }, events);
  assert.equal(detail.head.title, "Mine");
  assert.equal(detail.history.length, 1);
});

test("page detail is null when the page has no revision", () => {
  assert.equal(buildPageDetail({ channelId: H, pageId: D }, []), null);
  assert.equal(
    buildPageDetail({ channelId: H, pageId: D }, [
      suggestion({ id: hex("6"), base: hex("1"), createdAt: 150 }),
    ]),
    null,
  );
});

test("history is newest first, marks the head, and flags revisions off the head chain", () => {
  const events = [
    revision({ id: hex("1"), createdAt: 100, pubkey: ALICE }),
    revision({ id: hex("2"), prev: hex("1"), createdAt: 200 }),
    revision({ id: hex("9"), prev: hex("1"), createdAt: 150 }),
    revision({ id: hex("3"), prev: hex("2"), createdAt: 300 }),
  ];
  const detail = buildPageDetail({ channelId: H, pageId: D }, events);
  assert.deepEqual(
    detail.history.map((entry) => [
      entry.revision.id,
      entry.isHead,
      entry.onHeadChain,
    ]),
    [
      [hex("3"), true, true],
      [hex("2"), false, true],
      [hex("9"), false, false],
      [hex("1"), false, true],
    ],
  );
  assert.equal(detail.createdBy, ALICE);
  assert.equal(detail.createdAt, 100);
});

test("revisions made in one second are listed causally, newest first, whatever their ids say", () => {
  const order = (ids) => {
    // root (earlier second) <- a <- b <- c, all three edits in the same second.
    const [a, b, c] = ids;
    const detail = buildPageDetail({ channelId: H, pageId: D }, [
      revision({ id: hex("0"), createdAt: 100 }),
      revision({ id: a, prev: hex("0"), createdAt: 200 }),
      revision({ id: b, prev: a, createdAt: 200 }),
      revision({ id: c, prev: b, createdAt: 200 }),
    ]);
    return detail.history.map((entry) => entry.revision.id);
  };
  // Ascending ids: a plain id tie-break would list the oldest edit first and
  // bury the head under it.
  assert.deepEqual(order([hex("1"), hex("2"), hex("3")]), [
    hex("3"),
    hex("2"),
    hex("1"),
    hex("0"),
  ]);
  // Descending ids, and a mixed order, must give the same causal answer.
  assert.deepEqual(order([hex("9"), hex("8"), hex("7")]), [
    hex("7"),
    hex("8"),
    hex("9"),
    hex("0"),
  ]);
  assert.deepEqual(order([hex("5"), hex("e"), hex("2")]), [
    hex("2"),
    hex("e"),
    hex("5"),
    hex("0"),
  ]);
});

test("the head is the first history row when its same-second ancestors have lower ids", () => {
  const detail = buildPageDetail({ channelId: H, pageId: D }, [
    revision({ id: hex("1"), createdAt: 200 }),
    revision({ id: hex("2"), prev: hex("1"), createdAt: 200 }),
  ]);
  assert.equal(detail.head.id, hex("2"));
  assert.equal(detail.history[0].revision.id, hex("2"));
  assert.equal(detail.history[0].isHead, true);
});

test("same-second siblings on different branches fall back to depth, then the lowest id", () => {
  const detail = buildPageDetail({ channelId: H, pageId: D }, [
    revision({ id: hex("1"), createdAt: 100 }),
    revision({ id: hex("a"), prev: hex("1"), createdAt: 200 }),
    revision({ id: hex("b"), prev: hex("1"), createdAt: 200 }),
    revision({ id: hex("c"), prev: hex("b"), createdAt: 200 }),
  ]);
  assert.deepEqual(
    detail.history.map((entry) => entry.revision.id),
    // c is built on b (deeper) so it leads; a and b are equally deep: lowest id.
    [hex("c"), hex("a"), hex("b"), hex("1")],
  );
});

test("history ordering survives a corrupt prev cycle instead of looping", () => {
  const detail = buildPageDetail({ channelId: H, pageId: D }, [
    revision({ id: hex("1"), prev: hex("2"), createdAt: 200 }),
    revision({ id: hex("2"), prev: hex("1"), createdAt: 200 }),
  ]);
  assert.equal(detail.history.length, 2);
});

test("with several roots the creator is the earliest root, ties to the lowest event id", () => {
  const detail = (events) =>
    buildPageDetail({ channelId: H, pageId: D }, events);
  const earliest = detail([
    revision({ id: hex("1"), createdAt: 200, pubkey: BOB }),
    revision({ id: hex("2"), createdAt: 100, pubkey: ALICE }),
  ]);
  assert.equal(earliest.createdBy, ALICE);
  assert.equal(earliest.createdAt, 100);

  const tied = detail([
    revision({ id: hex("e"), createdAt: 100, pubkey: BOB }),
    revision({ id: hex("5"), createdAt: 100, pubkey: ALICE }),
  ]);
  assert.equal(tied.createdBy, ALICE);
});

test("pageKey is unambiguous for ids that contain delimiters", () => {
  assert.notEqual(
    pageKey({ channelId: "a:b", pageId: "c" }),
    pageKey({ channelId: "a", pageId: "b:c" }),
  );
});
