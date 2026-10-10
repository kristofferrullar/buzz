import assert from "node:assert/strict";
import test from "node:test";

import {
  fetchEventsPaged,
  fetchPageDetail,
  fetchPagesLibrary,
} from "./pageFetch.ts";

const H = "11111111-1111-4111-8111-111111111111";
const D = "22222222-2222-4222-8222-222222222222";

function revisionEvent(index, createdAt, { d = D, prev } = {}) {
  const tags = [
    ["h", H],
    ["d", d],
    ["title", `Title ${index}`],
  ];
  if (prev) tags.push(["prev", prev]);
  return {
    id: index.toString(16).padStart(64, "0"),
    pubkey: "a".repeat(64),
    created_at: createdAt,
    kind: 52000,
    tags,
    content: `body ${index}`,
    sig: "0".repeat(128),
  };
}

/**
 * A relay double that honours what the production REQ path relies on:
 * `kinds`, `#h`/`#d`, an inclusive `until`, newest-first order
 * (`created_at DESC, id ASC`) and a `limit`. It records every filter it sees.
 */
function fakeRelay(events) {
  const seen = [];
  const fetchEvents = async (filter) => {
    seen.push(filter);
    return events
      .filter((event) => filter.kinds.includes(event.kind))
      .filter((event) =>
        filter["#h"]
          ? event.tags.some((t) => t[0] === "h" && filter["#h"].includes(t[1]))
          : true,
      )
      .filter((event) =>
        filter["#d"]
          ? event.tags.some((t) => t[0] === "d" && filter["#d"].includes(t[1]))
          : true,
      )
      .filter((event) =>
        filter.until === undefined ? true : event.created_at <= filter.until,
      )
      .sort((a, b) => b.created_at - a.created_at || a.id.localeCompare(b.id))
      .slice(0, filter.limit);
  };
  return { fetchEvents, seen };
}

test("paging walks the until cursor to exhaustion and de-duplicates boundary rows", async () => {
  // 7 events at distinct seconds, page size 3: pages overlap on the boundary.
  const events = Array.from({ length: 7 }, (_, i) =>
    revisionEvent(i + 1, 100 + i),
  );
  const relay = fakeRelay(events);
  const result = await fetchEventsPaged(
    relay.fetchEvents,
    { kinds: [52000] },
    { pageLimit: 3, maxRequests: 10 },
  );
  assert.equal(result.truncated, false);
  assert.equal(result.events.length, 7);
  assert.equal(new Set(result.events.map((e) => e.id)).size, 7);
});

test("a read that ends exactly on a full page is not reported truncated", async () => {
  const events = Array.from({ length: 3 }, (_, i) =>
    revisionEvent(i + 1, 100 + i),
  );
  const result = await fetchEventsPaged(
    fakeRelay(events).fetchEvents,
    { kinds: [52000] },
    { pageLimit: 3, maxRequests: 10 },
  );
  assert.equal(result.truncated, false);
  assert.equal(result.events.length, 3);
});

test("the request budget is a hard bound and the result says it was hit", async () => {
  const events = Array.from({ length: 20 }, (_, i) =>
    revisionEvent(i + 1, 100 + i),
  );
  const relay = fakeRelay(events);
  const result = await fetchEventsPaged(
    relay.fetchEvents,
    { kinds: [52000] },
    { pageLimit: 3, maxRequests: 2 },
  );
  assert.equal(relay.seen.length, 2);
  assert.equal(result.truncated, true);
  // Newest events come first, so what was read is a newest-first prefix.
  assert.equal(result.events.length < 20, true);
  assert.equal(Math.max(...result.events.map((e) => e.created_at)), 119);
});

test("a second denser than one page stops the read as truncated instead of looping", async () => {
  // 6 events share one second and the page size is 3: the cursor cannot advance.
  const events = Array.from({ length: 6 }, (_, i) => revisionEvent(i + 1, 500));
  const relay = fakeRelay(events);
  const result = await fetchEventsPaged(
    relay.fetchEvents,
    { kinds: [52000] },
    { pageLimit: 3, maxRequests: 50 },
  );
  assert.equal(result.truncated, true);
  assert.equal(relay.seen.length <= 2, true);
});

test("a relay failure propagates instead of becoming an empty result", async () => {
  await assert.rejects(
    fetchEventsPaged(
      async () => {
        throw new Error("relay unreachable");
      },
      { kinds: [52000] },
    ),
    /relay unreachable/,
  );
});

test("the library filter names the revision kind and carries no channel tag", async () => {
  const relay = fakeRelay([revisionEvent(1, 100)]);
  const library = await fetchPagesLibrary(relay.fetchEvents);
  assert.deepEqual(relay.seen[0].kinds, [52000]);
  assert.equal("#h" in relay.seen[0], false);
  assert.equal(library.pages.length, 1);
  assert.equal(library.truncated, false);
});

function suggestionEvent(index, createdAt, base, { d = D } = {}) {
  return {
    id: index.toString(16).padStart(64, "0"),
    pubkey: "b".repeat(64),
    created_at: createdAt,
    kind: 52001,
    tags: [
      ["h", H],
      ["d", d],
      ["base", base],
    ],
    content: `suggested ${index}`,
    sig: "0".repeat(128),
  };
}

test("the page reads name explicit kinds and scope on #h and #d", async () => {
  const relay = fakeRelay([revisionEvent(1, 100)]);
  await fetchPageDetail(relay.fetchEvents, { channelId: H, pageId: D });
  // Revisions are read on their own, so suggestions and resolutions can never
  // crowd the head out of the window; the other two kinds share one read.
  assert.deepEqual(relay.seen.map((filter) => filter.kinds).sort(), [
    [52000],
    [52001, 52002],
  ]);
  for (const filter of relay.seen) {
    assert.deepEqual(filter["#h"], [H]);
    assert.deepEqual(filter["#d"], [D]);
  }
});

