import assert from "node:assert/strict";
import test from "node:test";

import {
  UNKNOWN_CHANNEL_LABEL,
  channelLabelFor,
  isChannelWritable,
  listWritableChannels,
} from "./channelWrite.ts";

const channel = (overrides) => ({
  id: overrides.name,
  name: "general",
  channelType: "stream",
  isMember: true,
  archivedAt: null,
  ...overrides,
});

test("writable means a member of a channel that is not archived", () => {
  const cases = [
    [channel({}), true],
    [channel({ isMember: false }), false],
    [channel({ archivedAt: "2026-01-01T00:00:00Z" }), false],
    [channel({ isMember: false, archivedAt: "2026-01-01T00:00:00Z" }), false],
    [undefined, false],
  ];
  for (const [input, expected] of cases) {
    assert.equal(isChannelWritable(input), expected, JSON.stringify(input));
  }
});

test("the create picker lists writable non-DM channels by name", () => {
  const listed = listWritableChannels([
    channel({ name: "random" }),
    channel({ name: "alice-tyler", channelType: "dm" }),
    channel({ name: "design", isMember: false }),
    channel({ name: "engineering" }),
    channel({ name: "old", archivedAt: "2026-01-01T00:00:00Z" }),
    channel({ name: "announcements", channelType: "forum" }),
  ]);
  assert.deepEqual(
    listed.map((c) => c.name),
    ["announcements", "engineering", "random"],
  );
});

test("labels: #name for channels, the bare name for DMs, a neutral label if unknown", () => {
  assert.equal(channelLabelFor(channel({ name: "general" })), "#general");
  assert.equal(
    channelLabelFor(channel({ name: "alice-tyler", channelType: "dm" })),
    "alice-tyler",
  );
  assert.equal(channelLabelFor(undefined), UNKNOWN_CHANNEL_LABEL);
});
