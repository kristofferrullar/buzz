import assert from "node:assert/strict";
import test from "node:test";

import { PAGE_WRITE_COMMANDS, createPageWriter } from "./pageWrite.ts";

function recordingInvoke(respond) {
  const calls = [];
  const invoke = async (command, args) => {
    calls.push({ command, args });
    return respond(command, args);
  };
  return { calls, invoke };
}

const ids = {
  channelId: "11111111-1111-4111-8111-111111111111",
  pageId: "22222222-2222-4222-8222-222222222222",
};

test("a revision is one publish_page_revision call carrying prev and no suggestion", async () => {
  const { calls, invoke } = recordingInvoke(() => ({ event_id: "e1" }));
  const result = await createPageWriter(invoke).publishRevision({
    ...ids,
    title: "Plan",
    content: "# body",
    prev: "f".repeat(64),
  });

  assert.deepEqual(result, { ok: true, eventId: "e1" });
  assert.deepEqual(calls, [
    {
      command: "publish_page_revision",
      args: {
        ...ids,
        title: "Plan",
        content: "# body",
        prev: "f".repeat(64),
        suggestion: null,
      },
    },
  ]);
});

test("creating a page sends a null prev; applying a suggestion names it", async () => {
  const { calls, invoke } = recordingInvoke(() => ({ event_id: "e" }));
  const writer = createPageWriter(invoke);
  await writer.publishRevision({ ...ids, title: "T", content: "", prev: null });
  await writer.publishRevision({
    ...ids,
    title: "T",
    content: "c",
    prev: "a".repeat(64),
    suggestion: "b".repeat(64),
  });
  assert.equal(calls[0].args.prev, null);
  assert.equal(calls[0].args.suggestion, null);
  assert.equal(calls[1].args.prev, "a".repeat(64));
  assert.equal(calls[1].args.suggestion, "b".repeat(64));
});

test("suggest and reject use their own commands and exactly the fields the SDK builders need", async () => {
  const { calls, invoke } = recordingInvoke(() => ({ event_id: "e" }));
  const writer = createPageWriter(invoke);
  await writer.publishSuggestion({
    ...ids,
    base: "a".repeat(64),
    content: "c",
  });
  await writer.rejectSuggestion({ ...ids, suggestion: "b".repeat(64) });

  assert.deepEqual(calls, [
    {
      command: PAGE_WRITE_COMMANDS.suggestion,
      args: { ...ids, base: "a".repeat(64), content: "c" },
    },
    {
      command: PAGE_WRITE_COMMANDS.rejection,
      args: { ...ids, suggestion: "b".repeat(64) },
    },
  ]);
  assert.deepEqual(Object.values(PAGE_WRITE_COMMANDS).sort(), [
    "publish_page_revision",
    "publish_page_suggestion",
    "reject_page_suggestion",
  ]);
});

test("a rejection becomes a classified failure, never a thrown error or a success", async () => {
  const { invoke } = recordingInvoke(() => {
    throw new Error("relay rejected event: conflict: stale prev (head x)");
  });
  const result = await createPageWriter(invoke).publishRevision({
    ...ids,
    title: "T",
    content: "c",
    prev: "a".repeat(64),
  });
  assert.equal(result.ok, false);
  assert.equal(result.error.kind, "conflict");
  assert.equal(result.error.conflictReason, "head-moved");
  assert.match(result.error.detail, /stale prev/);
});
