import assert from "node:assert/strict";
import test from "node:test";

import {
  buildPageLiveFilters,
  emptyPageRefresh,
  notePageEvent,
  PAGE_LIVE_CHANNELS_PER_REQ,
  PAGE_LIVE_MAX_CHANNELS,
} from "./pageLive.ts";

const D1 = "22222222-2222-4222-8222-222222222222";
const D2 = "33333333-3333-4333-8333-333333333333";

const pageEvent = (kind, { h = channel(1), d = D1 } = {}) => ({
  id: "a".repeat(64),
  pubkey: "b".repeat(64),
  created_at: 100,
  kind,
  tags: [h ? ["h", h] : ["x", "y"], d ? ["d", d] : ["x", "y"]],
  content: "",
  sig: "0".repeat(128),
});

const channel = (index) =>
  `00000000-0000-4000-8000-${index.toString(16).padStart(12, "0")}`;

test("every live filter names its channels in #h: a global REQ gets no page event", () => {
  const filters = buildPageLiveFilters([channel(1), channel(2)]);
  assert.equal(filters.length, 1);
  assert.deepEqual(filters[0]["#h"], [channel(1), channel(2)]);
  assert.deepEqual(filters[0].kinds, [52000, 52001, 52002]);
  assert.equal(filters[0].limit, 0);
});

test("no channels means no subscription, never a global one", () => {
  assert.deepEqual(buildPageLiveFilters([]), []);
  assert.deepEqual(buildPageLiveFilters([""]), []);
});

test("channels are chunked under the relay's per-REQ cap and de-duplicated", () => {
  const ids = Array.from({ length: 250 }, (_, i) => channel(i + 1));
  const filters = buildPageLiveFilters([...ids, ...ids]);
  assert.equal(filters.length, 3);
  for (const filter of filters) {
    assert.equal(filter["#h"].length <= PAGE_LIVE_CHANNELS_PER_REQ, true);
    assert.equal(filter["#h"].length <= 128, true);
  }
  assert.equal(new Set(filters.flatMap((filter) => filter["#h"])).size, 250);
});

test("the total number of watched channels is bounded", () => {
  const ids = Array.from({ length: PAGE_LIVE_MAX_CHANNELS + 500 }, (_, i) =>
    channel(i + 1),
  );
  const watched = buildPageLiveFilters(ids).flatMap((filter) => filter["#h"]);
  assert.equal(watched.length, PAGE_LIVE_MAX_CHANNELS);
});

test("the same channel set always yields the same filters, whatever its order", () => {
  const forward = buildPageLiveFilters([channel(3), channel(1), channel(2)]);
  const backward = buildPageLiveFilters([channel(2), channel(1), channel(3)]);
  assert.deepEqual(forward, backward);
});

test("a revision refreshes the library and its own page, nothing else", () => {
  const refresh = emptyPageRefresh();
  notePageEvent(refresh, pageEvent(52000));
  assert.equal(refresh.library, true);
  assert.equal(refresh.everything, false);
  assert.deepEqual(
    [...refresh.pages.values()],
    [{ channelId: channel(1), pageId: D1 }],
  );
});

test("a suggestion or resolution refreshes only its own page: the library lists heads", () => {
  for (const kind of [52001, 52002]) {
    const refresh = emptyPageRefresh();
    notePageEvent(refresh, pageEvent(kind));
    assert.equal(refresh.library, false);
    assert.equal(refresh.pages.size, 1);
  }
});

test("a burst for two pages is two page refreshes, however many events", () => {
  const refresh = emptyPageRefresh();
  for (let i = 0; i < 5; i += 1) notePageEvent(refresh, pageEvent(52001));
  notePageEvent(refresh, pageEvent(52001, { d: D2 }));
  assert.equal(refresh.pages.size, 2);
  assert.equal(refresh.library, false);
});

test("an event that names no page refreshes everything instead of being dropped", () => {
  for (const missing of [{ h: null }, { d: null }]) {
    const refresh = emptyPageRefresh();
    notePageEvent(refresh, pageEvent(52000, missing));
    assert.equal(refresh.everything, true);
  }
});
