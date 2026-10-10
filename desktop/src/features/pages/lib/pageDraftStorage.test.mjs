import assert from "node:assert/strict";
import test from "node:test";

import {
  MAX_STORED_DRAFT_CHARS,
  clearPageDraft,
  clearPageDraftIfSaved,
  pageDraftKey,
  readPageDraft,
  writePageDraft,
} from "./pageDraftStorage.ts";

function memoryStorage(initial = {}) {
  const map = new Map(Object.entries(initial));
  return {
    map,
    getItem: (key) => (map.has(key) ? map.get(key) : null),
    setItem: (key, value) => {
      map.set(key, String(value));
    },
    removeItem: (key) => {
      map.delete(key);
    },
  };
}

const throwing = () => {
  throw new Error("storage blocked");
};
const blockedStorage = {
  getItem: throwing,
  setItem: throwing,
  removeItem: throwing,
};

const draft = {
  title: "Plan",
  content: "# body",
  baseRevisionId: "a".repeat(64),
  channelId: null,
  savedAt: 1_700_000_000,
};

test("keys are scoped by community, channel and page and are unambiguous", () => {
  const key = pageDraftKey("c1", "ch", "p");
  assert.notEqual(key, pageDraftKey("c2", "ch", "p"));
  assert.notEqual(key, pageDraftKey("c1", "ch2", "p"));
  assert.notEqual(key, pageDraftKey("c1", "ch", "p2"));
  // Parts that contain the delimiter cannot collide with a different split.
  assert.notEqual(pageDraftKey("a:b", "c", "d"), pageDraftKey("a", "b:c", "d"));
  assert.notEqual(
    pageDraftKey("c1", null, null),
    pageDraftKey("c1", "ch", null),
  );
});

test("a draft round-trips", () => {
  const storage = memoryStorage();
  const key = pageDraftKey("c", "h", "d");
  assert.equal(writePageDraft(storage, key, draft), true);
  assert.deepEqual(readPageDraft(storage, key), draft);
  clearPageDraft(storage, key);
  assert.equal(readPageDraft(storage, key), null);
});

test("blocked or throwing storage degrades to no autosave and never throws", () => {
  const key = pageDraftKey("c", "h", "d");
  assert.equal(writePageDraft(blockedStorage, key, draft), false);
  assert.equal(readPageDraft(blockedStorage, key), null);
  assert.doesNotThrow(() => clearPageDraft(blockedStorage, key));
  assert.doesNotThrow(() => clearPageDraftIfSaved(blockedStorage, key, draft));
  // No storage at all (the accessor itself threw).
  assert.equal(writePageDraft(null, key, draft), false);
  assert.equal(readPageDraft(null, key), null);
});

test("malformed, foreign or wrong-version records are ignored", () => {
  const key = "k";
  for (const raw of [
    "not json",
    "null",
    "[]",
    JSON.stringify({ v: 2, title: "t", content: "c", savedAt: 1 }),
    JSON.stringify({ v: 1, title: 5, content: "c", savedAt: 1 }),
    JSON.stringify({ v: 1, title: "t", content: "c" }),
  ]) {
    assert.equal(readPageDraft(memoryStorage({ [key]: raw }), key), null, raw);
  }
});

test("an oversized draft is not stored (storage is a convenience, not a dump)", () => {
  const storage = memoryStorage();
  const huge = { ...draft, content: "x".repeat(MAX_STORED_DRAFT_CHARS + 1) };
  assert.equal(writePageDraft(storage, "k", huge), false);
  assert.equal(storage.map.size, 0);
});

test("a quota error on write is reported, not thrown", () => {
  const storage = memoryStorage();
  storage.setItem = () => {
    throw new DOMException("quota", "QuotaExceededError");
  };
  assert.equal(writePageDraft(storage, "k", draft), false);
});

test("a finished save clears only the draft it saved, never newer typing", () => {
  const storage = memoryStorage();
  const key = pageDraftKey("c", "h", "d");
  writePageDraft(storage, key, draft);

  // The user kept typing after the save started: the stored draft moved on.
  writePageDraft(storage, key, { ...draft, content: "# body, then more" });
  clearPageDraftIfSaved(storage, key, {
    title: draft.title,
    content: draft.content,
  });
  assert.equal(readPageDraft(storage, key)?.content, "# body, then more");

  // A newer draft that differs only in its title survives too.
  writePageDraft(storage, key, {
    ...draft,
    title: "Plan, renamed",
    content: "# body, then more",
  });
  clearPageDraftIfSaved(storage, key, {
    title: "Plan",
    content: "# body, then more",
  });
  assert.equal(readPageDraft(storage, key)?.title, "Plan, renamed");
  writePageDraft(storage, key, { ...draft, content: "# body, then more" });

  // Stored draft equals what was saved: cleared.
  clearPageDraftIfSaved(storage, key, {
    title: "Plan",
    content: "# body, then more",
  });
  assert.equal(readPageDraft(storage, key), null);
});
