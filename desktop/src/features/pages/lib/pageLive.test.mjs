import assert from "node:assert/strict";
import test from "node:test";

import {
  buildPageLiveFilters,
  PAGE_LIVE_CHANNELS_PER_REQ,
  PAGE_LIVE_MAX_CHANNELS,
} from "./pageLive.ts";

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