test("a flood of newer suggestions never hides the page or its head", async () => {
  // One revision, then 12 suggestions newer than it. With page size 5 and two
  // requests, a single mixed window would hold only suggestions: no head, and
  // the page would read as "not found".
  const root = revisionEvent(1, 100);
  const suggestions = Array.from({ length: 12 }, (_, i) =>
    suggestionEvent(0x100 + i, 200 + i, root.id),
  );
  const relay = fakeRelay([root, ...suggestions]);
  const result = await fetchPageDetail(
    relay.fetchEvents,
    { channelId: H, pageId: D },
    { pageLimit: 5, maxRequests: 2 },
  );
  assert.notEqual(result.detail, null);
  assert.equal(result.detail.head.id, root.id);
  assert.equal(result.truncated, false);
  // The suggestion window did hit its bound, and the result says so.
  assert.equal(result.suggestionsTruncated, true);
  assert.equal(result.detail.suggestions.length < 12, true);
});

test("a complete page read reports neither window as truncated", async () => {
  const root = revisionEvent(1, 100);
  const relay = fakeRelay([root, suggestionEvent(0x100, 150, root.id)]);
  const result = await fetchPageDetail(relay.fetchEvents, {
    channelId: H,
    pageId: D,
  });
  assert.equal(result.truncated, false);
  assert.equal(result.suggestionsTruncated, false);
  assert.equal(result.detail.suggestions.length, 1);
});

test("a failed suggestion read fails the whole page read instead of showing none", async () => {
  const root = revisionEvent(1, 100);
  const fetchEvents = async (filter) => {
    if (filter.kinds.includes(52001)) throw new Error("relay unreachable");
    return [root];
  };
  await assert.rejects(
    fetchPageDetail(fetchEvents, { channelId: H, pageId: D }),
    /relay unreachable/,
  );
});

test("a library window cut inside one second drops that second, so no page shows a stale head", async () => {
  // Page P: a (id 1) then b (id 2, built on a), both in second 500. The relay
  // lists id 1 before id 2, so a window of two events holds `other` and a but
  // not b. Reading a as P's head would be wrong.
  const other = "33333333-3333-4333-8333-333333333333";
  const x = revisionEvent(9, 600, { d: other });
  const a = revisionEvent(1, 500);
  const b = revisionEvent(2, 500, { prev: a.id });
  const relay = fakeRelay([x, a, b]);
  const library = await fetchPagesLibrary(relay.fetchEvents, {
    pageLimit: 2,
    maxRequests: 1,
  });
  assert.equal(library.truncated, true);
  assert.deepEqual(
    library.pages.map((page) => page.pageId),
    [other],
  );
});

test("a window that is one dense second still lists what it read, flagged truncated", async () => {
  const events = Array.from({ length: 6 }, (_, i) => revisionEvent(i + 1, 500));
  const library = await fetchPagesLibrary(fakeRelay(events).fetchEvents, {
    pageLimit: 3,
    maxRequests: 50,
  });
  assert.equal(library.truncated, true);
  // Never an authoritative empty library for a relay that has pages.
  assert.equal(library.pages.length > 0, true);
});

test("a complete library keeps every second, including the oldest", async () => {
  const a = revisionEvent(1, 500);
  const b = revisionEvent(2, 500, { prev: a.id });
  const library = await fetchPagesLibrary(fakeRelay([a, b]).fetchEvents, {
    pageLimit: 10,
  });
  assert.equal(library.truncated, false);
  assert.equal(library.pages[0].headId, b.id);
});

test("the library derives each page's head from a window spanning several requests", async () => {
  const other = "33333333-3333-4333-8333-333333333333";
  // Page D has 4 linear revisions, page `other` has 1; page size 2 forces paging.
  const d1 = revisionEvent(1, 100);
  const d2 = revisionEvent(2, 110, { prev: d1.id });
  const o1 = revisionEvent(5, 115, { d: other });
  const d3 = revisionEvent(3, 120, { prev: d2.id });
  const d4 = revisionEvent(4, 130, { prev: d3.id });
  const relay = fakeRelay([d1, d2, o1, d3, d4]);
  const library = await fetchPagesLibrary(relay.fetchEvents, {
    pageLimit: 2,
    maxRequests: 10,
  });
  assert.equal(relay.seen.length > 1, true);
  assert.equal(library.truncated, false);
  assert.deepEqual(
    library.pages.map((page) => [page.pageId, page.headId, page.revisionCount]),
    [
      [D, d4.id, 4],
      [other, o1.id, 1],
    ],
  );
});

test("page detail reports truncation without hiding the head", async () => {
  // 1200 revisions would exceed the default budget; use a tiny relay double
  // that keeps returning full pages of revisions with strictly older
  // timestamps, and nothing for the suggestion kinds.
  const calls = [];
  const fetchEvents = async (filter) => {
    if (!filter.kinds.includes(52000)) return [];
    calls.push(filter);
    const base = (filter.until ?? 100_000) - 1;
    return Array.from({ length: filter.limit }, (_, i) =>
      revisionEvent(calls.length * 1000 + i + 1, base - i),
    );
  };
  const result = await fetchPageDetail(fetchEvents, {
    channelId: H,
    pageId: D,
  });
  assert.equal(result.truncated, true);
  assert.equal(result.suggestionsTruncated, false);
  assert.equal(calls.length, 4);
  assert.notEqual(result.detail, null);
});
