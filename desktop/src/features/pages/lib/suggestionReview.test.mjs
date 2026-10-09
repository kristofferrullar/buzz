import assert from "node:assert/strict";
import test from "node:test";

import {
  APPLIED_ACCEPT_REASON,
  STALE_ACCEPT_REASON,
  buildAcceptRevision,
  decideSuggestion,
  resolveSuggestionDiffBase,
} from "./suggestionReview.ts";

const ids = { channelId: "ch", pageId: "pg" };
const head = {
  ...ids,
  id: "h".repeat(64),
  pubkey: "a".repeat(64),
  createdAt: 100,
  prev: null,
  title: "Plan",
  content: "# head",
  suggestionId: null,
};
const older = {
  ...head,
  id: "o".repeat(64),
  content: "# older",
  createdAt: 50,
};

const suggestion = {
  ...ids,
  id: "s".repeat(64),
  pubkey: "b".repeat(64),
  createdAt: 120,
  base: head.id,
  content: "# head\n\nplus",
};

const state = (overrides = {}) => ({
  suggestion,
  closed: null,
  stale: false,
  ...overrides,
});

test("decision table: writer x stale x already-applied x closed", () => {
  const cases = [
    {
      name: "fresh suggestion, writer",
      input: { canWrite: true, state: state() },
      expected: { canDecide: true, accept: { enabled: true, reason: null } },
    },
    {
      name: "read-only viewer sees no controls",
      input: { canWrite: false, state: state() },
      expected: { canDecide: false, accept: { enabled: false, reason: null } },
    },
    {
      name: "stale: accept disabled with a reason, reject still offered",
      input: { canWrite: true, state: state({ stale: true }) },
      expected: {
        canDecide: true,
        accept: { enabled: false, reason: STALE_ACCEPT_REASON },
      },
    },
    {
      name: "already equals the head: accept disabled (no-op revision), reject offered",
      input: {
        canWrite: true,
        state: state({ suggestion: { ...suggestion, content: head.content } }),
      },
      expected: {
        canDecide: true,
        accept: { enabled: false, reason: APPLIED_ACCEPT_REASON },
      },
    },
    {
      name: "closed suggestions offer nothing",
      input: { canWrite: true, state: state({ closed: "resolution" }) },
      expected: { canDecide: false, accept: { enabled: false, reason: null } },
    },
    {
      name: "stale wins over already-applied",
      input: {
        canWrite: true,
        state: state({
          stale: true,
          suggestion: { ...suggestion, content: head.content },
        }),
      },
      expected: {
        canDecide: true,
        accept: { enabled: false, reason: STALE_ACCEPT_REASON },
      },
    },
  ];
  for (const { expected, input, name } of cases) {
    assert.deepEqual(decideSuggestion({ ...input, head }), expected, name);
  }
});

test("accepting is one revision: prev is the head and equals the suggestion's base", () => {
  const revision = buildAcceptRevision(head, suggestion);
  assert.deepEqual(revision, {
    title: head.title,
    content: suggestion.content,
    prev: head.id,
    suggestion: suggestion.id,
  });
  assert.equal(revision.prev, suggestion.base);
});

test("the diff base is the suggestion's own base revision, else the head", () => {
  const detail = {
    head,
    history: [
      { revision: head, isHead: true, onHeadChain: true },
      { revision: older, isHead: false, onHeadChain: true },
    ],
  };
  assert.deepEqual(
    resolveSuggestionDiffBase(detail, { ...suggestion, base: older.id }),
    { kind: "base", revision: older },
  );
  assert.deepEqual(resolveSuggestionDiffBase(detail, suggestion), {
    kind: "base",
    revision: head,
  });
  // Base not in the loaded window: fall back to the head, and say so.
  assert.deepEqual(
    resolveSuggestionDiffBase(detail, { ...suggestion, base: "z".repeat(64) }),
    { kind: "head-fallback", revision: head },
  );
});
